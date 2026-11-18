//! Fine drags. A held Shift or Alt, or the fine lock, makes a rail drag
//! relative to where the pointer took hold, at a tenth of the speed. Every
//! rail the pointer drags reads its place through here: the score's inline
//! sliders, the expanded slider, the desk's score faders and the knobs
//! column.
//!
//! A drag starts absolute: the knob jumps under the pointer. Once fine is on,
//! the drag holds a grip and stays relative until the release, so letting go
//! of the key never jumps the value back under the pointer.
use super::*;

/// How far the value moves, as a part of the pointer's move, in a fine drag.
const FINE_GAIN: f64 = 0.1;

/// The fine state of the pointer: the keys the last report carried, the
/// lock, and the grip of a relative drag.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FineDrag {
    /// The last mouse report held Shift or Alt.
    held: bool,
    /// The fine lock, for terminals that keep Shift and Alt drags for
    /// their own selection.
    pub(super) lock: bool,
    grip: Option<Grip>,
}

/// Where a relative drag took hold: the pointer and the value as ratios
/// of the rail, and the gain it moves at.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Grip {
    pointer: f64,
    value: f64,
    gain: f64,
}

impl FineDrag {
    /// Read the keys of one mouse report.
    pub(super) fn report(&mut self, modifiers: KeyModifiers) {
        self.held = modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
    }

    /// A new gesture: the old grip is gone.
    pub(super) fn release(&mut self) {
        self.grip = None;
    }

    fn fine(&self) -> bool {
        self.held || self.lock
    }

    /// The drag moves slower than the pointer.
    pub(super) fn active(&self) -> bool {
        self.grip.is_some_and(|grip| grip.gain < 1.0)
    }

    /// The drag follows a grip, not the pointer's place.
    pub(super) fn relative(&self) -> bool {
        self.grip.is_some() || self.fine()
    }

    /// The rail ratio a pointer at `pointer` sets, and the gain it moved
    /// at. `value` is the rail's ratio now, read only when a grip starts.
    fn follow(&mut self, pointer: f64, value: f64) -> (f64, f64) {
        let fine = self.fine();
        let gain = if fine { FINE_GAIN } else { 1.0 };
        let grip = match self.grip {
            None if !fine => return (pointer, 1.0),
            Some(grip) if grip.gain == gain => grip,
            _ => Grip {
                pointer,
                value: value.clamp(0.0, 1.0),
                gain,
            },
        };
        let ratio = grip.value + (pointer - grip.pointer) * gain;
        let held = ratio.clamp(0.0, 1.0);
        // Past an end the grip moves with the pointer, so a move back
        // turns the value at once.
        self.grip = Some(if held == ratio {
            grip
        } else {
            Grip {
                pointer,
                value: held,
                gain,
            }
        });
        (held, gain)
    }
}

impl App {
    /// The ratio along `track` that a drag report at `x` sets, and the
    /// distance between two reported places as a ratio, both scaled for a
    /// fine drag. `value` is the rail's ratio now.
    pub(super) fn rail_drag(&mut self, track: SliderTrack, x: u16, value: f64) -> (f64, f64) {
        let cell_width = self
            .features
            .pixel_mouse
            .then_some(self.features.cell_pixels)
            .flatten()
            .map(|(width, _)| width);
        let pointer = track.ratio(x, self.pointer_subcell, cell_width);
        let (ratio, gain) = self.fine_drag.follow(pointer, value);
        (ratio, track.resolution(cell_width) * gain)
    }

    /// The words a rail's status ends with: how to drag fine, or that the
    /// drag is fine now.
    pub(super) fn fine_drag_hint(&self) -> String {
        if self.fine_drag.active() {
            " · fine".to_owned()
        } else if self.fine_drag.lock {
            " · fine lock".to_owned()
        } else {
            " · Shift/Alt drag: fine".to_owned()
        }
    }

    /// Turn the fine lock on or off.
    pub(super) fn toggle_fine_lock(&mut self) {
        self.fine_drag.lock = !self.fine_drag.lock;
        self.status = if self.fine_drag.lock {
            "fine drag on: every slider drag moves a tenth as far".to_owned()
        } else {
            "fine drag off".to_owned()
        };
        self.dirty_frame = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fine(held: bool) -> FineDrag {
        let mut drag = FineDrag::default();
        drag.report(if held {
            KeyModifiers::ALT
        } else {
            KeyModifiers::NONE
        });
        drag
    }

    #[test]
    fn a_plain_drag_follows_the_pointer() {
        let mut drag = fine(false);
        assert_eq!(drag.follow(0.8, 0.2), (0.8, 1.0));
        assert!(!drag.relative());
    }

    #[test]
    fn a_fine_drag_starts_where_the_value_is_and_moves_a_tenth() {
        let mut drag = fine(true);
        assert_eq!(drag.follow(0.9, 0.3), (0.3, FINE_GAIN));
        let (ratio, _) = drag.follow(0.4, 0.0);
        assert!((ratio - 0.25).abs() < 1e-12, "{ratio}");
        assert!(drag.active());
    }

    #[test]
    fn letting_go_of_the_key_keeps_the_value_and_moves_at_full_speed() {
        let mut drag = fine(true);
        drag.follow(0.5, 0.5);
        drag.follow(0.7, 0.0);
        drag.report(KeyModifiers::NONE);
        let (ratio, gain) = drag.follow(0.7, 0.52);
        assert!((ratio - 0.52).abs() < 1e-12, "no jump: {ratio}");
        assert_eq!(gain, 1.0);
        let (ratio, _) = drag.follow(0.8, 0.0);
        assert!((ratio - 0.62).abs() < 1e-12, "{ratio}");
        assert!(!drag.active() && drag.relative());
    }

    #[test]
    fn past_an_end_a_move_back_turns_at_once() {
        let mut drag = fine(true);
        drag.follow(0.0, 0.95);
        assert_eq!(drag.follow(1.0, 0.0).0, 1.0);
        let (ratio, _) = drag.follow(0.9, 0.0);
        assert!((ratio - 0.99).abs() < 1e-12, "{ratio}");
    }

    /// `f` in the knobs column locks fine drags on: a rail dragged across
    /// most of its width moves a tenth as far, from where it was.
    #[test]
    fn f_in_the_knobs_column_locks_a_fine_drag() {
        use crate::app::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        let score = "$: s(\"bd\").gain(0.5)\n";
        app.scenes.current_mut().editor = Editor::new(score).expect("score");
        let caret = score.find("0.5").expect("the gain");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(caret)));
        app.open_knobs_panel(true);
        press(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(app.fine_drag.lock);
        let rail = Rect::new(0, 0, 24, 1);
        app.drag_knob(0, rail, 0);
        app.drag_knob(0, rail, 20);
        let source = app.editor().source();
        let start = source.find("gain(").expect("the call") + 5;
        let end = start + source[start..].find(')').expect("the paren");
        let gain: f64 = source[start..end].parse().expect("a number");
        // A plain drag there sets the gain near the top of its rail.
        assert!((0.55..0.75).contains(&gain), "a tenth of the move: {gain}");
        press(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(!app.fine_drag.lock);
    }

    #[test]
    fn the_lock_makes_a_drag_fine_without_a_key() {
        let mut drag = fine(false);
        drag.lock = true;
        assert_eq!(drag.follow(0.9, 0.3).1, FINE_GAIN);
        drag.release();
        assert!(!drag.active());
    }
}
