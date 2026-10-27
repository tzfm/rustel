//! Check Alt chords, SGR mouse reports and pane shortcuts through a real PTY.
//! In-process event injection bypasses terminal decoding, including ConPTY's
//! conversion of ESC-prefixed input to Alt key events.
//!
//! Run with `cargo test -p rustel-studio-e2e --features pty`.

#![cfg(feature = "pty")]

#[path = "pty_alt_chords/gestures.rs"]
mod gestures;

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use rustel_studio_e2e::printable;

/// Locate the `rustel` binary in the running test's Cargo profile directory.
/// For a relocated test, fall back to the configured or workspace target root.
fn rustel_binary() -> Option<std::path::PathBuf> {
    let exe = if cfg!(windows) {
        "rustel.exe"
    } else {
        "rustel"
    };
    if let Ok(test) = std::env::current_exe()
        && let Some(deps) = test.parent()
        && deps.file_name().is_some_and(|name| name == "deps")
        && let Some(profile) = deps.parent()
    {
        let binary = profile.join(exe);
        return binary.is_file().then_some(binary);
    }
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let roots = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => vec![std::path::PathBuf::from(dir), workspace],
        None => vec![workspace],
    };
    roots
        .into_iter()
        .map(|root| root.join(profile).join(exe))
        .find(|path| path.is_file())
}

struct Terminal {
    buffer: Arc<Mutex<Vec<u8>>>,
    // One writer, shared with the handshake responder a negotiated open
    // spawns: portable-pty hands out a single writer per master.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    // The master must outlive the child: on Windows dropping it closes the
    // ConPTY and takes the child with it.
    _master: Box<dyn MasterPty + Send>,
    // The responder stops itself once it has answered, so the join handle
    // only keeps the thread from outliving the terminal.
    _responder: Option<std::thread::JoinHandle<()>>,
}

impl Terminal {
    fn open(binary: &std::path::Path, rows: u16, cols: u16, cwd: &std::path::Path) -> Self {
        Self::open_reported_as(binary, rows, cols, cwd, None)
    }

    /// The same studio, under a terminal that names itself: `TERM_PROGRAM`
    /// decides the studio's terminal profile, which is where the Option
    /// recovery table lives.
    fn open_reported_as(
        binary: &std::path::Path,
        rows: u16,
        cols: u16,
        cwd: &std::path::Path,
        term_program: Option<&str>,
    ) -> Self {
        Self::open_with(binary, rows, cols, cwd, term_program, None)
    }

    /// The same studio, under a terminal that also answers the kitty
    /// keyboard protocol handshake, so the studio negotiates the protocol
    /// and pushes its flags, the way kitty and Ghostty do. The reply names
    /// the flags the "terminal" supports, then the primary device
    /// attributes, which is what the terminals send and what the studio's
    /// probe waits for.
    ///
    /// Windows ConPTY does not carry the protocol, so the negotiated
    /// proofs that use this open run on Unix alone, and the open exists
    /// there with them.
    #[cfg(unix)]
    fn open_negotiating(
        binary: &std::path::Path,
        rows: u16,
        cols: u16,
        cwd: &std::path::Path,
        term_program: &str,
    ) -> Self {
        Self::open_with(
            binary,
            rows,
            cols,
            cwd,
            Some(term_program),
            Some(b"\x1b[?31u\x1b[?62;1;2;6;9;15;18;21;22c".to_vec()),
        )
    }

    fn open_with(
        binary: &std::path::Path,
        rows: u16,
        cols: u16,
        cwd: &std::path::Path,
        term_program: Option<&str>,
        handshake: Option<Vec<u8>>,
    ) -> Self {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("a pseudo-terminal opens");
        let mut command = CommandBuilder::new(binary);
        command.arg("studio");
        command.arg(cwd.join("live.strudel"));
        command.arg("--output");
        command.arg("silent");
        command.cwd(cwd);
        command.env_remove("RUSTEL_THEME");
        command.env_remove("RUSTEL_THEME_DIR");
        command.env("RUSTEL_CONFIG_DIR", cwd);
        command.env("RUSTEL_NO_UPDATE_CHECK", "1");
        // Headless: this probe presses Alt+O, whose whole job is to ask the
        // desktop to open something. Without the pin the ask reaches the
        // machine running the test - real browser tabs on the developer's
        // desktop, from a terminal suite. With it the reveal answers at
        // once and the status walk is still exactly what a desktop gives.
        command.env(rustel_studio::reveal::HEADLESS_ENV, "1");
        // No plugin of the runner either: an empty list stands in for the
        // standard VST3 folders, so a vst tab lists and loads no plugin.
        command.env(rustel_runtime::vst::FOLDERS_ENV, "");
        for key in [
            "TERM",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "COLORTERM",
            "COLORFGBG",
            "KITTY_WINDOW_ID",
            "KONSOLE_VERSION",
            "XTERM_VERSION",
            "RXVT_SOCKET",
            "VTE_VERSION",
            "WT_SESSION",
            "RUSTEL_SESSION_DIR",
        ] {
            command.env_remove(key);
        }
        // The reported name goes on last, clear of the removals: the
        // studio's terminal profile hangs off it.
        if let Some(term_program) = term_program {
            command.env("TERM_PROGRAM", term_program);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .expect("the studio spawns on the pty");
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().expect("pty reader");
        let writer = Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer")));
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let gathered = Arc::clone(&buffer);
        // A negotiated open answers the studio's handshake query, which is
        // the first thing the studio writes. The responder waits for the
        // gathered output to open with the query, so nothing later in the
        // stream can take its answer, and answers it once.
        let responder = handshake.map(|reply| {
            let tail = Arc::clone(&buffer);
            let replier = Arc::clone(&writer);
            std::thread::Builder::new()
                .name("handshake-responder".into())
                .spawn(move || {
                    let deadline = std::time::Instant::now() + Duration::from_secs(30);
                    while std::time::Instant::now() < deadline {
                        let asked = {
                            let Ok(gathered) = tail.lock() else { return };
                            gathered.starts_with(b"\x1b[?u")
                        };
                        if asked {
                            if let Ok(mut writer) = replier.lock() {
                                let _ = writer.write_all(&reply);
                                let _ = writer.flush();
                            }
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                })
                .expect("the responder spawns")
        });
        std::thread::Builder::new()
            .name("pty-drain".into())
            .spawn(move || {
                let mut reader = reader;
                let mut chunk = [0u8; 8192];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut buffer) = gathered.lock() {
                                buffer.extend_from_slice(&chunk[..n]);
                            }
                        }
                    }
                }
            })
            .expect("the drain thread spawns");
        Self {
            buffer,
            writer,
            child,
            killer,
            _master: pair.master,
            _responder: responder,
        }
    }

    /// The stream so far, safe to print: every escape sequence and control
    /// byte spelled out (`ESC[2J`, `<BEL>`), nothing left for the reader's
    /// terminal to execute. A panic message that embedded the raw capture
    /// would beep, wipe and alt-screen the terminal running the suite.
    fn diagnostics(&self) -> String {
        printable(&self.text())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.buffer.lock().expect("drain buffer")).into_owned()
    }

    /// Whether the needle has appeared. Matches as pty_smoke does: against
    /// the reconstructed screen and the plain text, because the diff painter
    /// splits lines across cursor repositions.
    fn wait_for(&mut self, needle: &str, budget: Duration) -> bool {
        let stop = Instant::now() + budget;
        while Instant::now() < stop {
            if contains_plain(&self.text(), needle) {
                return true;
            }
            if self.child.try_wait().expect("child polls").is_some() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// The same wait, but only against output that arrived after `mark`. A
    /// needle that the studio painted before this step cannot satisfy the
    /// wait from a stale frame.
    fn wait_for_since(&mut self, mark: usize, needle: &str, budget: Duration) -> bool {
        let stop = Instant::now() + budget;
        while Instant::now() < stop {
            if contains_plain(&self.text()[mark..], needle) {
                return true;
            }
            if self.child.try_wait().expect("child polls").is_some() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Match the current screen, including cells the diff painter retained
    /// from the previous tab, without accepting text from an older frame.
    fn wait_for_screen(&mut self, needle: &str, budget: Duration) -> bool {
        let stop = Instant::now() + budget;
        while Instant::now() < stop {
            let mut screen = Screen::new(80, 24);
            screen.feed(&self.text());
            if screen.contains(needle) {
                return true;
            }
            if self.child.try_wait().expect("child polls").is_some() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().expect("the writer");
        writer.write_all(bytes).expect("the pty takes input");
        writer.flush().expect("the pty flushes");
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.killer.kill();
        for _ in 0..20 {
            match self.child.try_wait() {
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                _ => break,
            }
        }
    }
}

/// The screen a terminal reconstructs from the studio's diff painting. The
/// painter emits CUP positioning, erase-to-end-of-line, erase-characters and
/// clear-screen. Reading them as a terminal does matches a needle against
/// the visible screen and not against the byte stream. The erasures matter
/// under ConPTY: without them a rewritten row keeps cells from the previous
/// frame, and a row that is not selected can match by mistake.
struct Screen {
    cols: usize,
    rows: usize,
    cells: Vec<char>,
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![' '; cols * rows],
        }
    }

    fn put(&mut self, row: usize, col: usize, ch: char) {
        if row < self.rows && col < self.cols {
            self.cells[row * self.cols + col] = ch;
        }
    }

    fn blank(&mut self, row: usize, from: usize, to: usize) {
        for col in from..to.min(self.cols) {
            self.put(row, col, ' ');
        }
    }

    fn feed(&mut self, frame: &str) {
        let (mut row, mut col) = (0usize, 0usize);
        let mut characters = frame.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '\u{1b}' {
                match characters.peek() {
                    Some('[') => {
                        characters.next();
                        let mut body = String::new();
                        while let Some(&next) = characters.peek() {
                            characters.next();
                            if next.is_ascii_alphabetic() {
                                body.push(next);
                                break;
                            }
                            body.push(next);
                        }
                        let final_byte = body.pop().unwrap_or(' ');
                        let params: Vec<usize> = body
                            .split(';')
                            .map(|part| part.parse::<usize>().unwrap_or(0))
                            .collect();
                        let first = params.first().copied().unwrap_or(0);
                        match final_byte {
                            'H' => {
                                let mut coords = params.iter().copied().chain([1, 1]);
                                row = coords.next().unwrap_or(1).max(1) - 1;
                                col = coords.next().unwrap_or(1).max(1) - 1;
                            }
                            'K' => match first {
                                0 => self.blank(row, col, self.cols),
                                1 => self.blank(row, 0, col + 1),
                                _ => self.blank(row, 0, self.cols),
                            },
                            'J' => {
                                if first == 0 {
                                    self.blank(row, col, self.cols);
                                    for below in row + 1..self.rows {
                                        self.blank(below, 0, self.cols);
                                    }
                                } else if first == 2 {
                                    for every in 0..self.rows {
                                        self.blank(every, 0, self.cols);
                                    }
                                }
                            }
                            'X' => self.blank(row, col, col + first.max(1)),
                            'C' => col += first.max(1),
                            _ => {}
                        }
                    }
                    Some(']') => {
                        characters.next();
                        while let Some(&next) = characters.peek() {
                            characters.next();
                            if next == '\u{7}' {
                                break;
                            }
                            if next == '\u{1b}' {
                                // ConPTY ends OSC strings with the two-byte
                                // ST (ESC \): swallow the backslash too, or
                                // it lands in the grid as a printable cell
                                // and every column after it drifts by one.
                                if characters.peek() == Some(&'\\') {
                                    characters.next();
                                }
                                break;
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            if character == '\r' {
                col = 0;
                continue;
            }
            if character == '\n' {
                row += 1;
                continue;
            }
            if character == '\u{7}' {
                continue;
            }
            // A full-width line wraps on the next printable character:
            // ConPTY re-serializes the screen without explicit newlines
            // between rows and relies on the terminal's autowrap.
            if col >= self.cols {
                col = 0;
                row += 1;
            }
            self.put(row, col, character);
            col += 1;
        }
    }

    fn row_text(&self, row: usize) -> String {
        self.cells[row * self.cols..(row + 1) * self.cols]
            .iter()
            .collect()
    }

    fn contains(&self, needle: &str) -> bool {
        (0..self.rows).any(|r| self.row_text(r).contains(needle))
    }

    /// Whether the selected row carries the needle. The studio marks the
    /// selected row with a selector glyph (`▸` or `>`). A list paints the
    /// text of every row, so only the selector identifies the row under the
    /// cursor. The selector is not always the first cell: the settings sheet
    /// draws its border left of the list, so a selected row reads `│ ▸ ...`
    /// or `| > ...`.
    fn selected_row_contains(&self, needle: &str) -> bool {
        (0..self.rows).any(|r| {
            let line = self.row_text(r);
            (line.contains('▸') || line.contains('>')) && line.contains(needle)
        })
    }
}

/// Whether the needle is on screen in `frame`: the plain text and every
/// reconstructed row are both searched, since the diff painter splits lines
/// across cursor repositions.
fn contains_plain(frame: &str, needle: &str) -> bool {
    if frame.contains(needle) {
        return true;
    }
    let mut screen = Screen::new(80, 24);
    screen.feed(frame);
    screen.contains(needle)
}

/// Whether the row the terminal-profile selector sits on carries the needle.
fn selected_contains(frame: &str, needle: &str) -> bool {
    let mut screen = Screen::new(80, 24);
    screen.feed(frame);
    screen.selected_row_contains(needle)
}

#[test]
fn probe_alt_chords_through_a_real_pty() {
    let Some(binary) = rustel_binary() else {
        panic!("build the binary first");
    };
    let home = tempfile::tempdir().expect("temporary home");
    std::fs::write(
        home.path().join("live.strudel"),
        "$: note(\"c4\").s(\"sine\")\n",
    )
    .expect("starter score");
    // A local fixture makes the reveal probe independent of remote sample
    // manifests and of how quickly the catalogue finishes loading on CI.
    let sample_dir = home.path().join("pty_probe");
    std::fs::create_dir(&sample_dir).expect("sample folder");
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&38_u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1_u16.to_le_bytes()); // mono
    wav.extend_from_slice(&8000_u32.to_le_bytes());
    wav.extend_from_slice(&16000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&2_u32.to_le_bytes());
    wav.extend_from_slice(&0_i16.to_le_bytes());
    std::fs::write(sample_dir.join("pty_probe.wav"), wav).expect("sample file");
    let prefs = serde_json::json!({"sample_sources": [{"spec": sample_dir} ]});
    std::fs::write(home.path().join("studio.json"), prefs.to_string())
        .expect("sample source preferences");
    let mut terminal = Terminal::open(&binary, 24, 80, home.path());

    // Diagnostic: did the child start at all?
    let text = terminal.text();
    let tail: String = text
        .chars()
        .rev()
        .take(3000)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    // Printable: this diagnostic carries the raw capture, whose BELs and
    // clears would otherwise be executed by whichever terminal showed it.
    println!(
        "=== last 3000 chars of output ===\n{}\n=== end tail ===",
        printable(&tail)
    );

    assert!(
        terminal.wait_for(" ready", Duration::from_secs(30)),
        "the studio never finished loading:\n{}",
        terminal.diagnostics()
    );

    // ---- the Keybinds page lists Alt+O (the user's second symptom) ----
    terminal.write(&[0x0f]); // Ctrl+O: the settings sheet
    std::thread::sleep(Duration::from_millis(600));
    // This fresh Studio opens Settings on its first page. Three Tabs land on Keybinds:
    // settings → appearance → mapping → keybinds. Do not infer the current page
    // from a diff-painted phrase: an unchanged cell may not be emitted by
    // ConPTY, which used to make this loop skip straight past Keybinds.
    for _ in 0..3 {
        terminal.write(b"\t");
        std::thread::sleep(Duration::from_millis(500));
    }
    // Walk by what the page shows rather than by Alt+O's position. More
    // rows may be added around it, and that must not turn this
    // terminal-input probe into a count of today's settings list.
    let rows = rustel_studio::keybinds::BindAction::ALL.len() + 3;
    // The input bytes are the same across platforms, but macOS displays
    // the Option symbol while Linux and Windows display Alt.
    let reveal_shortcut = if cfg!(target_os = "macos") {
        "⌥O"
    } else {
        "Alt+O"
    };
    let mut sample_file_row = selected_contains(&terminal.text(), reveal_shortcut);
    for _ in 0..rows {
        if sample_file_row {
            break;
        }
        terminal.write(&[0x1b, b'[', b'B']);
        std::thread::sleep(Duration::from_millis(60));
        sample_file_row = selected_contains(&terminal.text(), reveal_shortcut);
    }
    assert!(
        sample_file_row,
        "the keybinds page never put the selector on the {reveal_shortcut} row:\n{}",
        terminal.diagnostics()
    );
    // Enter on the row arms a learn, as on every other row.
    terminal.write(b"\r");
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        contains_plain(&terminal.text(), "press the chord"),
        "Enter on the Alt+O row did not arm a learn:\n{}",
        terminal.diagnostics()
    );
    terminal.write(&[0x1b]); // Esc: the learn is called off
    std::thread::sleep(Duration::from_millis(400));
    terminal.write(&[0x1b]); // Esc: the sheet goes
    std::thread::sleep(Duration::from_millis(400));

    // ---- Alt+O on the samples tab, as Windows Terminal delivers it ----
    let mark = terminal.buffer.lock().unwrap().len();
    terminal.write(&[0x06]); // Ctrl+F opens the reference
    // Fresh bytes only: the sheet painted "search:" into the buffer already,
    // so a whole-buffer needle would match the old frame before Esc landed.
    assert!(
        terminal.wait_for_since(mark, "search:", Duration::from_secs(10)),
        "the reference browser never opened:\n{}",
        terminal.diagnostics()
    );
    terminal.write(b"\t"); // the samples tab
    // The count row may retain letters from the reference tab. Reconstruct
    // the full screen: fresh bytes alone can contain only "ounds".
    assert!(
        terminal.wait_for_screen("sounds", Duration::from_secs(10)),
        "the samples tab never opened:\n{}",
        terminal.diagnostics()
    );
    let mark = terminal.buffer.lock().unwrap().len();
    // Search for the local fixture rather than relying on a remote pack's
    // manifest being ready. The `^⌫ clears` hint witnesses the new search.
    terminal.write(b"pty_probe");
    assert!(
        terminal.wait_for_since(mark, "clears", Duration::from_secs(10)),
        "the search never filtered:\n{}",
        terminal.diagnostics()
    );
    let selected_by = Instant::now() + Duration::from_secs(30);
    while !selected_contains(&terminal.text(), "pty_probe") && Instant::now() < selected_by {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        selected_contains(&terminal.text(), "pty_probe"),
        "the local sample never became the selected row:\n{}",
        terminal.diagnostics()
    );

    // Alt+O as Windows Terminal delivers it: ESC o. The studio is pinned
    // headless, so the status reads `opening <url>` and the next poll turns
    // it into `opened <url>`. A clipboard or missing-desktop message means
    // the real spawn path ran. A bare `nowhere` means the chord was not
    // dispatched.
    terminal.write(&[0x1b, b'o']);
    std::thread::sleep(Duration::from_millis(1500));
    let after = terminal.text()[mark..].to_owned();
    assert!(
        contains_plain(&after, "opened"),
        "Alt+O on the local sample never reached `opened` under the headless pin:\n{}",
        terminal.diagnostics()
    );
    assert!(
        !after.contains("no desktop") && !after.contains("copied"),
        "the reveal fell back to the clipboard: the headless pin leaked:\n{}",
        terminal.diagnostics()
    );

    // Alt+A as Windows Terminal delivers it: ESC a (sanity: the other Alt
    // chord, proven in the same encoding). The status line is the witness:
    // the footer shows the shortcut whether or not it is on.
    let mark = terminal.buffer.lock().unwrap().len();
    terminal.write(&[0x1b, b'a']);
    std::thread::sleep(Duration::from_millis(800));
    let frame = terminal.text();
    let after = &frame[mark..];
    assert!(
        after.contains("auto-play on"),
        "Alt+A produced no reaction:\n{}",
        terminal.diagnostics()
    );

    // And Escape to leave, then Ctrl+Q twice to quit.
    terminal.write(&[0x1b]);
    std::thread::sleep(Duration::from_millis(200));
    terminal.write(&[0x11]);
    std::thread::sleep(Duration::from_millis(200));
    terminal.write(&[0x11]);
    std::thread::sleep(Duration::from_millis(500));
    println!("=== probe done ===");
}

/// Alt with a digit selects that strip's score, in both spellings a
/// terminal delivers: the legacy ESC prefix, as Windows Terminal and a
/// plain xterm send it, and the kitty protocol's CSI u, as Ghostty and
/// kitty send it once the studio asks for disambiguation. Both must reach
/// the same chord: the status line names the scene each digit selected.
#[test]
fn alt_digits_select_strip_scores_in_both_wire_spellings() {
    let Some(binary) = rustel_binary() else {
        panic!("build the binary first");
    };
    let home = tempfile::tempdir().expect("temporary home");
    std::fs::write(
        home.path().join("live.strudel"),
        "$: note(\"c4\").s(\"sine\")\n",
    )
    .expect("starter score");
    let mut terminal = Terminal::open(&binary, 24, 80, home.path());
    assert!(
        terminal.wait_for(" ready", Duration::from_secs(30)),
        "the studio never finished loading:\n{}",
        terminal.diagnostics()
    );

    // A second score on the strip, so the digits have somewhere to go.
    let mark = terminal.buffer.lock().unwrap().len();
    terminal.write(&[0x0e]); // Ctrl+N: a new scene, written beside the set
    assert!(
        terminal.wait_for_since(mark, "scene 2 - ", Duration::from_secs(10)),
        "Ctrl+N never put a second score on the strip:\n{}",
        terminal.diagnostics()
    );

    // Alt+1 as a legacy terminal sends it: ESC then the key in one chunk,
    // which the input layer reads as one Alt+1 event. The first score,
    // named `live` by its file, comes back.
    terminal.write(&[0x1b, b'1']);
    assert!(
        terminal.wait_for_screen("scene 1 - live", Duration::from_secs(10)),
        "the ESC-prefixed Alt+1 never selected the first score:\n{}",
        terminal.diagnostics()
    );

    // Alt+2 as a kitty-protocol terminal sends it: CSI u with the base
    // key's codepoint and the Alt modifier bit. Windows ConPTY does not
    // carry the kitty spelling, so a Unix PTY alone drives this half.
    #[cfg(unix)]
    {
        terminal.write(b"\x1b[50;3u");
        assert!(
            terminal.wait_for_screen("scene 2 - ", Duration::from_secs(10)),
            "the CSI u Alt+2 never selected the second score:\n{}",
            terminal.diagnostics()
        );
    }

    // A digit the strip does not hold says so, through the legacy spelling:
    // the chord reached the strip and the strip answered.
    terminal.write(&[0x1b, b'9']);
    assert!(
        terminal.wait_for_screen("no scene 9 on the strip", Duration::from_secs(10)),
        "the ESC-prefixed Alt+9 never reached the strip:\n{}",
        terminal.diagnostics()
    );
}

/// A negotiated keyboard delivers the Alt digit the way kitty and Ghostty
/// do once the studio asks for the protocol: the Option press and release
/// bracket the digit, which arrives as CSI u with the base key's codepoint
/// and the Alt bit, whatever the layout composes Option into. A plain PTY
/// cannot answer the handshake, so the harness answers it as the terminals
/// do, and the studio negotiates and pushes its flags for real.
#[cfg(unix)]
#[test]
fn negotiated_keyboards_option_digit_selects_the_strip_score() {
    let Some(binary) = rustel_binary() else {
        panic!("build the binary first");
    };
    let home = tempfile::tempdir().expect("temporary home");
    std::fs::write(
        home.path().join("live.strudel"),
        "$: note(\"c4\").s(\"sine\")\n",
    )
    .expect("starter score");
    let mut terminal = Terminal::open_negotiating(&binary, 24, 80, home.path(), "ghostty");
    assert!(
        terminal.wait_for(" ready", Duration::from_secs(30)),
        "the studio never finished loading:\n{}",
        terminal.diagnostics()
    );
    // A second score, now the selected one, so the digit has a selection
    // to make.
    terminal.write(&[0x0e]); // Ctrl+N: a new scene, written beside the set
    assert!(
        terminal.wait_for_screen("scene 2 - ", Duration::from_secs(10)),
        "Ctrl+N never put a second score on the strip:\n{}",
        terminal.diagnostics()
    );
    // The full negotiated stream for Option+1: the Option press (report
    // all keys sends modifier presses too), then the digit with the Alt
    // bit, then the release. The release carries an event type the
    // studio never asked for, so its presence also proves a stray
    // release cannot swallow the digit that follows.
    terminal.write(b"\x1b[57443;1u\x1b[49;3u\x1b[57443;1:3u");
    assert!(
        terminal.wait_for_screen("scene 1 - live", Duration::from_secs(10)),
        "the negotiated Option+1 never selected the first score:\n{}",
        terminal.diagnostics()
    );
    // The same stream for Option+2, back the other way: the chord works
    // while the flags stand, not only on the first press.
    terminal.write(b"\x1b[57443;1u\x1b[50;3u\x1b[57443;1:3u");
    assert!(
        terminal.wait_for_screen("scene 2 - ", Duration::from_secs(10)),
        "the negotiated Option+2 never selected the second score:\n{}",
        terminal.diagnostics()
    );
}

/// On iTerm2 with Option as Normal, Alt+1 never reaches the studio as a
/// chord: the terminal composes the key into text and sends the bytes of
/// `¡`. The studio's recovery table turns it back into the chord, so the
/// strip still answers - the third delivery path, proven under the
/// identity that produces it.
///
/// Windows ConPTY does not deliver a composed Option key as text the
/// studio can read. The recovery table has unit coverage in the
/// terminal profile, the keybinds and the app's key dispatch.
#[cfg(unix)]
#[test]
fn iterm2s_composed_digit_selects_the_strip_score() {
    let Some(binary) = rustel_binary() else {
        panic!("build the binary first");
    };
    let home = tempfile::tempdir().expect("temporary home");
    std::fs::write(
        home.path().join("live.strudel"),
        "$: note(\"c4\").s(\"sine\")\n",
    )
    .expect("starter score");
    let mut terminal = Terminal::open_reported_as(&binary, 24, 80, home.path(), Some("iTerm.app"));
    assert!(
        terminal.wait_for(" ready", Duration::from_secs(30)),
        "the studio never finished loading:\n{}",
        terminal.diagnostics()
    );
    // A second score, so the digit has a selection to make: the studio
    // does not repaint a scene the strip already shows.
    let mark = terminal.buffer.lock().unwrap().len();
    terminal.write(&[0x0e]); // Ctrl+N: a new scene, written beside the set
    assert!(
        terminal.wait_for_since(mark, "scene 2 - ", Duration::from_secs(10)),
        "Ctrl+N never put a second score on the strip:\n{}",
        terminal.diagnostics()
    );
    terminal.write("¡".as_bytes());
    assert!(
        terminal.wait_for_screen("scene 1 - live", Duration::from_secs(10)),
        "the composed Option+1 never selected the first score:\n{}",
        terminal.diagnostics()
    );
}

/// The composition a protocol terminal reports beside the Alt digit must
/// not swallow the chord. Ghostty and kitty push the digit with the Alt
/// bit and the composed character as text; the exotic composition gives
/// up the chord, the way the US Option+1 spelling ¡ does.
#[test]
#[cfg(unix)]
fn a_negotiated_exotic_composition_selects_the_strip_score() {
    let Some(binary) = rustel_binary() else {
        panic!("build the binary first");
    };
    let home = tempfile::tempdir().expect("temporary home");
    std::fs::write(
        home.path().join("live.strudel"),
        "$: note(\"c4\").s(\"sine\")\n",
    )
    .expect("starter score");
    let mut terminal = Terminal::open_negotiating(&binary, 24, 80, home.path(), "ghostty");
    assert!(
        terminal.wait_for(" ready", Duration::from_secs(30)),
        "the studio never finished loading:\n{}",
        terminal.diagnostics()
    );
    terminal.write(&[0x0e, 0x0e]);
    assert!(
        terminal.wait_for_screen("scene 3 - ", Duration::from_secs(10)),
        "Ctrl+N twice never put a third score on the strip:\n{}",
        terminal.diagnostics()
    );
    // The stream a real Ghostty sends under the studio's five flags:
    // the Option press, then the digit with the Alt bit and the composed
    // text as the last sub-param, then the releases.
    terminal.write(b"\x1b[57443;3:1u\x1b[49;3:1;161u\x1b[49;3:3;161u\x1b[57443;3:3u");
    assert!(
        terminal.wait_for_screen("scene 1 - live", Duration::from_secs(10)),
        "the composed Option+1 never selected the first score:\n{}",
        terminal.diagnostics()
    );
}

/// A plain ASCII composition is real typing on some layouts - Option+2 is
/// the one way to type @ in Swedish - so under the same flags it types
/// into the score and moves nothing on the strip.
#[test]
#[cfg(unix)]
fn a_negotiated_plain_composition_types_instead_of_selecting() {
    let Some(binary) = rustel_binary() else {
        panic!("build the binary first");
    };
    let home = tempfile::tempdir().expect("temporary home");
    std::fs::write(
        home.path().join("live.strudel"),
        "$: note(\"c4\").s(\"sine\")\n",
    )
    .expect("starter score");
    let mut terminal = Terminal::open_negotiating(&binary, 24, 80, home.path(), "ghostty");
    assert!(
        terminal.wait_for(" ready", Duration::from_secs(30)),
        "the studio never finished loading:\n{}",
        terminal.diagnostics()
    );
    terminal.write(&[0x0e, 0x0e]);
    assert!(
        terminal.wait_for_screen("scene 3 - ", Duration::from_secs(10)),
        "Ctrl+N twice never put a third score on the strip:\n{}",
        terminal.diagnostics()
    );
    terminal.write(b"\x1b[57443;3:1u\x1b[50;3:1;64u\x1b[50;3:3;64u\x1b[57443;3:3u");
    assert!(
        terminal.wait_for_screen("@", Duration::from_secs(10)),
        "the composed Option+2 never typed its text:\n{}",
        terminal.diagnostics()
    );
    assert!(
        !terminal.text().contains("scene 2 - "),
        "the plain composition selected a strip score:\n{}",
        terminal.text()
    );
}
