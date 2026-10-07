//! The end-to-end harness: a real studio `App` a test can drive.
//!
//! [`Studio`] wraps the private [`App`] exactly the way the binary does -
//! same worker, same editor, same event handling - and turns the things the
//! terminal provides into methods: keys, pastes, mouse, resize, and the
//! frame itself, rendered into a ratatui test backend through the same
//! [`App::paint_into`] the real terminal takes. What a test sees is what a
//! musician would: screens, focus, status text, errors - never the engine's
//! internals.
//!
//! Gated behind the `harness` feature, which is not in `default`: none of
//! this is reachable from the product, and the product must not depend on it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::{Terminal, backend::TestBackend};

use super::{App, Focus, StudioOptions};
use crate::editor::KeyboardCapabilities;
use crate::engine::StudioConfig;
use crate::prebake;
use crate::scenes::SceneSet;
use crate::terminal::TerminalFeatures;
use crate::theme::Theme;
use crate::view::PanelKind;
use crate::worker::StudioWorker;
use rustel_runtime::RuntimeError;

/// Everything a test may tune, in one struct so a new knob is a compile
/// error at every call site rather than a silent default.
///
/// The defaults are the hermetic set: the `rustel-dark` theme by name, a
/// `legacy()` keyboard, and a terminal identity that claims nothing - no
/// kitty graphics, no sync output, no cell size - which is what pins the
/// graphics tier to cells no matter what the runner's real terminal reports.
pub struct HarnessOptions {
    /// The set directory: a folder of `.strudel` scores. Created if missing;
    /// an empty one is seeded with the same starter score the CLI writes.
    pub directory: std::path::PathBuf,
    /// Built-in theme name, user theme name, or a path; `None` falls through
    /// `$RUSTEL_THEME` to the default exactly as the CLI does - set the
    /// variable (or pass a name) to keep a themed runner out of the result.
    pub theme: Option<String>,
    /// Keyboard capability set. `legacy()` by default; `enhanced()` is
    /// exercised deliberately, because it changes which chords exist and
    /// what the footer spells.
    pub capabilities: KeyboardCapabilities,
    /// Terminal identity the studio believes it is talking to.
    pub terminal: TerminalFeatures,
    /// Session recording opened with the studio - the tape the sessions
    /// panel marks as being written. `None` (the default) records nothing;
    /// a test opts in to exercise the recording lifecycle hermetically.
    pub recording: Option<crate::RecordingOptions>,
}

impl HarnessOptions {
    /// The hermetic baseline over a set directory.
    pub fn new(directory: impl Into<std::path::PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            theme: Some(crate::theme::DEFAULT_THEME.to_owned()),
            capabilities: KeyboardCapabilities::legacy(),
            terminal: TerminalFeatures {
                name: "rustel-e2e".to_owned(),
                ..TerminalFeatures::default()
            },
            recording: None,
        }
    }
}

/// A real studio, driven the way the terminal would drive it.
///
/// The worker thread inside is real, so an evaluate is asynchronous in the
/// same way it is live: `press` hands the key to the app, `settle` gives the
/// engine and the save and the lint queues their turns, and what the next
/// `screen` shows is what the terminal would have painted.
pub struct Studio {
    app: App,
    size: (u16, u16),
    /// The set directory the studio was opened over, kept for the accessor.
    directory: std::path::PathBuf,
    /// The frame `render` last painted, kept so `cell` can read one cell
    /// without repainting the world.
    last_buffer: Option<Buffer>,
    /// The terminal the frame was painted into, kept so [`Self::cursor`]
    /// can read where the studio left the terminal's own caret - the one
    /// thing a buffer read cannot show, drawn by the terminal over its
    /// cells rather than in them.
    terminal: Option<Terminal<TestBackend>>,
    /// Where painted frames are recorded for the demo video, when
    /// `RUSTEL_E2E_RECORD_DIR` is set. `None` - the normal state - costs
    /// one field and nothing else.
    record_dir: Option<std::path::PathBuf>,
    /// The open recording, created on the first paint of the first studio
    /// this test drives.
    recorder: Option<FrameRecorder>,
}

impl Studio {
    /// Open a studio over a set directory, the way `rustel studio` does.
    ///
    /// The differences from the binary are exactly the harness's knobs and
    /// none besides: `output: Some("silent")` so no audio device is opened,
    /// no default input for a sample either (`open_default_input: false`),
    /// no gamepad watcher (`watch_pads: false`), the log in the set's own
    /// config folder, an in-memory clipboard, and a pinned terminal
    /// identity. Default-sample loading stays on - the same
    /// pinned manifests the CLI loads, whose background fetch (a cache miss
    /// or a failure) never reaches the frame a score only names local banks
    /// in; registering those banks needs the loaded library. The prebake,
    /// prefs and session paths come from the environment the same way the
    /// CLI's do, which is how a test redirects them: it sets the variables
    /// before calling here, not after.
    pub fn open(options: HarnessOptions) -> Result<Self, RuntimeError> {
        let directory = options.directory;
        std::fs::create_dir_all(&directory).map_err(RuntimeError::Io)?;
        // The starter the CLI writes for a fresh folder, so a test that
        // passes an empty temporary directory opens on the same score the
        // musician would. Any other layout - scores provided, or housekeeping
        // folders alongside - is left exactly as given.
        let has_scores = directory
            .read_dir()
            .map_err(RuntimeError::Io)?
            .filter_map(Result::ok)
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "strudel"));
        if !has_scores {
            std::fs::write(directory.join("first.strudel"), "$: s(\"bd\")\n")
                .map_err(RuntimeError::Io)?;
        }
        let scenes = SceneSet::open(&directory, "first").map_err(|error| {
            RuntimeError::Message(format!("harness could not open the set: {error}"))
        })?;
        let theme = Theme::resolve(options.theme.as_deref())
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        let studio_options = StudioOptions {
            path: Some(directory.clone()),
            mini: false,
            session: Default::default(),
            theme: options.theme,
            output: Some("silent".to_owned()),
            output_buffer_frames: None,
            // No audio input in the harness: `s("in")` opens nothing and the
            // device panel shows no live input unless a test says otherwise.
            input: None,
            cancellation: None,
            recording: options.recording,
            build_features: &[],
            performance_events: false,
            #[cfg(feature = "remote-control")]
            remote_control: None,
            #[cfg(feature = "remote-control")]
            auth_token: None,
            // A hermetic studio still has the sample library: the inline
            // banks compile into the binary, and the background manifest
            // fetches only ever fail into the failure list - they surface
            // nowhere in the UI until a score asks for a sound the inline
            // set cannot resolve. Tests that need specific banks register
            // them from `local:` folders through the library's trusted API.
            default_samples: true,
            // The log is one file for every set, in the session directory
            // the environment pins - never in the runner's real home.
            memory_clipboard: true,
        };
        let worker = StudioWorker::spawn(
            StudioConfig {
                output: studio_options.output.clone(),
                default_samples: studio_options.default_samples,
                // A runner's plugged-in pads are not part of any test's
                // expected frame: the watcher's hello is once-per-process
                // and process-global, so without this whichever hermetic
                // studio pumps first inherits it, and the layout goldens
                // would flake on the runner's hardware.
                watch_pads: false,
                // Nor is the runner's microphone. With no input chosen,
                // ^H would open the host's default input; a hermetic
                // studio answers it as a machine with no input does.
                open_default_input: false,
                ..Default::default()
            },
            #[cfg(feature = "hydra")]
            rustel_runtime::hydra::HydraBridge::new(),
        )
        .map_err(|failure| RuntimeError::Message(failure.message))?;
        let mut app = App::new(studio_options, theme, scenes, worker)?;
        // What `run_once` does between opening the app and the loop: adopt
        // the terminal's identity (which is where the graphics tier comes
        // from) and read the global prebake from the redirected config home.
        app.adopt_terminal(options.terminal);
        app.global_prebake_path = Some(directory.join("config").join("prebake.strudel"));
        app.global_prebake = prebake::load_global_from(app.global_prebake_path.as_deref());
        app.queue_startup_prebakes();
        app.capabilities = options.capabilities;
        // Harness keys are injected after terminal delivery, so no desktop or
        // emulator can intercept them. Keep the product terminal identity for
        // rendering while letting each injected chord reach the keymap.
        app.keybinds.set_reach(crate::keybinds::Reach::default());
        // The panels come back the way they were left - the same restore
        // the CLI's startup does, so a test reopening a set sees the desk,
        // the set panel and the docks a previous studio left standing,
        // keyless, exactly as the product would show them.
        app.restore_panels_from_prefs();
        // The frame's host-read inputs (process stats, render timing, MIDI
        // inventory) are pinned, so the frame a test sees is the app's own
        // doing and holds on every machine.
        app.pin_frame_inputs = true;
        // The device listing is host-read too, and the panel shows it
        // directly: stand in with the host that has nothing attached and
        // nothing failing, or a runner whose MIDI backend cannot even
        // initialize writes its own error where a test expects no ports.
        app.devices
            .pin(crate::devices::DeviceInventory::no_hardware());
        // Paint the opening frame now. The real loop paints before it reads
        // a key (`dirty_frame` starts true), and key handlers read the
        // frame's geometry - the log panel's scroll rows among them - so a
        // harness that defers the first paint makes the first key behave
        // differently in a test than behind a terminal.
        // `RUSTEL_E2E_RECORD_DIR`: when a run sets it, every painted
        // frame - the explicit `screen`/`rows` calls, plus a snapshot
        // after every input and settle - is appended to
        // `<dir>/<test>__<n>.jsonl`, one ANSI frame per line.
        // `src/bin/e2e-demo.rs` turns those into the suite's demo video.
        // Opt-in, so the suite's normal runs pay nothing beyond the env
        // read; an empty value means off.
        let record_dir = std::env::var_os("RUSTEL_E2E_RECORD_DIR")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from);
        if let Some(dir) = &record_dir {
            std::fs::create_dir_all(dir).expect("the recording directory opens");
        }
        let mut studio = Self {
            app,
            size: (150, 40),
            directory: directory.clone(),
            last_buffer: None,
            terminal: None,
            record_dir,
            recorder: None,
        };
        // The opening frame, before any key can arrive.
        studio.paint_once();
        Ok(studio)
    }

    // -- input ------------------------------------------------------------

    /// Send one key, as the terminal would report it. This is one call into
    /// the real `App::handle_terminal_event`, with no panel or focus
    /// special-casing: what the app does with the key is what it does live.
    pub fn press(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.handle(Event::Key(crossterm::event::KeyEvent::new(code, modifiers)));
    }

    /// Send a chord by name: `"ctrl+s"`, `"ctrl+shift+x"`, `"f5"`. A typo is
    /// a panic - a test that names a chord that does not exist is broken,
    /// not merely unsuccessful.
    pub fn chord(&mut self, chord: &str) {
        let (code, modifiers) = parse_chord(chord);
        self.press(code, modifiers);
    }

    /// Type text one character at a time, as a keyboard does.
    pub fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.press(KeyCode::Char(character), KeyModifiers::NONE);
        }
    }

    /// Paste text, through the same event a terminal delivers a bracketed
    /// paste with - not a synthetic sequence of keystrokes.
    pub fn paste(&mut self, text: &str) {
        self.handle(Event::Paste(text.to_owned()));
    }

    /// Send a mouse event, built here so a test names intent rather than
    /// struct fields.
    pub fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) {
        self.handle(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }));
    }

    /// A left click and release.
    pub fn click(&mut self, column: u16, row: u16) {
        self.mouse(
            MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            KeyModifiers::NONE,
        );
        self.mouse(
            MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            KeyModifiers::NONE,
        );
    }

    /// Resize the frame the next render paints. The app also learns of the
    /// resize through the event a real terminal sends, so both sides of the
    /// terminal's behaviour are covered.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.size = (width, height);
        self.handle(Event::Resize(width, height));
    }

    /// Plug a virtual MIDI controller in, the way a USB pad appears at a
    /// port: the studio sees a MIDI input it can learn from, and nothing
    /// on the runner is opened or touched. The controller's notes arrive
    /// through [`Self::midi_note_on`].
    pub fn attach_midi_controller(&mut self, port: &str) {
        self.app.pads.attach_harness_port(port);
    }

    /// One note from the virtual controller, delivered through the same
    /// path a real port's driver callback takes, and one turn of the loop
    /// so the studio answers it the way it answers a real press: a learn
    /// in flight completes, a bound pad launches its scene. The same note
    /// twice inside the mirror gate's 40 ms window is one press, as it is
    /// live - a test re-pressing the same pad waits the window out first.
    pub fn midi_note_on(&mut self, port: &str, channel: u8, note: u8, velocity: u8) {
        self.app.pads.harness_message(
            port,
            rustel_midi::input::MidiEvent::NoteOn {
                channel,
                note,
                velocity,
            },
        );
        self.pump();
        self.snapshot();
    }

    // -- turns of the loop ------------------------------------------------

    /// One turn of the loop's per-tick housekeeping - the drains, the timed
    /// decorations, the debounce timers - exactly what [`App::pump_turn`]
    /// does for the event loop between frames. A test calls this between a
    /// key and its effect when it wants to observe an intermediate state
    /// without waiting for the world to go quiet.
    pub fn pump(&mut self) {
        self.app.pump_turn();
    }

    /// Turn the loop until the engine, the save worker, the linter and the
    /// prebake queue have nothing pending, then wait out the lint debounce
    /// and settle once more, so an edit's diagnostics are part of the final
    /// state. This is the public form of the in-tree suite's `settle`.
    ///
    /// Deliberately not instant: the engine answers when it answers, and a
    /// test that asserts before `settle` returns is asserting the queue, not
    /// the studio.
    pub fn settle(&mut self) {
        self.settle_once();
        // The lint debounce means an edit's diagnostics land a beat after
        // the text does, and the theme picker's apply debounce means a
        // browsed theme lands after the arrows stop; wait the longer of the
        // two out and settle again so what the test sees is what a musician
        // who stopped typing would see.
        std::thread::sleep(
            super::LINT_DEBOUNCE.max(super::THEME_APPLY_SETTLE) + Duration::from_millis(50),
        );
        self.settle_once();
        // The settled state is the state a musician who stopped acting
        // would be looking at - worth a snapshot for the demo.
        self.snapshot();
    }

    fn settle_once(&mut self) {
        for _ in 0..2_000 {
            self.app.pump_turn();
            let app = &self.app;
            let quiet = app.prebake_inflight.is_none()
                && app.prebake_queue.is_empty()
                && app.pending_evaluation.is_none()
                && app.inflight.is_empty()
                && !app.lint_pending
                && app.save_worker.idle();
            if quiet {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Not an error: an engine that never answers is a state a test may
        // be deliberately in. The caller asserts on what it can see.
    }

    // -- output -----------------------------------------------------------

    /// Render the frame the terminal would paint and return it as rows.
    /// Live values (the meter, the playhead, beat pulses, the stats) are in
    /// here; a golden that contains one is a bug in the test.
    pub fn render(&mut self) -> Vec<String> {
        self.paint_once();
        let buffer = self
            .last_buffer
            .as_ref()
            .expect("paint_once filled the buffer");
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol().to_owned())
                    .collect::<String>()
            })
            .collect()
    }

    /// Render the frame and return the screen as rows joined with newlines -
    /// the shape `tests/studio_frame.rs` already flattens to.
    pub fn screen(&mut self) -> String {
        self.render().join("\n")
    }

    /// One cell's symbol from the current frame. Paints first, like every
    /// other reader - a cell read after a key without an intervening
    /// `rows()` must be the frame that key produced, not whatever the last
    /// assert happened to leave in `last_buffer`.
    pub fn cell(&mut self, x: u16, y: u16) -> Option<String> {
        self.paint_once();
        self.last_buffer
            .as_ref()
            .and_then(|buffer| buffer.cell((x, y)))
            .map(|cell| cell.symbol().to_owned())
    }

    /// One cell's foreground colour from the current frame, painting
    /// first as [`Self::cell`] does. Decorations (bracket match, syntax
    /// colour) are colour contracts, not text ones - a test that wants
    /// "the bracket is lit" asks for the colour rather than for a glyph.
    pub fn cell_fg(&mut self, x: u16, y: u16) -> Option<ratatui::style::Color> {
        self.paint_once();
        self.last_buffer
            .as_ref()
            .and_then(|buffer| buffer.cell((x, y)))
            .map(|cell| cell.fg)
    }

    /// One cell's background colour, including block-shaped decorations.
    pub fn cell_bg(&mut self, x: u16, y: u16) -> Option<ratatui::style::Color> {
        self.paint_once();
        self.last_buffer
            .as_ref()
            .and_then(|buffer| buffer.cell((x, y)))
            .map(|cell| cell.bg)
    }

    /// One cell's underline colour, including bracket and lint marks.
    pub fn cell_underline_color(&mut self, x: u16, y: u16) -> Option<ratatui::style::Color> {
        self.paint_once();
        self.last_buffer
            .as_ref()
            .and_then(|buffer| buffer.cell((x, y)))
            .map(|cell| cell.underline_color)
    }

    // -- typed accessors ---------------------------------------------------

    /// Where the keyboard is. Behavioural assertions go through this, not
    /// through screens: focus is the contract, the footer is its rendering.
    pub fn focus(&self) -> Option<PanelKind> {
        match self.app.focus {
            Focus::Editor => None,
            Focus::Panel(kind) => Some(kind),
            // A replay's timeline has the keys; no panel does.
            Focus::Timeline => None,
        }
    }

    /// Whether a replay's timeline, not the editor, has the keys.
    pub fn timeline_focused(&self) -> bool {
        matches!(self.app.focus, Focus::Timeline)
    }

    /// How many blocks the focused pane's timeline shows across, as last
    /// laid out - the screenful PgUp and PgDn move by. `None` when the
    /// focused pane has no timeline on screen. A paging test reads this
    /// rather than assuming a pane width, so it holds at any frame size.
    pub fn timeline_blocks_across(&self) -> Option<usize> {
        self.app.timeline_page()
    }

    /// The status bar's text.
    pub fn status(&self) -> &str {
        &self.app.status
    }

    /// The focused pane's source, exactly as stored.
    pub fn source(&self) -> String {
        self.app.editor().source()
    }

    /// The studio's current theme's name - what the picker previews and
    /// Enter keeps.
    pub fn theme_name(&self) -> String {
        self.app.theme.name.clone()
    }

    /// The theme's bracket-pair colour - what the brackets under the caret
    /// are drawn in, and what a bracket-match test reads the frame against.
    pub fn theme_bracket_color(&self) -> ratatui::style::Color {
        self.app.theme.bracket()
    }

    /// The theme's error colour - what an orphan bracket wears.
    pub fn theme_error_color(&self) -> ratatui::style::Color {
        self.app.theme.error
    }

    /// The scene names, in strip order.
    pub fn scene_names(&self) -> Vec<String> {
        self.app
            .scenes
            .scenes()
            .iter()
            .map(|scene| scene.name().to_owned())
            .collect()
    }

    /// The scene that is lit - the one the strip scrolls to and the pane
    /// shows. A test that walks scenes reads this rather than guessing
    /// from the header, whose readout can truncate on a narrow terminal.
    pub fn current_scene(&self) -> Option<String> {
        Some(self.app.scenes.current().name().to_owned())
    }

    /// The folder of the set open *now* - live, not the one the harness
    /// was created over. A test that makes, opens or renames a set through
    /// the studio reads this to find where the set went on the disk.
    pub fn set_directory(&self) -> std::path::PathBuf {
        self.app.scenes.directory().to_path_buf()
    }

    /// The error line's current text, if one is showing.
    pub fn errors(&self) -> Option<String> {
        self.app.errors.visible().map(str::to_owned)
    }

    /// The toast text, if one is up.
    pub fn toast(&self) -> Option<String> {
        self.app.toast.as_ref().map(|(text, _)| text.clone())
    }

    /// How long a toast keeps its welcome - a test that wants one gone
    /// waits this out (plus a beat) and pumps, exactly what the event loop
    /// does between frames.
    pub fn toast_duration() -> Duration {
        super::TOAST_DURATION
    }

    /// The open device panel, if one is open - open and focused differ:
    /// the reference stays readable while the editor owns the keyboard.
    pub fn panel(&self) -> Option<&crate::devices::DevicePanel> {
        self.app.panel.as_ref()
    }

    /// Whether playback is sounding.
    pub fn is_playing(&self) -> bool {
        self.app.is_playing()
    }

    /// Whether a stop is in flight.
    pub fn is_stopping(&self) -> bool {
        self.app.is_stopping()
    }

    /// Whether an evaluate is in flight.
    pub fn is_evaluating(&self) -> bool {
        self.app.is_evaluating()
    }

    /// The index of the scene the focused pane is showing, in strip order -
    /// the observable half of the previous/next-scene chords.
    pub fn current_scene_index(&self) -> usize {
        self.app.scenes.current_index()
    }

    /// Whether the menu bar is open with nothing dropped down - what F1
    /// does where the bar has a row to live in.
    pub fn menu_open(&self) -> bool {
        self.app.menu.is_some()
    }

    /// Where the studio left the terminal's own caret after the last
    /// paint, and whether it is shown at all. The caret is the one thing a
    /// buffer read cannot show - the terminal draws it over its cells - so
    /// an assert about layering (a dropped menu covering the caret) asks
    /// here rather than in [`Self::cell`]. Paints first, as every reader
    /// does.
    pub fn cursor(&mut self) -> Option<(u16, u16)> {
        self.paint_once();
        let backend = self.terminal.as_ref()?.backend();
        if !backend.cursor_visible() {
            return None;
        }
        let position = backend.cursor_position();
        Some((position.x, position.y))
    }

    /// Whether the help overlay is up - what F1 means where the bar has no
    /// row (zen, the menu bar hidden, or a terminal too short for one).
    pub fn help_is_open(&self) -> bool {
        self.app.help.is_some()
    }

    /// Quit the studio the way `Ctrl+Q` does. The workers are taken down in
    /// `Drop`; a test ends on this when it wants to assert the quit path.
    pub fn quit(&mut self) {
        self.app.quit = true;
    }

    /// Whether the app's loop has been told to quit - the truth the
    /// File ▸ Quit row and the second ^Q of the armed chord reach.
    pub fn wants_quit(&self) -> bool {
        self.app.quit
    }

    /// The set directory this studio was opened over.
    pub fn directory(&self) -> &std::path::Path {
        &self.directory
    }

    /// The folder the sessions panel lists and a take records into: the
    /// recording options' directory, or the session log's default - the
    /// environment's answer, whatever a test pinned it to. Exposed so a
    /// test writes its tapes where the panel actually looks.
    pub fn sessions_directory(&self) -> std::path::PathBuf {
        self.app.sessions_directory()
    }

    /// The folder finished recording takes use, independently of session
    /// tapes. Tests inspect this instead of assuming both features share a
    /// directory.
    pub fn recordings_directory(&self) -> std::path::PathBuf {
        self.app.recordings_directory()
    }

    /// Whether the focused scene has unsaved edits - the dirty chip's
    /// contract, asserted directly rather than read off a screen.
    pub fn is_dirty(&self) -> bool {
        self.app.scenes.current().dirty
    }

    /// Whether the focused scene's editor can undo - history is the
    /// contract for every edit test, deeper than one screen repaint.
    pub fn can_undo(&self) -> bool {
        self.app.editor().can_undo()
    }

    /// The clipboard's text, as the studio last set it. The harness pins an
    /// in-memory clipboard, so this is the only thing a test's copy keys
    /// could have written - and the thing a paste would read back.
    pub fn clipboard_text(&mut self) -> Option<String> {
        self.app.clipboard.get_text().ok()
    }

    /// Whether the focused scene's editor can redo.
    pub fn can_redo(&self) -> bool {
        self.app.editor().can_redo()
    }

    /// The master fader's position, in dB - the state `Ctrl+Shift+↑/↓`
    /// moves.
    pub fn master_gain_db(&self) -> f32 {
        self.app.master.gain_db()
    }

    /// Replace the focused scene's whole score - the way a paste of a
    /// complete score would, without a test typing it key by key. The
    /// caret ends after the last inserted byte, where a paste leaves it.
    pub fn set_score(&mut self, text: &str) {
        let len = self.app.editor().document().len_bytes();
        self.app
            .dispatch_editor(crate::editor::Command::ReplaceRange {
                from: crate::editor::ByteOffset(0),
                to: crate::editor::ByteOffset(len),
                text: text.to_owned(),
            })
            .expect("score replaced");
        self.snapshot();
    }

    /// The focused scene's whole score, as text.
    pub fn score(&self) -> String {
        self.app.editor().document().text()
    }

    /// Put the caret at a byte offset of the focused scene - the way a
    /// click places it, with no selection. Tests walk the caret to a
    /// bracket or a word without guessing at screen geometry.
    pub fn set_caret(&mut self, offset: usize) {
        self.app
            .editor_mut()
            .set_selection(crate::editor::Selection::caret(crate::editor::ByteOffset(
                offset,
            )))
            .expect("caret moved");
        self.snapshot();
    }

    /// The reference panel's tab, when the panel is open - what Tab walks
    /// and what decides which list the keys mean anything to.
    pub fn reference_tab(&self) -> Option<&'static str> {
        use crate::reference::Tab;
        Some(match self.app.reference_panel.as_ref()?.tab {
            Tab::Reference => "reference",
            Tab::Samples => "samples",
            Tab::Chords => "chords",
            Tab::Scales => "scales",
            #[cfg(feature = "hydra")]
            Tab::Examples => "examples",
            #[cfg(feature = "hydra")]
            Tab::Generator => "generator",
        })
    }

    /// Register a host-trusted local sample folder - the hermetic way a
    /// test gives the samples browser banks to list, with no network.
    /// `root` is a directory the set folder contains (created if missing);
    /// its subfolders become banks. Blocking: a returned `Ok` means the
    /// banks are registered and playable.
    pub fn register_local_samples(&mut self, root: &str) -> Result<(), RuntimeError> {
        let library = self
            .app
            .worker
            .library()
            .ok_or_else(|| RuntimeError::Message("the studio has no sample library".into()))?;
        let samples_root = self.directory.join(root);
        std::fs::create_dir_all(&samples_root).map_err(RuntimeError::Io)?;
        let map = serde_json::to_string(&format!("local:{}", samples_root.display()))
            .expect("a path serialises");
        library
            .register_trusted_custom(&map, None)
            .map_err(RuntimeError::Message)?;
        // The helper mutates the library directly instead of going through
        // the settings action, so it must ask the worker for the same fresh
        // catalogue that action would request.
        self.app.refresh_catalogue();
        Ok(())
    }

    /// The sound names the samples browser would list right now, in its
    /// own order - the catalogue's contract with the browser, read through
    /// the same call the refresh uses.
    pub fn samples_catalogue_names(&self) -> Vec<String> {
        self.app
            .worker
            .library()
            .map(|library| {
                library
                    .catalogue()
                    .into_iter()
                    .map(|entry| entry.name)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pump until the open samples tab has read the library's catalogue in.
    /// The refresh rides a one-second loop tick, so a test that asserts
    /// right after opening the tab would otherwise see the loading state a
    /// musician sees for one beat.
    pub fn wait_for_catalogue(&mut self) {
        use crate::reference::Tab;
        let expected = self.samples_catalogue_names();
        for _ in 0..200 {
            self.app.pump_turn();
            if let Some(panel) = &self.app.reference_panel
                && panel.tab == Tab::Samples
                && expected
                    .iter()
                    .all(|name| panel.sounds.iter().any(|sound| sound.name.as_str() == name))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the samples tab never read the catalogue in");
    }

    /// Pump until the focused scene's sounds have all resolved - the
    /// header reads `✓ ready` (or a failed/unknown state), never `◐
    /// loading`. The lint warms readiness at open, and the fetch answers
    /// on the loop's own ticks, so a test that asserts at once would see
    /// one beat of loading that a slow machine stretches.
    pub fn wait_for_readiness(&mut self) {
        use crate::view::GoState;
        for _ in 0..200 {
            self.app.pump_turn();
            if !matches!(self.app.go_state(), GoState::Loading { .. }) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the scene's sounds never stopped loading");
    }

    // -- internals ---------------------------------------------------------

    /// One event through the real handler. A harness that cannot handle an
    /// event is a broken harness: panic rather than fail the test with a
    /// lie about what happened.
    fn handle(&mut self, event: Event) {
        if let Err(error) = self.app.handle_terminal_event(event) {
            panic!("the studio refused an event: {error}");
        }
        // The demo records the frame after each input, so the video shows
        // the interaction - key, then the screen it produced - rather than
        // only the screens a test happened to assert on.
        self.snapshot();
    }

    /// Record one frame of the current state, when recording. A test's
    /// explicit `screen`/`rows` calls paint through `paint_once` anyway;
    /// this is what lets the demo show every step the test drove.
    fn snapshot(&mut self) {
        if self.record_dir.is_some() {
            self.paint_once();
        }
    }

    /// Paint one frame into a fresh test backend. The app's own `paint_into`
    /// does all the work - the same compose, the same paint - so what a test
    /// renders is what a terminal renders.
    fn paint_once(&mut self) {
        let (width, height) = self.size;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let started = std::time::Instant::now();
        terminal
            .draw(|frame| {
                self.app
                    .paint_into(frame, started)
                    .expect("harness frame paints");
            })
            .expect("test backend draw");
        let buffer = terminal.backend().buffer().clone();
        self.last_buffer = Some(buffer.clone());
        self.terminal = Some(terminal);
        // The demo-video seam: the frame the terminal would have received
        // is the frame the recording gets, so the video shows what the
        // tests actually asserted on.
        if let Some(dir) = &self.record_dir {
            let recorder = self
                .recorder
                .get_or_insert_with(|| FrameRecorder::open_under(dir).expect("the recorder opens"));
            recorder.record(&buffer, width, height);
        }
    }
}

/// One frame recording: the frame's ANSI text, appended to this studio's
/// JSON-lines file under `RUSTEL_E2E_RECORD_DIR`.
///
/// `paint_once` is the only place frames are painted, so a recording made
/// here is exactly the sequence of screens each test drove: what the video
/// shows is what the test saw. The test name comes from the thread libtest
/// runs the test on - the demo run therefore uses the default parallel
/// test threads (which is also the faster run); under `--test-threads=1`
/// the fallback name is used and the segments still render, just without
/// the banner naming each one.
struct FrameRecorder {
    file: std::fs::File,
    test_name: String,
    frame: u64,
    last: String,
}

impl FrameRecorder {
    /// Open (or reopen, for a test that drives a second studio) this
    /// test's recording file. Each studio instance gets its own file -
    /// `__<n>` - so two studios one test opens never interleave frames.
    fn open_under(dir: &std::path::Path) -> std::io::Result<Self> {
        static INSTANCE: AtomicU64 = AtomicU64::new(0);
        let test_name = std::thread::current()
            .name()
            .map(str::to_owned)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "unknown-test".to_owned());
        let safe: String = test_name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!(
            "{safe}__{}.jsonl",
            INSTANCE.fetch_add(1, Ordering::Relaxed)
        ));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file,
            test_name,
            frame: 0,
            last: String::new(),
        })
    }

    /// Append one frame, unless it is byte-identical to the last recorded
    /// one - a settle's quiet wait paints the same frame many times, and
    /// a demo gains nothing from the repeats.
    fn record(&mut self, buffer: &Buffer, width: u16, height: u16) {
        let ansi = buffer_to_ansi(buffer);
        if ansi == self.last {
            return;
        }
        self.last = ansi.clone();
        let line = serde_json::json!({
            "test": self.test_name,
            "n": self.frame,
            "w": width,
            "h": height,
            "ansi": ansi,
        });
        self.frame += 1;
        let mut line = line.to_string();
        line.push('\n');
        use std::io::Write;
        if let Err(error) = self.file.write_all(line.as_bytes()) {
            // A recording that silently stops would produce a demo that
            // quietly misses the tests after the failure. A test that
            // cannot record is a broken demo run, not a broken test.
            panic!("the frame recorder could not write: {error}");
        }
    }
}

/// The frame as ANSI truecolor text: `\x1b[38;2;r;g;bm` foreground,
/// `\x1b[48;2;r;g;bm` background, one `\x1b[0m` per row. Cells with no
/// colour render as plain text; cells a wide glyph left empty become a
/// space, which keeps the grid exactly `width`×`height` for the renderer.
fn buffer_to_ansi(buffer: &Buffer) -> String {
    use ratatui::style::Color;
    let mut out = String::with_capacity((buffer.area.width * buffer.area.height * 3) as usize);
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let Some(cell) = buffer.cell((x, y)) else {
                out.push(' ');
                continue;
            };
            if let Color::Rgb(r, g, b) = cell.fg {
                out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
            }
            if let Color::Rgb(r, g, b) = cell.bg {
                out.push_str(&format!("\x1b[48;2;{r};{g};{b}m"));
            }
            out.push(cell.symbol().chars().next().unwrap_or(' '));
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

impl Drop for Studio {
    fn drop(&mut self) {
        // What `run_once` does after the loop: the worker joins, the save
        // queue drains. Without this a test leaks a live engine thread.
        self.app.worker.shutdown();
        let _ = self.app.shutdown_save_worker();
    }
}

/// `"ctrl+shift+x"` → `(Char('x'), CONTROL | SHIFT)`; `"f5"` → `(F(5), NONE)`.
/// Unknown names panic: a test naming a chord that does not exist is broken.
pub(super) fn parse_chord(chord: &str) -> (KeyCode, KeyModifiers) {
    let mut modifiers = KeyModifiers::NONE;
    let mut rest = chord;
    // `alt++` is a documented chord: the trailing plus is the KEY, not
    // another separator. Walking while the remainder is exactly `+` leaves
    // that last plus for the key match below.
    while rest != "+"
        && let Some((part, tail)) = rest.split_once('+')
    {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "alt" | "opt" | "option" => modifiers |= KeyModifiers::ALT,
            "cmd" | "super" => modifiers |= KeyModifiers::SUPER,
            // META is its own bit in crossterm, not another spelling of SUPER.
            // Mapping it there would have a test assert on a modifier the
            // product never sees.
            "meta" => modifiers |= KeyModifiers::META,
            other => panic!("unknown modifier `{other}` in chord `{chord}`"),
        }
        rest = tail;
    }
    let name = rest.trim().to_ascii_lowercase();
    let code = match name.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        function if function.starts_with('f') && function[1..].parse::<u8>().is_ok() => {
            KeyCode::F(function[1..].parse().expect("parsed above"))
        }
        // `name` is already lowercase, so there is no case left to preserve
        // here. A shifted letter reaches the app as the lowercase codepoint
        // plus a SHIFT modifier - how the enhanced-keyboard path reports it,
        // and how the app derives the shift.
        single if single.chars().count() == 1 => {
            KeyCode::Char(single.chars().next().expect("one character"))
        }
        other => panic!("unknown key `{other}` in chord `{chord}`"),
    };
    // A terminal reports Shift+Tab as `BackTab`, never as `Tab` carrying a
    // SHIFT modifier: crossterm normalises both the `ESC[Z` and the CSI-u
    // paths to it, and the app's own comment on the mixer's desk key says the
    // same. Handing out `(Tab, SHIFT)` would let a test drive a key the
    // product can never receive - and pass.
    if code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT) {
        return (KeyCode::BackTab, modifiers - KeyModifiers::SHIFT);
    }
    (code, modifiers)
}
