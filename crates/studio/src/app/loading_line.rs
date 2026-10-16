//! The header's loading line: a thin line that grows along the bottom of
//! the header row while the sounds and the plugins the playing or the
//! waiting score needs come in, shown only once a load has lasted, and after
//! a while its words in the tempo's place.

use super::*;

/// A load shorter than this never shows, so a quick load does not flash.
pub(super) const LOADING_LINE_AFTER: Duration = Duration::from_millis(250);

/// After this long the line also says what it is loading.
pub(super) const LOADING_LABEL_AFTER: Duration = Duration::from_millis(1_500);

impl App {
    /// Follow the engine's loading cue: when the current load began, and a
    /// repaint when the line or its words become due or the load ends.
    pub(super) fn follow_loading_line(&mut self, now: Instant) {
        let loading = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.loading.is_some());
        match (loading, self.loading_since) {
            (true, None) => self.loading_since = Some(now),
            (false, Some(_)) => {
                self.loading_since = None;
                self.dirty_frame = true;
            }
            _ => {}
        }
        let due = self.loading_since.map_or((false, false), |since| {
            let lasted = now.saturating_duration_since(since);
            (lasted >= LOADING_LINE_AFTER, lasted >= LOADING_LABEL_AFTER)
        });
        if due != self.loading_due {
            self.loading_due = due;
            self.dirty_frame = true;
        }
    }

    /// The line as the header draws it at `now`, if it is due. A terminal
    /// that reports 24-bit colour draws a coloured underline (SGR 58, which
    /// the same terminals implement); one that does not gets a faint tint,
    /// since an underline colour it does not understand is at best ignored.
    pub(super) fn loading_line(&self, now: Instant) -> Option<view::LoadingLine> {
        let cue = self.snapshot.as_ref()?.loading.as_ref()?;
        let lasted = now.saturating_duration_since(self.loading_since?);
        if lasted < LOADING_LINE_AFTER || cue.total == 0 {
            return None;
        }
        let label = (lasted >= LOADING_LABEL_AFTER).then(|| {
            let words = if cue.waiting {
                "waiting for"
            } else {
                "loading"
            };
            // A plugin wait is named as a sound wait is.
            let what = if cue.plugins == 0 {
                "sounds"
            } else if cue.plugins < cue.total - cue.settled {
                "sounds and plugins"
            } else {
                "plugins"
            };
            let percent = cue.settled * 100 / cue.total;
            let mut label = format!(
                "{words} {what} · {}/{} · {percent}%",
                cue.settled, cue.total
            );
            if let Some(name) = &cue.loading {
                label.push_str(" · ");
                label.push_str(name);
            }
            label
        });
        Some(view::LoadingLine {
            reach: cue.settled as f64 / cue.total as f64,
            label,
            underline: self.features.truecolor,
        })
    }
}

#[cfg(test)]
mod tests {
    mod loading_line {
        //! Tests for `app/loading_line.rs`: the header's loading line and its
        //! words, as the studio draws them.

        use crate::app::loading_line::{LOADING_LABEL_AFTER, LOADING_LINE_AFTER};
        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::engine::{LoadingCue, RecordingInfo};

        fn cue(settled: usize, total: usize, waiting: bool) -> LoadingCue {
            LoadingCue {
                settled,
                total,
                loading: Some("mlkr-grsl".into()),
                plugins: 0,
                waiting,
            }
        }

        /// The studio playing, its engine reporting `loading`, the load having
        /// lasted `lasted`.
        fn loading(app: &mut App, loading: Option<LoadingCue>, lasted: Duration) {
            app.snapshot = Some(StudioSnapshot {
                playing: true,
                cps: 0.5,
                loading,
                ..Default::default()
            });
            app.loading_since = None;
            let now = Instant::now();
            app.follow_loading_line(now - lasted);
        }

        fn frame(app: &mut App, width: u16) -> ratatui::buffer::Buffer {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24))
                    .expect("terminal");
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                .expect("draw");
            terminal.backend().buffer().clone()
        }

        fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        }

        fn header_y(buffer: &ratatui::buffer::Buffer) -> u16 {
            (0..buffer.area.height)
                .find(|y| row_text(buffer, *y).contains("PLAYING"))
                .expect("the header is drawn")
        }

        fn underlined(
            buffer: &ratatui::buffer::Buffer,
            y: u16,
            colour: ratatui::style::Color,
        ) -> usize {
            (0..buffer.area.width)
                .filter(|x| {
                    let cell = &buffer[(*x, y)];
                    cell.modifier.contains(ratatui::style::Modifier::UNDERLINED)
                        && cell.underline_color == colour
                })
                .count()
        }

        /// A quick load never shows; a load that lasts shows the line, and one
        /// that lasts longer says what it waits for. Each step repaints.
        #[test]
        fn the_line_shows_after_the_debounce_and_its_words_after_that() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            loading(&mut app, Some(cue(3, 12, true)), Duration::ZERO);
            let since = app.loading_since.expect("the load began");

            assert_eq!(app.loading_line(since + LOADING_LINE_AFTER / 2), None);
            let line = app
                .loading_line(since + LOADING_LINE_AFTER)
                .expect("the line after the debounce");
            assert_eq!(line.reach, 0.25);
            assert_eq!(line.label, None, "no words yet");

            app.dirty_frame = false;
            app.follow_loading_line(since + LOADING_LINE_AFTER);
            assert!(app.dirty_frame, "the line becoming due repaints");
            let line = app
                .loading_line(since + LOADING_LABEL_AFTER)
                .expect("the line");
            assert_eq!(
                line.label.as_deref(),
                Some("waiting for sounds · 3/12 · 25% · mlkr-grsl")
            );

            loading(&mut app, Some(cue(3, 12, false)), LOADING_LABEL_AFTER);
            let label = app.loading_line(Instant::now()).and_then(|line| line.label);
            assert_eq!(
                label.as_deref(),
                Some("loading sounds · 3/12 · 25% · mlkr-grsl"),
                "nothing is held"
            );

            // A plugin wait has its own word, alone or beside the sounds.
            for (plugins, words) in [(9, "plugins"), (2, "sounds and plugins")] {
                let waiting = LoadingCue {
                    plugins,
                    ..cue(3, 12, true)
                };
                loading(&mut app, Some(waiting), LOADING_LABEL_AFTER);
                let label = app.loading_line(Instant::now()).and_then(|line| line.label);
                assert_eq!(
                    label,
                    Some(format!("waiting for {words} · 3/12 · 25% · mlkr-grsl"))
                );
            }
        }

        /// The line grows with the load along the header row, adds no row and
        /// moves nothing else, and leaves no trace once the load is over.
        #[test]
        fn the_line_grows_adds_no_row_and_clears_when_done() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.features.truecolor = true;
            let colour = view::loading_line_colour(&app.theme);
            loading(&mut app, None, Duration::ZERO);
            let before = frame(&mut app, 90);
            let y = header_y(&before);

            let mut reaches = Vec::new();
            for settled in [1, 3] {
                loading(&mut app, Some(cue(settled, 4, true)), LOADING_LINE_AFTER);
                let during = frame(&mut app, 90);
                assert_eq!(header_y(&during), y, "no row is added");
                for row in (0..24).filter(|row| *row != y) {
                    assert_eq!(
                        row_text(&during, row),
                        row_text(&before, row),
                        "row {row} moved"
                    );
                }
                reaches.push(underlined(&during, y, colour));
            }
            assert_eq!(reaches, [23, 68]);

            loading(&mut app, None, Duration::ZERO);
            let after = frame(&mut app, 90);
            assert_eq!(underlined(&after, y, colour), 0);
            for x in 0..90 {
                assert_eq!(after[(x, y)], before[(x, y)], "cell {x} kept a trace");
            }
        }

        /// A terminal that does not report 24-bit colour gets a faint tint rather
        /// than an underline colour it may not understand.
        #[test]
        fn the_line_falls_back_to_a_tint() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.features.truecolor = false;
            loading(&mut app, Some(cue(2, 4, true)), LOADING_LINE_AFTER);
            let buffer = frame(&mut app, 90);
            let y = header_y(&buffer);
            assert_eq!(
                underlined(&buffer, y, view::loading_line_colour(&app.theme)),
                0
            );
            let tint = crate::theme::mix(app.theme.surface, app.theme.accent, 0.2);
            assert_eq!((0..90).filter(|x| buffer[(*x, y)].bg == tint).count(), 45);
        }

        /// On a narrow header the words stop short of the scene's name, ending in
        /// an ellipsis: the take's clock, the readiness chip's words at its click
        /// target and the scene name stay where they are.
        #[test]
        fn the_words_fit_a_narrow_header_beside_the_take_the_chip_and_the_scene() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            app.readiness.insert(
                scene,
                Readiness {
                    ready: 1,
                    ..Readiness::default()
                },
            );
            loading(&mut app, Some(cue(3, 12, true)), LOADING_LABEL_AFTER);
            if let Some(snapshot) = app.snapshot.as_mut() {
                snapshot.recording = Some(RecordingInfo {
                    path: directory.path().join("take.wav"),
                    seconds: 5.0,
                    bytes: 0,
                    dropped_seconds: 0.0,
                    error: None,
                });
            }
            let buffer = frame(&mut app, 80);
            let y = header_y(&buffer);
            let row = row_text(&buffer, y);
            assert!(row.contains("REC 0:05"), "{row}");
            assert!(row.trim_end().ends_with("first"), "{row}");
            let words = row.find("waiting for").expect("the words are drawn");
            assert!(words > row.find("REC").expect("the take"), "{row}");
            let label: String = row[words..].chars().take_while(|c| *c != '…').collect();
            assert!(row[words..].contains('…'), "shortened: {row}");
            assert!(
                !label.contains("first"),
                "the words stop short of the scene: {row}"
            );

            let word = view::TransportWord {
                stopping: app.is_stopping(),
                playing: app.is_playing(),
            };
            let chip = view::header_go_chip(Rect::new(0, y, 80, 1), word, app.go_state());
            let at_chip: String = (chip.x..chip.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            assert_eq!(
                at_chip, "✓ ready",
                "the chip's words are at its click target"
            );
        }

        /// An update held for its sounds is not evaluating: the header keeps its
        /// transport word, and the loading line says what is happening.
        #[test]
        fn an_update_held_for_its_sounds_is_not_evaluating() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            app.inflight.insert(
                7,
                PendingEvaluation {
                    request_id: 7,
                    scene,
                    editor_revision: app.editor().revision(),
                    source: Arc::from("$: s(\"kick\")"),
                    launch: Launch::Now,
                    rewind: false,
                    record: true,
                },
            );
            loading(&mut app, Some(cue(0, 1, true)), Duration::ZERO);
            assert!(!app.is_evaluating());
            loading(&mut app, Some(cue(0, 1, false)), Duration::ZERO);
            assert!(
                app.is_evaluating(),
                "a load that holds nothing is not a wait"
            );
        }

        /// While a job runs, the word keeps saying what the transport is
        /// doing - an evaluation is a job, not a transport state - and the
        /// logo dot lights for it: brief work is seen without a word ever
        /// changing.
        #[test]
        fn while_evaluating_the_word_stays_and_the_dot_lights() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let in_flight = |app: &mut App, scene: SceneId| {
                app.inflight.insert(
                    7,
                    PendingEvaluation {
                        request_id: 7,
                        scene,
                        editor_revision: app.editor().revision(),
                        source: Arc::from("$: s(\"kick\")"),
                        launch: Launch::Now,
                        rewind: false,
                        record: true,
                    },
                );
                app.evaluating_since = Some(Instant::now());
            };

            // Playing, an update in flight.
            app.snapshot = Some(sounding(true, false));
            in_flight(&mut app, scene);
            assert!(app.is_evaluating());
            let buffer = frame(&mut app, 80);
            let y = header_y(&buffer);
            let row = row_text(&buffer, y);
            assert!(row.contains("PLAYING"), "{row}");
            assert!(!row.contains("EVALUATING"), "{row}");
            assert_eq!(
                buffer[(1, y)].symbol(),
                crate::terminal::symbol("●"),
                "the dot lights for the work"
            );

            // The answer lands: the beat pulse returns to the same cell,
            // whatever the cycle says it looks like.
            app.inflight.clear();
            app.evaluating_since = None;
            let buffer = frame(&mut app, 80);
            let y = header_y(&buffer);
            let beat = view::beat_pulse(
                true,
                app.snapshot.as_ref().map(|snapshot| snapshot.cycle),
                &app.theme,
            );
            assert_eq!(buffer[(1, y)].symbol(), beat.0, "the beat returns");
            assert_eq!(buffer[(1, y)].fg, beat.1, "in the beat's colour");

            // Stopped, the first update in flight: the word keeps saying
            // STOPPED rather than swapping to a word too brief to read.
            app.snapshot = Some(sounding(false, false));
            in_flight(&mut app, scene);
            let buffer = frame(&mut app, 80);
            let y = (0..buffer.area.height)
                .find(|y| row_text(&buffer, *y).contains("STOPPED"))
                .expect("the header is drawn");
            let row = row_text(&buffer, y);
            assert!(!row.contains("EVALUATING"), "{row}");
            assert_eq!(
                buffer[(1, y)].symbol(),
                crate::terminal::symbol("●"),
                "the dot lights while stopped too"
            );
        }
    }
}
