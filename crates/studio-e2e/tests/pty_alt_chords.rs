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

fn rustel_binary() -> Option<std::path::PathBuf> {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let exe = if cfg!(windows) {
        "rustel.exe"
    } else {
        "rustel"
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
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    // The master must outlive the child: on Windows dropping it closes the
    // ConPTY and takes the child with it.
    _master: Box<dyn MasterPty + Send>,
}

impl Terminal {
    fn open(binary: &std::path::Path, rows: u16, cols: u16, cwd: &std::path::Path) -> Self {
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
        let child = pair
            .slave
            .spawn_command(command)
            .expect("the studio spawns on the pty");
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().expect("pty reader");
        let writer = pair.master.take_writer().expect("pty writer");
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let gathered = Arc::clone(&buffer);
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

    fn write(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("the pty takes input");
        self.writer.flush().expect("the pty flushes");
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
    // settings → advanced → mapping → keybinds. Do not infer the current page
    // from a diff-painted phrase: an unchanged cell may not be emitted by
    // ConPTY, which used to make this loop skip straight past Keybinds.
    for _ in 0..3 {
        terminal.write(b"\t");
        std::thread::sleep(Duration::from_millis(500));
    }
    // Walk by what the page shows rather than by Alt+O's position. More
    // rows may be added around it, and that must not turn this
    // terminal-input probe into a count of today's settings list.
    let rows = rustel_studio::keybinds::BindAction::ALL.len() + 2; // terminal profile and reset-all rows
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
    let mark = terminal.buffer.lock().unwrap().len();
    terminal.write(b"\t"); // the samples tab
    assert!(
        terminal.wait_for_since(mark, "sounds", Duration::from_secs(10)),
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
    // chord, proven in the same encoding).
    let mark = terminal.buffer.lock().unwrap().len();
    terminal.write(&[0x1b, b'a']);
    std::thread::sleep(Duration::from_millis(800));
    let frame = terminal.text();
    let after = &frame[mark..];
    assert!(
        after.contains("samples") && contains_plain(&frame, "auto-play"),
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
