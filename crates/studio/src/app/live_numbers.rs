//! Live numbers: the plain numbers of the playing score that the knobs
//! column moves the way a slider moves, with no new evaluation.
//!
//! A number becomes live the first time a rail moves it while the set
//! plays. The score the set plays is installed again as it is, with that
//! number as a live cell (see `TranspileOptions::live_numbers`), so edits
//! made since and not evaluated stay out of the sound. From then on a move
//! writes the number into the score and sets the cell, as a slider drag
//! does. The numbers stay live through the updates that follow, while the
//! score still holds them.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use rustel_runtime::ui_events::{LiveNumber as EngineLiveNumber, MAX_UI_LAYOUT_SLIDERS};
use rustel_transpiler::{live_number_id, live_number_range};

use super::*;

/// Whether a call reads its argument at `at` as a pattern, so a live cell
/// can stand in for a number there: an audio control's value, such as the
/// `4000` of `lpf(4000)`, or an argument of a function the engine
/// patternifies, such as `seg(4)` or `pg(0.8)`. A function that takes its
/// numbers as they are, and a function defined in the score, keep the
/// number as written.
pub(super) fn takes_a_live_number(callee: &str, control: bool, at: usize) -> bool {
    /// Each patternified function's name and how many arguments it
    /// patternifies: all but the pattern it is applied to.
    static PATTERNIFIED: LazyLock<HashMap<String, usize>> = LazyLock::new(|| {
        #[cfg(feature = "extensions")]
        let registry = rustel_ext::default_registry();
        #[cfg(not(feature = "extensions"))]
        let registry = rustel_core::register::default_registry();
        registry
            .names()
            .into_iter()
            .filter_map(|name| {
                let registration = registry.get(name)?;
                registration
                    .patternify
                    .then(|| (name.to_owned(), registration.arity.saturating_sub(1)))
            })
            .collect()
    });
    (control && at == 0) || PATTERNIFIED.get(callee).is_some_and(|count| at < *count)
}

/// Whether the score defines `name` itself, so its calls are the score's
/// own and take their numbers as they are written: a `function` of that
/// name, a `register` of it, or the name bound any other way - `const`,
/// `let`, `var`, a destructuring `{ name }`. A name the score only calls,
/// as `name(` or `.name(`, or uses as an object key, is not its own.
pub(super) fn defined_in(source: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$');
    let quote = |c: char| matches!(c, '\'' | '"' | '`');
    source.match_indices(name).any(|(at, _)| {
        let (before, after) = (&source[..at], &source[at + name.len()..]);
        if before.ends_with(word) || after.starts_with(word) {
            return false;
        }
        if before.ends_with(quote) {
            return before[..before.len() - 1].trim_end().ends_with("register(");
        }
        let declared = before.trim_end();
        if declared.ends_with("function") && declared.len() < before.len() {
            return true;
        }
        // A binding: neither a call, nor a method, nor a key, nor a word of
        // a string.
        let next = after.trim_start().chars().next();
        !declared.ends_with('.') && !after.starts_with(quote) && !matches!(next, Some('(' | ':'))
    })
}

/// A scene's live numbers, pinned to the text revision they were placed in
/// and mapped forward from it at the point of use, as [`LiveSliderSet`] is.
pub(super) struct LiveNumberSet {
    revision: Revision,
    numbers: Vec<LiveNumber>,
}

#[derive(Clone)]
struct LiveNumber {
    /// The cell's id in the last text the engine was sent with it.
    id: String,
    /// The number's literal at the set's revision.
    range: Range<usize>,
    /// The value a rail last moved it to since the score was evaluated.
    /// The running score plays it, whatever was typed over it since.
    moved: Option<f64>,
}

impl App {
    /// The scene's live numbers in its current text. A number whose
    /// literal an edit replaced or deleted is gone.
    fn live_numbers_now(&self, scene: SceneId) -> Vec<LiveNumber> {
        let (Some(set), Some(entry)) = (self.live_numbers.get(&scene), self.scenes.get(scene))
        else {
            return Vec::new();
        };
        set.numbers
            .iter()
            .filter_map(|number| {
                let range = entry
                    .editor
                    .map_range_since(set.revision, number.range.clone())?;
                Some(LiveNumber {
                    id: number.id.clone(),
                    range,
                    moved: number.moved,
                })
            })
            .collect()
    }

    /// Pin the scene's live numbers to its current text.
    fn place_live_numbers(&mut self, scene: SceneId, numbers: Vec<LiveNumber>) {
        let Some(revision) = self.scenes.get(scene).map(|entry| entry.editor.revision()) else {
            return;
        };
        self.live_numbers
            .insert(scene, LiveNumberSet { revision, numbers });
    }

    /// Tell the engine which numbers an evaluation of the scene's text at
    /// `revision` makes live, ahead of that evaluation. Only for the
    /// scene's current text, and only once the scene has had a live
    /// number: any other text keeps what the engine was told for it.
    pub(super) fn send_live_numbers(&mut self, scene: SceneId, revision: Revision, source: &str) {
        let current = self
            .scenes
            .get(scene)
            .is_some_and(|entry| entry.editor.revision() == revision);
        if !current || !self.live_numbers.contains_key(&scene) {
            return;
        }
        // The numbers this text carries take their ids and values from it.
        let numbers: Vec<LiveNumber> = self
            .live_numbers_now(scene)
            .into_iter()
            .map(|number| LiveNumber {
                id: live_number_id(number.range.clone()),
                range: number.range,
                moved: None,
            })
            .collect();
        let sent = numbers
            .iter()
            .map(|number| EngineLiveNumber {
                literal: number.range.clone(),
                value: None,
            })
            .collect();
        if !self.worker.try_set_live_numbers(Arc::from(source), sent) {
            // A full queue: the evaluation plays every number as written,
            // and the next move of a rail makes its number live again.
            self.live_numbers.remove(&scene);
            return;
        }
        self.place_live_numbers(scene, numbers);
    }

    /// Move a number of the current score to `value`, written as `text`, as
    /// a live number: its cell is set, and a number of the playing score
    /// that is not live yet becomes live. `held` is the text from the
    /// call's name to the number: the playing score must hold it as it is.
    /// False when the number cannot be moved live - the set is stopped, or
    /// another score plays, or another update is on its way - and the
    /// caller writes it as an edit.
    pub(super) fn move_live_number(
        &mut self,
        held: Range<usize>,
        literal: Range<usize>,
        value: f64,
        text: &str,
    ) -> bool {
        let scene = self.scenes.current().id;
        if !self.plays_live(scene) {
            return false;
        }
        self.follow_slider_spans();
        let live = self
            .live_numbers_now(scene)
            .iter()
            .any(|number| number.range == literal);
        if !live && !self.make_live(scene, held, literal.clone()) {
            return false;
        }
        self.write_live_number(scene, literal, value, text)
    }

    /// Whether a rail can move a number of the scene live: the set plays
    /// the scene itself, not a tape or a prebake of it.
    pub(super) fn plays_live(&self, scene: SceneId) -> bool {
        self.audible_scene == Some(scene)
            && self.is_playing()
            && !self.is_stopping()
            && self.current_replay().is_none()
            && self.current_prebake().is_none()
    }

    /// Whether a rail moves the scene's number at `literal` live: it is
    /// live already, or the playing score holds it as it is, with the text
    /// `held` that leads to it, and the layout has room for its cell.
    /// `source` is the scene's current text.
    pub(super) fn number_moves_live(
        &self,
        scene: SceneId,
        source: &str,
        held: Range<usize>,
        literal: Range<usize>,
    ) -> bool {
        self.live_numbers_now(scene)
            .iter()
            .any(|number| number.range == literal)
            || (self.sliders.len() < MAX_UI_LAYOUT_SLIDERS
                && self.playing_literal(scene, source, held, literal).is_some())
    }

    /// The number at `literal`, a range of `source`, the scene's current
    /// text, as the playing score holds it. `held`, the text from the
    /// call's name to the number, is the same text there, which the edits
    /// since moved to where it is now. A number typed or retyped since, or
    /// a call renamed since, is not in the playing score.
    fn playing_literal(
        &self,
        scene: SceneId,
        source: &str,
        held: Range<usize>,
        literal: Range<usize>,
    ) -> Option<Range<usize>> {
        let installed = self.evaluated_source.as_deref()?;
        let at = self.visual_revision?;
        let editor = &self.scenes.get(scene)?.editor;
        let text = source.get(held.clone())?;
        let offset = literal.start.checked_sub(held.start)?;
        installed
            .match_indices(text)
            .map(|(start, _)| start..start + text.len())
            .find(|range| editor.map_range_since(at, range.clone()).as_ref() == Some(&held))
            .map(|range| range.start + offset..range.start + offset + literal.len())
    }

    /// Install the playing score again with the number at `literal` - a
    /// range of the current text - as a live cell, beside the numbers that
    /// are live already. False when that cannot be done without changing
    /// what plays.
    fn make_live(&mut self, scene: SceneId, held: Range<usize>, literal: Range<usize>) -> bool {
        // An update on its way installs a newer text: this one must not
        // replace it, and the move is written as an edit for it to carry.
        // So must an update whose layout has not landed: until it lands,
        // the installed text named here is the score before it.
        if self.reinstall_on_its_way()
            || self.launch_arming.is_armed()
            || self.sliders.len() >= MAX_UI_LAYOUT_SLIDERS
        {
            return false;
        }
        let (Some(installed), Some(at)) = (self.evaluated_source.clone(), self.visual_revision)
        else {
            return false;
        };
        let Some(source) = self.scenes.get(scene).map(|entry| entry.editor.source()) else {
            return false;
        };
        let Some(new) = self.playing_literal(scene, &source, held, literal.clone()) else {
            return false;
        };
        // The numbers that are live already, by the cells the playing score
        // holds for them, each with the value its rail left it at. A number
        // with no cell there is not live in it.
        let mut live: Vec<LiveNumber> =
            self.live_numbers_now(scene)
                .into_iter()
                .filter_map(|number| {
                    let span = self.sliders.iter().find(|span| {
                        span.from < number.range.end && number.range.start < span.to
                    })?;
                    live_number_range(&span.id)?;
                    Some(LiveNumber {
                        id: span.id.clone(),
                        ..number
                    })
                })
                .collect();
        let mut sent: Vec<EngineLiveNumber> = live
            .iter()
            .filter_map(|number| {
                Some(EngineLiveNumber {
                    literal: live_number_range(&number.id)?,
                    value: number.moved,
                })
            })
            .collect();
        sent.push(EngineLiveNumber {
            literal: new.clone(),
            value: None,
        });
        if !self
            .worker
            .try_set_live_numbers(Arc::clone(&installed), sent)
        {
            return false;
        }
        live.push(LiveNumber {
            id: live_number_id(new),
            range: literal,
            moved: None,
        });
        self.place_live_numbers(scene, live);
        self.next_evaluation = LaunchRequest {
            in_place: true,
            ..LaunchRequest::default()
        };
        // Nobody changed the score, so the tape has nothing to keep.
        self.queue_evaluation_for(scene, at, installed, false);
        true
    }

    /// Write a live number's new value into the score, keep every live
    /// number placed in the text the write leaves, and hand the value to
    /// the running score.
    fn write_live_number(
        &mut self,
        scene: SceneId,
        literal: Range<usize>,
        value: f64,
        text: &str,
    ) -> bool {
        let numbers = self.live_numbers_now(scene);
        // The running score already plays the value, so this write owes no
        // auto update. One the typing owed before it is still owed.
        let update_owed = self.auto_evaluate_due;
        let Some(before) = self.control_edit(vec![(literal.clone(), text.to_owned())]) else {
            return false;
        };
        self.auto_evaluate_due = update_owed;
        // The write replaces the literal whole, so its own number is placed
        // by hand and the rest follow the write.
        let Some(editor) = self.scenes.get(scene).map(|entry| &entry.editor) else {
            return false;
        };
        let written = literal.start..literal.start + text.len();
        let numbers: Vec<LiveNumber> = numbers
            .into_iter()
            .filter_map(|number| {
                if number.range == literal {
                    return Some(LiveNumber {
                        range: written.clone(),
                        moved: Some(value),
                        ..number
                    });
                }
                Some(LiveNumber {
                    range: editor.map_range_since(before, number.range.clone())?,
                    ..number
                })
            })
            .collect();
        // Before the reinstall that made it live lands, the running score
        // has no cell for it: the value goes when the layout comes, by
        // `sync_live_numbers`.
        if self.hand_written_value(scene, before, literal, text.len(), value, false)
            || self.reinstall_on_its_way()
        {
            self.place_live_numbers(scene, numbers);
            return true;
        }
        // The reinstall landed with no cell for it: the number stays as
        // written there. The move is an edit, as any other rail's.
        let numbers = numbers
            .into_iter()
            .filter(|number| number.range != written)
            .collect();
        self.place_live_numbers(scene, numbers);
        self.auto_evaluate_due = false;
        self.auto_evaluate();
        true
    }

    /// An install of this app is on its way, or installed with its layout
    /// still to land.
    fn reinstall_on_its_way(&self) -> bool {
        self.pending_evaluation.is_some() || !self.inflight.is_empty() || self.layout_due.is_some()
    }

    /// Where the audible spans lost a live number to a write - its literal
    /// replaced after the text it was evaluated in - the number's own place
    /// stands in, found by its id.
    pub(super) fn live_number_spans(&self) -> Vec<SliderSpan> {
        let Some(scene) = self.audible_scene else {
            return Vec::new();
        };
        self.live_numbers_now(scene)
            .into_iter()
            .map(|number| SliderSpan {
                id: number.id,
                from: number.range.start,
                to: number.range.end,
                min: f64::MIN,
                max: f64::MAX,
                step: f64::MIN_POSITIVE,
                value: 0.0,
            })
            .collect()
    }

    /// Once a layout lands, every live number the running score holds
    /// plays the value a rail last moved it to: a move made before its
    /// cell existed, or before a reinstall reset it, is handed over now.
    /// A number typed over and not evaluated stays out of the sound.
    pub(super) fn sync_live_numbers(&mut self) {
        let Some(scene) = self.audible_scene else {
            return;
        };
        let owed: Vec<(String, f64)> = self
            .live_numbers_now(scene)
            .into_iter()
            .filter_map(|number| {
                let span = self.sliders.iter().find(|span| span.id == number.id)?;
                let moved = number.moved?;
                (moved != span.value).then(|| (span.id.clone(), moved))
            })
            .collect();
        for (id, value) in owed {
            if let Some(span) = self.sliders.iter_mut().find(|span| span.id == id) {
                span.value = value;
            }
            self.owe_slider(id, value, false);
        }
        self.flush_pending_sliders();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::support::*;
    use crate::editor::{ByteOffset, MemoryClipboard, Selection};
    use ratatui::layout::Rect;

    /// The score the set plays, installed, with an edit typed since that
    /// nobody evaluated: `seg(4)` became `seg(16)`. The caret is on the
    /// cutoff and the knobs column is open on it.
    fn playing_with_an_unevaluated_edit(
        directory: &std::path::Path,
        source: &'static str,
        number: &str,
    ) -> App {
        let (mut app, scene, _) = build_app(directory, source, None);
        app.evaluation_revisions.remember(
            source_revision(source),
            app.editor().revision(),
            source.into(),
            scene,
            None,
        );
        let layout = rustel_runtime::ui_events::visual_layout(source, 1).expect("layout");
        app.handle_control(StudioControlEvent::Layout(layout));
        app.snapshot = Some(sounding(true, false));
        let seg = source.find("seg(4").expect("the seg") + 4;
        app.editor_mut()
            .dispatch(
                Command::ReplaceRange {
                    from: ByteOffset(seg),
                    to: ByteOffset(seg + 1),
                    text: "16".into(),
                },
                HistoryMoment(1),
                &mut MemoryClipboard::default(),
            )
            .expect("the unevaluated edit");
        let caret = app.editor().source().find(number).expect("the number");
        app.editor_mut()
            .set_selection(Selection::caret(ByteOffset(caret)))
            .expect("the caret on the number");
        app.open_knobs_panel(true);
        app
    }

    /// Live numbers that start at their literals.
    fn as_written(ranges: &[Range<usize>]) -> Vec<EngineLiveNumber> {
        ranges
            .iter()
            .map(|literal| EngineLiveNumber {
                literal: literal.clone(),
                value: None,
            })
            .collect()
    }

    const CUTOFF: &str = "$: s(\"sine\").seg(4).lpf(4000)\n";

    /// The texts sent to the engine since the test began, newest last.
    fn sent(app: &App) -> Vec<Arc<str>> {
        let mut sent: Vec<_> = app.inflight.values().collect();
        sent.sort_by_key(|pending| pending.request_id);
        sent.into_iter()
            .chain(app.pending_evaluation.as_ref())
            .map(|pending| Arc::clone(&pending.source))
            .collect()
    }

    /// The reported case: a rail moved after an edit nobody evaluated. The
    /// first move installs the playing score again with the cutoff live,
    /// and the edit stays out of it. Every move after that sets the cell,
    /// with no evaluation at all.
    #[test]
    fn a_rail_moves_the_playing_score_and_leaves_an_unevaluated_edit_alone() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installed = CUTOFF;
        let mut app = playing_with_an_unevaluated_edit(directory.path(), installed, "4000");
        app.ui_settings.auto_evaluate = true;
        let rail = Rect::new(0, 0, 24, 1);

        app.drag_knob(0, rail, 4);
        let buffer = app.editor().source();
        assert!(buffer.contains("seg(16)"), "the edit stays in the score");
        assert!(
            !buffer.contains("4000"),
            "the rail wrote the cutoff: {buffer}"
        );
        assert_eq!(
            sent(&app)
                .iter()
                .map(|text| text.as_ref())
                .collect::<Vec<_>>(),
            [installed],
            "the playing score is installed again, without the edit"
        );

        // The engine answers with the score's layout, the cutoff a cell of
        // it, and the value the rail wrote is handed to that cell.
        let cutoff = installed.find("4000").expect("the cutoff");
        let cell = cutoff..cutoff + "4000".len();
        let layout = rustel_runtime::ui_events::visual_layout_with_live_numbers(
            installed,
            2,
            &as_written(std::slice::from_ref(&cell)),
        )
        .expect("layout");
        app.handle_control(StudioControlEvent::Layout(layout));
        let written = |app: &App| -> f64 {
            let source = app.editor().source();
            let start = source.find("lpf(").expect("the call") + 4;
            let end = start + source[start..].find(')').expect("the paren");
            source[start..end].parse().expect("a number")
        };
        let id = live_number_id(cell);
        let span = |app: &App| {
            app.sliders
                .iter()
                .find(|span| span.id == id)
                .map(|span| span.value)
        };
        assert_eq!(
            span(&app),
            Some(written(&app)),
            "the cell plays the written value"
        );

        app.drag_knob(0, rail, 20);
        assert_eq!(
            sent(&app).len(),
            1,
            "a move of a live number evaluates nothing"
        );
        assert!(
            !app.auto_evaluate_due,
            "nor does the typing pause after it, with auto evaluate on"
        );
        assert_eq!(span(&app), Some(written(&app)), "the cell follows the rail");
        assert!(app.editor().source().contains("seg(16)"));
    }

    /// A live number typed over and not evaluated stays out of the sound
    /// when another rail makes its number live: the reinstall hands each
    /// cell the value its rail last moved it to, not the typed text.
    #[test]
    fn a_number_typed_over_stays_out_when_another_goes_live() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installed = "$: s(\"sine\").seg(4).lpf(4000).gain(0.5)\n";
        let mut app = playing_with_an_unevaluated_edit(directory.path(), installed, "4000");
        let rail = Rect::new(0, 0, 24, 1);
        app.drag_knob(0, rail, 4);
        let at = |needle: &str| {
            let start = installed.find(needle).expect("the number");
            start..start + needle.len()
        };
        let (cutoff, gain) = (at("4000"), at("0.5"));
        let layout = |generation, cells: &[Range<usize>]| {
            rustel_runtime::ui_events::visual_layout_with_live_numbers(
                installed,
                generation,
                &as_written(cells),
            )
            .expect("layout")
        };
        app.handle_control(StudioControlEvent::Layout(layout(
            2,
            std::slice::from_ref(&cutoff),
        )));
        let id = live_number_id(cutoff.clone());
        let cell = |app: &App| {
            app.sliders
                .iter()
                .find(|span| span.id == id)
                .map(|span| span.value)
        };
        let moved = cell(&app).expect("the cutoff is live");

        // A key deletes a digit inside the written cutoff.
        let source = app.editor().source();
        let start = source.find("lpf(").expect("the call") + 4;
        app.editor_mut()
            .dispatch(
                Command::ReplaceRange {
                    from: ByteOffset(start + 1),
                    to: ByteOffset(start + 2),
                    text: String::new(),
                },
                HistoryMoment(2),
                &mut MemoryClipboard::default(),
            )
            .expect("the typed edit");
        let caret = app.editor().source().find("0.5").expect("the gain");
        app.editor_mut()
            .set_selection(Selection::caret(ByteOffset(caret)))
            .expect("the caret on the gain");
        app.open_knobs_panel(true);
        app.drag_knob(0, rail, 20);
        assert!(
            !app.editor().source().contains("0.5"),
            "the gain rail wrote"
        );
        app.handle_control(StudioControlEvent::Layout(layout(3, &[cutoff, gain])));
        assert_eq!(cell(&app), Some(moved), "the cutoff plays its rail's value");
    }

    /// A function the engine patternifies takes a live number as a control
    /// does: `fast(2)` moves live, and `pg(0.8)` with the extensions. A
    /// function that takes its number as it is keeps the write in the score.
    #[test]
    fn a_patternified_function_moves_live_and_a_plain_one_does_not() {
        for (source, number, live) in [
            #[cfg(feature = "extensions")]
            ("$: s(\"sine\").seg(4).pg(0.8)\n", "0.8", true),
            ("$: s(\"sine\").seg(4).fast(2)\n", "2", true),
            ("$: s(\"sine\").seg(4).ply(3)\n", "3", true),
            ("$: s(\"sine\").seg(4).midi(\"out\", 3)\n", "3", false),
        ] {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = playing_with_an_unevaluated_edit(directory.path(), source, number);
            app.refresh_knobs(Rect::default());
            let at = app
                .knobs_rows
                .as_ref()
                .and_then(|rows| rows.knobs.iter().position(|knob| knob.state.is_some()));
            let Some(at) = at else {
                assert!(!live, "{source} has a rail");
                continue;
            };
            app.drag_knob(at, Rect::new(0, 0, 24, 1), 20);
            assert_ne!(app.editor().source(), source, "{source}: the rail wrote");
            assert_eq!(!sent(&app).is_empty(), live, "{source}");
        }
    }

    #[test]
    fn which_calls_take_a_live_number() {
        assert!(takes_a_live_number("lpf", true, 0));
        assert!(!takes_a_live_number("lpf", true, 1));
        assert!(takes_a_live_number("fast", false, 0));
        #[cfg(feature = "extensions")]
        assert!(takes_a_live_number("pg", false, 0), "an extension function");
        assert!(takes_a_live_number("euclid", false, 1));
        assert!(
            !takes_a_live_number("euclid", false, 2),
            "the pattern itself"
        );
        assert!(!takes_a_live_number("midi", false, 0));
        assert!(!takes_a_live_number("myOwnFunction", false, 0));
    }

    /// While the set plays, a rail whose number the playing score does
    /// not hold - typed since, not evaluated - waits for the update and
    /// says so. The cutoff the score plays moves live and does not wait.
    #[test]
    fn a_rail_the_playing_score_lacks_waits_for_the_update() {
        let directory = tempfile::tempdir().expect("tempdir");
        let waits = |number: &str| {
            let mut app = playing_with_an_unevaluated_edit(directory.path(), CUTOFF, number);
            app.ui_settings.auto_evaluate = false;
            app.refresh_knobs(Rect::default());
            app.knobs_rows.as_ref().expect("the rails").knobs[0].waits
        };
        assert!(waits("16"), "the seg typed since waits");
        assert!(!waits("4000"), "the playing cutoff moves live");
    }

    /// A rail moves live only what the playing score holds: a call renamed
    /// since, or an update whose layout has not landed, keeps the move in
    /// the score, and nothing is installed again.
    #[test]
    fn a_renamed_call_or_a_layout_still_due_keeps_the_write_in_the_score() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = playing_with_an_unevaluated_edit(directory.path(), CUTOFF, "4000");
        let call = app.editor().source().find("lpf").expect("the call");
        app.editor_mut()
            .dispatch(
                Command::ReplaceRange {
                    from: ByteOffset(call),
                    to: ByteOffset(call + 1),
                    text: "h".into(),
                },
                HistoryMoment(2),
                &mut MemoryClipboard::default(),
            )
            .expect("the rename");
        app.open_knobs_panel(true);
        app.drag_knob(0, Rect::new(0, 0, 24, 1), 4);
        assert!(!app.editor().source().contains("4000"), "the rail wrote");
        assert!(sent(&app).is_empty(), "hpf is not the playing lpf");

        let mut app = playing_with_an_unevaluated_edit(directory.path(), CUTOFF, "4000");
        app.layout_due = Some(source_revision("a newer score"));
        app.drag_knob(0, Rect::new(0, 0, 24, 1), 4);
        assert!(!app.editor().source().contains("4000"), "the rail wrote");
        assert!(sent(&app).is_empty(), "the newer score is not replaced");
    }

    /// A reinstall that lands with no cell for the number leaves it as
    /// written: the next move is an edit, which auto evaluate carries.
    #[test]
    fn a_number_the_reinstall_left_as_written_moves_as_an_edit() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installed = CUTOFF;
        let mut app = playing_with_an_unevaluated_edit(directory.path(), installed, "4000");
        app.ui_settings.auto_evaluate = true;
        let rail = Rect::new(0, 0, 24, 1);
        app.drag_knob(0, rail, 4);
        // The reinstall was sent and answered, and its layout has no cell.
        app.pending_evaluation = None;
        app.inflight.clear();
        let layout = rustel_runtime::ui_events::visual_layout_with_live_numbers(installed, 2, &[])
            .expect("layout");
        app.handle_control(StudioControlEvent::Layout(layout));
        app.drag_knob(0, rail, 20);
        assert!(app.live_numbers_now(app.scenes.current().id).is_empty());
        let sent = sent(&app);
        assert_eq!(sent.len(), 1, "the edit is evaluated");
        assert!(sent[0].contains("seg(16)"), "as any edit is");
    }

    #[test]
    fn a_name_the_score_defines_is_its_own() {
        assert!(defined_in("const seg = (n, p) => p; seg(4, x)", "seg"));
        assert!(defined_in("function fast(n) {}", "fast"));
        assert!(defined_in("register('pg', (x, pat) => pat)", "pg"));
        assert!(defined_in("const { fast } = lib; fast(2, x)", "fast"));
        assert!(!defined_in("$: s(\"bd\").seg(4)", "seg"));
        assert!(!defined_in("const segment = 4; s(\"bd\").seg(4)", "seg"));
        assert!(!defined_in(
            "$: s(\"bd\").set({ lpf: 800 }).lpf(400)",
            "lpf"
        ));
        assert!(!defined_in("$: s(\"fast bd\").fast(2)", "fast"));
    }

    /// A stopped set has no score to move: the rail writes the score, and
    /// nothing is installed.
    #[test]
    fn a_stopped_set_keeps_the_write_in_the_score() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = playing_with_an_unevaluated_edit(directory.path(), CUTOFF, "4000");
        app.snapshot = Some(sounding(false, false));
        app.drag_knob(0, Rect::new(0, 0, 24, 1), 4);
        assert!(!app.editor().source().contains("4000"));
        assert!(sent(&app).is_empty());
        assert!(app.live_numbers.is_empty());
    }
}
