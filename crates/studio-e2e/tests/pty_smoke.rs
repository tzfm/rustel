//! Run the default-feature rustel binary in a PTY. Check paint, paste, mouse,
//! resize and keyboard quit through terminal bytes.
//!
//! Run with `cargo test -p rustel-studio-e2e --features pty`.

#![cfg(feature = "pty")]

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

/// How long the studio may take to paint its first frame, and to leave
/// once quit: a debug-build binary warms its engine slowly, and the quit
/// joins the worker. Generous, because the failure these budgets exist
/// for is a hang, not a slow machine - the waits below poll, so a healthy
/// run finishes at the speed of the product, never at the budget's.
const BOOT_BUDGET: Duration = Duration::from_secs(30);
const REPAINT_BUDGET: Duration = Duration::from_secs(10);
const QUIT_BUDGET: Duration = Duration::from_secs(60);

/// A terminal's worth of studio output, gathered by a reader thread so a
/// blocking pty read can never stall the test: the thread drains, the
/// test polls what has accumulated.
struct Terminal {
    buffer: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
}

impl Terminal {
    fn open(binary: &std::path::Path, rows: u16, cols: u16, cwd: &std::path::Path) -> Self {
        Self::open_with_update_checks(binary, rows, cols, cwd, false)
    }

    fn open_with_update_checks(
        binary: &std::path::Path,
        rows: u16,
        cols: u16,
        cwd: &std::path::Path,
        check_updates: bool,
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
        // The score is passed the way the CLI documents it - a file opens
        // with its folder as the set. Nothing opens implicitly from the
        // working directory: with no argument the studio continues the set
        // open last time, which would make this smoke read a stranger's
        // session state.
        command.arg(cwd.join("live.strudel"));
        command.arg("--output");
        command.arg("silent");
        command.cwd(cwd);
        // The studio must meet a terminal that claims nothing: no theme,
        // no graphics answers, and a config home of its own - the
        // runner's real `studio.json` must not colour the smoke.
        command.env_remove("RUSTEL_THEME");
        command.env_remove("RUSTEL_THEME_DIR");
        command.env("RUSTEL_CONFIG_DIR", cwd);
        command.env(
            "RUSTEL_NO_UPDATE_CHECK",
            if check_updates { "" } else { "1" },
        );
        if check_updates {
            command.env_remove("CI");
        }
        // Headless: the smoke must never start a process the studio's
        // desktop commands would reach - no file manager, no browser - on
        // whatever machine runs the pty suite.
        command.env(rustel_studio::reveal::HEADLESS_ENV, "1");
        // No plugin of the runner either: an empty list stands in for the
        // standard VST3 folders, so a vst tab lists and loads no plugin.
        command.env(rustel_runtime::vst::FOLDERS_ENV, "");
        // `CommandBuilder::new` copies the whole parent environment, so
        // without this the studio reads the runner's terminal identity.
        // `studio/terminal.rs::identity()` picks a graphics tier and a
        // keyboard-capability set from these variables, so a runner started
        // from kitty, WezTerm or tmux would negotiate capabilities that a
        // hosted runner does not. The in-process fixture pins its identity
        // for the same reason.
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
            master: pair.master,
        }
    }

    /// The stream so far, safe to print: every escape sequence and control
    /// byte is spelled out (`ESC[2J`, `<BEL>`), so the reader's terminal
    /// executes nothing. A panic message with the raw capture would ring the
    /// bell and clear the terminal that runs the suite: ConPTY adds a
    /// BEL-terminated title sequence, and the studio clears the screen,
    /// enters the alternate screen and hides the cursor.
    fn diagnostics(&self) -> String {
        printable(&self.text())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.buffer.lock().expect("drain buffer")).into_owned()
    }

    /// Whether the needle (plain text, stripped of escapes) has appeared,
    /// polled until the deadline.
    fn wait_for(&mut self, needle: &str, budget: Duration) -> bool {
        let stop = Instant::now() + budget;
        while Instant::now() < stop {
            if contains_plain(&self.text(), needle) {
                return true;
            }
            if self.child.try_wait().expect("child polls").is_some() {
                // Exited early: the needle will never come.
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// More output since `mark` - the studio repainted into the pty.
    fn wait_for_more(&mut self, mark: usize, budget: Duration) -> bool {
        let stop = Instant::now() + budget;
        while Instant::now() < stop {
            if self.buffer.lock().expect("drain buffer").len() > mark {
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

    fn resize(&mut self, rows: u16, cols: u16) {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("the pty resizes");
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // A failing test must not leave a studio running on a pty nobody
        // reads.
        let _ = self.killer.kill();
        // `kill` only signals. Reap it too, or the child sits a zombie until
        // the test binary exits, the kill is never confirmed, and the
        // `TempDir` underneath is removed while it may still be writing.
        // Bounded rather than `wait()`: a `Drop` that blocks forever takes the
        // whole test binary with it, which is a worse failure than a zombie.
        for _ in 0..20 {
            match self.child.try_wait() {
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                _ => break,
            }
        }
    }
}

/// A directory that looks like a set: the score the studio opens when
/// named a folder, in a temporary home.
fn set_directory() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("temporary home");
    // A score that names no sample. `s("bd")` sends the real binary to the
    // sample CDN: a local bank must be registered as trusted through the
    // library, and the pty child has no harness to register one. This smoke
    // checks that the binary starts under a real terminal, paints, survives
    // a resize and quits. None of that needs a sample, and a synthesised
    // voice loads without the network.
    std::fs::write(
        home.path().join("live.strudel"),
        "$: note(\"c4 e4\").s(\"sine\")\n",
    )
    .expect("starter score");
    home
}

/// The frame is ANSI, and the app paints a diff: it moves the cursor between
/// fragments instead of repainting whole lines, so cursor moves can split a
/// message. This reader interprets cursor positioning as a terminal does,
/// so a needle is matched against the screen and not against the byte
/// stream.
fn contains_plain(frame: &str, needle: &str) -> bool {
    const COLS: usize = 120;
    const ROWS: usize = 40;
    let mut grid = vec![(' ', 0); COLS * ROWS];
    let (mut row, mut col): (usize, usize) = (0, 0);
    let mut characters = frame.chars().peekable();
    let mut plain = String::with_capacity(COLS * ROWS);
    let emit =
        |grid: &mut Vec<(char, usize)>, plain: &mut String, ch: char, row: usize, col: usize| {
            if row < ROWS && col < COLS {
                grid[row * COLS + col] = (ch, 0);
            }
            plain.push(ch);
        };
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            match characters.peek() {
                Some('[') => {
                    characters.next();
                    // Collect the sequence body up to its final byte.
                    let mut body = String::new();
                    while let Some(&next) = characters.peek() {
                        characters.next();
                        if next.is_ascii_alphabetic() {
                            body.push(next);
                            break;
                        }
                        body.push(next);
                    }
                    // CUP (H) and the row/col variants: the placement
                    // the diff painter actually uses.
                    if body.ends_with('H') {
                        let coords = body[..body.len() - 1]
                            .split(';')
                            .map(|part| part.parse::<usize>().unwrap_or(1).max(1) - 1);
                        let mut coords = coords.chain([0]);
                        row = coords.next().unwrap_or(0);
                        col = coords.next().unwrap_or(0);
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
                            // ConPTY ends OSC strings with the two-byte ST
                            // (ESC \): swallow the backslash too, or it lands
                            // in the grid as a printable cell and every
                            // column after it drifts by one.
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
        emit(&mut grid, &mut plain, character, row, col);
        col += 1;
    }
    // The needles may span a line the painter redrew (a diff repaints a
    // changed span, not the message's whole row), so both the placement-
    // reconstructed text and each row of the final screen are searched.
    if plain.contains(needle) {
        return true;
    }
    for row in 0..ROWS {
        let line: String = grid[row * COLS..(row + 1) * COLS]
            .iter()
            .map(|(ch, _)| ch)
            .collect();
        if line.contains(needle) {
            return true;
        }
    }
    false
}

/// Find visible score text by interpreting the cursor-positioning and erase
/// sequences in the output, so the mouse test clicks the painted source
/// instead of guessing a layout coordinate that changes with terminal size
/// or header content.
fn screen_position(frame: &str, needle: &str, rows: usize, cols: usize) -> Option<(u16, u16)> {
    let mut grid = vec![' '; rows * cols];
    let (mut row, mut col) = (0usize, 0usize);
    let mut chars = frame.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    let mut body = String::new();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        body.push(next);
                        if next.is_ascii_alphabetic() || next == '~' {
                            break;
                        }
                    }
                    let end = body.pop().unwrap_or(' ');
                    let parameters: Vec<usize> = body
                        .split(';')
                        .map(|part| part.parse::<usize>().unwrap_or(0))
                        .collect();
                    let first = parameters.first().copied().unwrap_or(0);
                    match end {
                        'H' => {
                            row = first.max(1) - 1;
                            col = parameters.get(1).copied().unwrap_or(1).max(1) - 1;
                        }
                        'K' => {
                            let (start, end) = match first {
                                0 => (col, cols),
                                1 => (0, col + 1),
                                _ => (0, cols),
                            };
                            if row < rows {
                                for x in start..end.min(cols) {
                                    grid[row * cols + x] = ' ';
                                }
                            }
                        }
                        'J' if first == 2 => grid.fill(' '),
                        'X' if row < rows => {
                            for x in col..(col + first.max(1)).min(cols) {
                                grid[row * cols + x] = ' ';
                            }
                        }
                        'C' => col += first.max(1),
                        _ => {}
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            let _ = chars.next(); // OSC string terminator: ESC \\
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        match ch {
            '\r' => col = 0,
            '\n' => row += 1,
            '\u{7}' => {}
            _ => {
                // A full-width line wraps on the next printable character:
                // ConPTY re-serializes the screen without explicit newlines
                // between rows and relies on the terminal's autowrap.
                if col >= cols {
                    col = 0;
                    row += 1;
                }
                if row < rows && col < cols {
                    grid[row * cols + col] = ch;
                }
                col += 1;
            }
        }
    }
    (0..rows).find_map(|y| {
        let line: String = grid[y * cols..(y + 1) * cols].iter().collect();
        line.find(needle).map(|x| (x as u16, y as u16))
    })
}

/// The update/save runs on a worker. Poll the file rather than waiting for a
/// status line that may be repainted before the PTY reader sees it.
fn wait_for_source(path: &std::path::Path, expected: &str, budget: Duration) -> bool {
    wait_for_source_any(path, &[expected], budget)
}

/// Like [`wait_for_source`], but the saved file may match any of `acceptable`.
fn wait_for_source_any(path: &std::path::Path, acceptable: &[&str], budget: Duration) -> bool {
    let stop = Instant::now() + budget;
    while Instant::now() < stop {
        if let Ok(source) = std::fs::read(path)
            && acceptable.iter().any(|text| source == text.as_bytes())
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Terminal paste framing and mixed line endings reach the real editor as
/// one paste, and update writes its normalized text to the scene on disk.
#[test]
fn bracketed_paste_normalizes_line_endings_in_the_saved_scene() {
    let binary = rustel_binary().expect("build `rustel` before the feature=pty suite");
    let home = set_directory();
    let path = home.path().join("live.strudel");
    let mut terminal = Terminal::open(&binary, 24, 80, home.path());
    assert!(
        terminal.wait_for(" ready", BOOT_BUDGET),
        "the studio did not load:\n{}",
        terminal.diagnostics()
    );

    // ^A selects the opened score; the CSI 200/201 frame is what a terminal
    // actually sends when bracketed paste is enabled. A mixed CRLF/bare-CR/LF
    // payload must become one edit, rather than multiple Enter shortcuts.
    terminal.write(&[0x01]);
    let payload = b"\x1b[200~$: note(\"c4\").s(\"sine\")\r\n// second\r// third\n\x1b[201~";
    let paste_started = Instant::now();
    terminal.write(payload);
    assert!(
        terminal.wait_for("third", REPAINT_BUDGET),
        "the paste never reached the editor:\n{}",
        terminal.diagnostics()
    );
    eprintln!(
        "pty_timing operation=paste_to_visible_output elapsed_ms={:.3} bytes={} rows=24 cols=80",
        paste_started.elapsed().as_secs_f64() * 1000.0,
        payload.len(),
    );
    terminal.write(&[0x13]); // ^S: update and save the active scene.
    let expected = "$: note(\"c4\").s(\"sine\")\n// second\n// third\n";
    // On Windows, ConPTY re-serializes input and can drop an LF directly
    // before the paste-end marker. The saved scene then keeps every pasted
    // line but may lack the final newline. POSIX PTYs deliver every byte.
    let acceptable: &[&str] = if cfg!(windows) {
        &[expected, expected.trim_end_matches('\n')]
    } else {
        &[expected]
    };
    assert!(
        wait_for_source_any(&path, acceptable, REPAINT_BUDGET),
        "the saved scene differs from the pasted source; got {:?}; terminal:\n{}",
        std::fs::read_to_string(&path),
        terminal.diagnostics()
    );
}

/// Raw SGR pointer reports must bring keyboard focus back from the set panel
/// and place the caret in the visible editor line. Focus in/out reports are
/// part of the same real terminal stream.
#[test]
fn mouse_click_restores_editor_focus_and_places_the_caret() {
    let binary = rustel_binary().expect("build `rustel` before the feature=pty suite");
    let home = set_directory();
    let path = home.path().join("live.strudel");
    let mut terminal = Terminal::open(&binary, 24, 80, home.path());
    assert!(
        terminal.wait_for(" ready", BOOT_BUDGET),
        "the studio did not load:\n{}",
        terminal.diagnostics()
    );
    let source = "note(\"c4 e4\").s(\"sine\")";
    assert!(
        terminal.wait_for(source, REPAINT_BUDGET),
        "the source was not painted:\n{}",
        terminal.diagnostics()
    );
    terminal.write(&[0x02]); // ^B: show and focus the set panel.
    assert!(
        terminal.wait_for("Enter open · n new", REPAINT_BUDGET),
        "the set panel was not painted:\n{}",
        terminal.diagnostics()
    );
    let (x, y) = screen_position(&terminal.text(), source, 24, 80)
        .unwrap_or_else(|| panic!("the source is not visible:\n{}", terminal.diagnostics()));
    terminal.write(b"\x1b[O\x1b[I"); // terminal focus lost, then regained.
    terminal.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
    terminal.write(b" ");
    terminal.write(&[0x13]); // ^S
    assert!(
        wait_for_source(&path, "$:  note(\"c4 e4\").s(\"sine\")\n", REPAINT_BUDGET),
        "mouse click did not place the caret in the score; got {:?}; terminal:\n{}",
        std::fs::read_to_string(&path),
        terminal.diagnostics()
    );

    // Two presses on the same painted word select it. Replacing `sine`
    // with another valid oscillator proves that the raw reports reached the
    // editor's multi-click selection path, not only its focus path.
    let (x, y) = screen_position(&terminal.text(), "\"sine\")", 24, 80).unwrap_or_else(|| {
        panic!(
            "the edited source is not visible:\n{}",
            terminal.diagnostics()
        )
    });
    let click = format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 2, y + 1, x + 2, y + 1);
    terminal.write(click.as_bytes());
    terminal.write(click.as_bytes());
    terminal.write(b"saw");
    terminal.write(&[0x13]);
    assert!(
        wait_for_source(&path, "$:  note(\"c4 e4\").s(\"saw\")\n", REPAINT_BUDGET),
        "double click did not select the oscillator name; got {:?}; terminal:\n{}",
        std::fs::read_to_string(&path),
        terminal.diagnostics()
    );
}

/// The product starts under a terminal, paints a frame, survives a
/// resize, and quits cleanly on Ctrl+Q twice - the whole smoke.
#[test]
fn the_real_binary_paints_resizes_and_quits_cleanly() {
    // A missing binary fails the test. libtest shows captured output only
    // on failure, so a skip would report a pass while the suite covers
    // nothing.
    let Some(binary) = rustel_binary() else {
        panic!(
            "the `rustel` binary is not built, so there is no product to drive. \
             Run `cargo build -p rustel` and try again."
        );
    };
    let home = set_directory();
    let startup_started = Instant::now();
    let mut terminal = Terminal::open(&binary, 24, 80, home.path());

    // The header names the studio, the selected scene row shows the score's
    // stem and the editor shows its content. The status line holds whichever
    // startup message came last, so these waits read the rest of the frame.
    assert!(
        terminal.wait_for("rustel", BOOT_BUDGET),
        "the header never reached the terminal; got:\n{}",
        terminal.diagnostics()
    );
    assert!(
        terminal.wait_for(" live", BOOT_BUDGET),
        "the header never named the opened set's scene; got:\n{}",
        terminal.diagnostics()
    );
    assert!(
        terminal.wait_for("s(\"sine\")", BOOT_BUDGET),
        "the editor never showed the opened score; got:\n{}",
        terminal.diagnostics()
    );
    // Ready, not still loading. This is the state the network dependency showed
    // up in, and holding it here means a future score that names a sample fails
    // on this line with the header in the message instead of hanging at the
    // quit below and reporting only a budget that ran out.
    assert!(
        terminal.wait_for(" ready", BOOT_BUDGET),
        "the studio never finished loading; a score that names a sample would \
         fetch it from the network here, and this smoke must not:\n{}",
        terminal.diagnostics()
    );

    eprintln!(
        "pty_timing operation=startup_to_ready_output elapsed_ms={:.3} rows=24 cols=80",
        startup_started.elapsed().as_secs_f64() * 1000.0,
    );

    // It survives a resize: the studio repaints into the new shape rather
    // than dying. The proof is output arriving after the resize mark
    // while the child is still alive.
    let mark = terminal.buffer.lock().expect("drain buffer").len();
    let resize_started = Instant::now();
    terminal.resize(30, 110);
    assert!(
        terminal.wait_for_more(mark, REPAINT_BUDGET),
        "no frame after the resize; the studio stopped painting or died"
    );
    eprintln!(
        "pty_timing operation=resize_to_output elapsed_ms={:.3} rows=30 cols=110",
        resize_started.elapsed().as_secs_f64() * 1000.0,
    );
    assert!(
        terminal.child.try_wait().expect("child polls").is_none(),
        "the studio did not survive the resize"
    );

    // ConPTY can omit unchanged cells, so match one new word rather than the
    // full quit prompt. Check absence first to avoid matching existing text.
    // Reconstruct from raw positioned output; scrub only the diagnostic so
    // captured escape sequences cannot execute on the reader's terminal.
    let before = terminal.text();
    assert!(
        !contains_plain(&before, "quit"),
        "`quit` is already on screen before any key is pressed, so waiting for \
         it afterwards would prove nothing:\n{}",
        printable(&before)
    );

    // The first Ctrl+Q arms and says so, and does not leave.
    terminal.write(&[0x11]);
    assert!(
        terminal.wait_for("quit", REPAINT_BUDGET),
        "the first Ctrl+Q did not arm the quit; got:\n{}",
        terminal.diagnostics()
    );
    assert!(
        terminal.child.try_wait().expect("child polls").is_none(),
        "one Ctrl+Q left outright; the studio is meant to ask twice"
    );

    // The second Ctrl+Q exits cleanly. Assert the budget before `wait()`:
    // `wait()` blocks until the child exits, so a studio that never quits
    // would hang here and report no diagnostic.
    terminal.write(&[0x11]);
    let stop = Instant::now() + QUIT_BUDGET;
    let exited = loop {
        if terminal.child.try_wait().expect("child polls").is_some() {
            break true;
        }
        if Instant::now() >= stop {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        exited,
        "the studio did not quit within {QUIT_BUDGET:?}; last screen:\n{}",
        terminal.diagnostics()
    );
    let status = terminal.child.wait().expect("child waits");
    assert!(
        status.success(),
        "a keyboard quit is a clean exit: {status:?}"
    );
}

#[test]
fn release_notice_appears_once_after_leaving_the_studio() {
    let binary = rustel_binary().expect("build `rustel` before the feature=pty suite");
    let home = set_directory();
    let cache = home.path().join("cache/update-check.json");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cached = serde_json::json!({"checked_at": now, "latest_version": "99.0.0"}).to_string();
    std::fs::write(&cache, &cached).unwrap();
    let mut terminal = Terminal::open_with_update_checks(&binary, 24, 80, home.path(), true);
    assert!(
        terminal.wait_for(" ready", BOOT_BUDGET),
        "the studio did not load:\n{}",
        terminal.diagnostics()
    );
    let active = terminal.text();
    assert!(!contains_plain(&active, "99.0.0"), "{}", printable(&active));
    assert!(
        !contains_plain(&active, "rustelup"),
        "{}",
        printable(&active)
    );

    terminal.write(&[0x11]);
    assert!(
        terminal.wait_for("quit", REPAINT_BUDGET),
        "the first Ctrl+Q did not arm the quit:\n{}",
        terminal.diagnostics()
    );
    assert!(terminal.child.try_wait().expect("child polls").is_none());
    terminal.write(&[0x11]);
    let notice = if cfg!(windows) {
        "Rustel 99.0.0 is available. Run rustelup.cmd to update."
    } else {
        "Rustel 99.0.0 is available. Run rustelup to update."
    };
    let deadline = Instant::now() + QUIT_BUDGET;
    let (output, status) = loop {
        let output = terminal.text();
        if let Some(status) = terminal.child.try_wait().expect("child polls")
            && contains_plain(&output, notice)
        {
            break (output, status);
        }
        assert!(
            Instant::now() < deadline,
            "the studio did not quit with an update notice:\n{}",
            terminal.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        status.success(),
        "a keyboard quit is a clean exit: {status:?}"
    );
    // ConPTY writes the screen again in its own bytes. The raw sequence and
    // the single copy of the notice hold only on a Unix pty.
    if !cfg!(windows) {
        let restored = output
            .rfind("\x1b[?1049l")
            .expect("the terminal left the alternate screen");
        assert!(
            !contains_plain(&output[..restored], "99.0.0"),
            "the release appeared before terminal restoration:\n{}",
            printable(&output)
        );
        assert!(contains_plain(&output[restored..], notice));
        assert_eq!(
            output.matches("99.0.0").count(),
            1,
            "{}",
            printable(&output)
        );
    }
    assert_eq!(std::fs::read_to_string(cache).unwrap(), cached);
}
