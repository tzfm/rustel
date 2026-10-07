//! What went on under the scene: the studio's log.
//!
//! The interface owns the whole terminal, so nothing the engine says can
//! scroll past the way it would in a shell. Every diagnostic, refused
//! update, save failure, device change and take is kept here - a ring in
//! memory for the panel, and `studio.log` in the sessions folder for the
//! morning after, whether or not anyone opened the panel.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use super::devices::draw_border;
use super::editor::{ByteOffset, Editor, GridRect, ScreenRow, Selection};
use super::engine::StudioDeviceInfo;
use super::theme::Theme;
use super::viz_panel::{BAND_DEFAULT_HEIGHT, BAND_MAX_HEIGHT, BAND_MIN_HEIGHT, Edge};
use rustel_runtime::{EnginePressureSnapshot, ProducerPhase};

/// Lines kept in memory for the panel.
const CAPACITY: usize = 2000;
pub const LOG_FILE_NAME: &str = "studio.log";

/// How much a line is worth interrupting a set for, quietest first.
///
/// `Debug` is the one that pays for a studio that says what it is doing.
/// Everything a musician wants to know about - a drop landing, a source
/// imported, a device changed - is `Info` or louder and is on screen by
/// default; the running commentary underneath it, which is what makes the
/// log worth reading when something has gone wrong, is `Debug` and waits
/// behind `v`. Both go to `studio.log` on disk either way: the toggle
/// chooses what is shown, never what is kept.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    /// How many there are, for a tally indexed by `level as usize`.
    /// Derived from the loudest, so adding one below it needs nothing
    /// here - and an index into the tally cannot run off the end.
    const COUNT: usize = Self::Error as usize + 1;

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info ",
            Self::Warn => "warn ",
            Self::Error => "error",
        }
    }
}

/// One `studio.log` record as the file holds it: the millisecond UTC stamp,
/// the process ID, level column, kind in brackets, and text on one line.
fn file_record(unix_millis: i64, level: Level, kind: &str, text: &str) -> String {
    format!(
        "{} pid {} {} [{kind}] {}\n",
        rustel_runtime::session_log::iso8601_utc_millis(unix_millis),
        std::process::id(),
        level.label(),
        text.replace('\n', " ⏎ ")
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Seconds since the studio opened - the clock the musician has.
    pub at: f64,
    pub level: Level,
    /// Which part spoke: `engine`, `update`, `check`, `save`, `device`, `take`.
    pub kind: String,
    pub text: String,
    /// An active header alert this line owns, until it is seen or resolved.
    alert: Option<String>,
    unseen: bool,
    /// The condition its alert names has cleared: the panel shows the line
    /// dim, with a ✓, and the header no longer counts it.
    pub resolved: bool,
    /// How many times this exact line was said in a row. A refusal of a
    /// sounding score is repeated every scheduling window on purpose - it is
    /// still news - and a row per window buried everything else under nine
    /// identical warnings a second. One row, counted, keeps the news and the
    /// rest of the log.
    pub repeats: u32,
}

pub struct StudioLog {
    lines: VecDeque<Line>,
    file: Option<File>,
    path: Option<PathBuf>,
    started: Instant,
    /// Warnings and errors since the panel was last open.
    unseen: usize,
    /// Moves whenever a line is resolved or raised again, so a panel
    /// notices a change that leaves the ring's length alone.
    resolutions: u64,
    /// How many lines are held at each level, kept as the ring changes.
    /// The sheet is sized to what it will show and has to know that before
    /// it has drawn anything to count.
    tally: [usize; Level::COUNT],
    /// Where standard error pointed before the studio moved it into the log,
    /// so shutdown can put it back. A returned `RuntimeError` must reach the
    /// shell, not disappear into `studio.log`.
    saved_stderr: Option<SavedStderr>,
}

/// The original standard-error destination, per platform.
#[cfg(unix)]
type SavedStderr = std::os::unix::io::RawFd;
#[cfg(windows)]
type SavedStderr = windows_sys::Win32::Foundation::HANDLE;
#[cfg(not(any(unix, windows)))]
type SavedStderr = ();

/// Duplicate the current standard error for a later restore.
#[cfg(unix)]
fn save_stderr() -> Option<SavedStderr> {
    // SAFETY: descriptor 2 is open; `dup` returns a new owned descriptor, or -1.
    let saved = unsafe { libc::dup(2) };
    (saved >= 0).then_some(saved)
}

/// Point standard error back at `saved` and release the saved descriptor.
#[cfg(unix)]
fn restore_stderr_to(saved: SavedStderr) -> bool {
    // SAFETY: both descriptors are owned by this process; `dup2` replaces fd 2
    // atomically. Close the duplicate only once that replacement succeeded -
    // otherwise it is the only remaining handle to the original destination.
    let restored = unsafe { libc::dup2(saved, 2) == 2 };
    if restored {
        unsafe {
            libc::close(saved);
        }
    }
    restored
}

/// Remember the handle `SetStdHandle` is about to replace.
#[cfg(windows)]
fn save_stderr() -> Option<SavedStderr> {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    let invalid = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    (!handle.is_null() && handle != invalid).then_some(handle)
}

/// Point standard error back at the saved handle.
#[cfg(windows)]
fn restore_stderr_to(saved: SavedStderr) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
    // The capture path duplicated the log file onto standard error. After
    // pointing the slot back at the original, that duplicate is ours to
    // close; leaving it open leaked a handle on every studio restart.
    let current = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    let restored = unsafe { SetStdHandle(STD_ERROR_HANDLE, saved) != 0 };
    if restored && !current.is_null() && current != INVALID_HANDLE_VALUE && current != saved {
        unsafe {
            CloseHandle(current);
        }
    }
    restored
}

#[cfg(not(any(unix, windows)))]
fn save_stderr() -> Option<SavedStderr> {
    None
}

#[cfg(not(any(unix, windows)))]
fn restore_stderr_to(_saved: SavedStderr) -> bool {
    false
}

impl StudioLog {
    /// Open the log, appending to `studio.log` in `directory` when there is
    /// one. A folder that cannot be written costs the file, not the log.
    pub fn open(directory: Option<&Path>) -> Self {
        let mut log = Self {
            lines: VecDeque::with_capacity(64),
            file: None,
            path: None,
            started: Instant::now(),
            unseen: 0,
            resolutions: 0,
            tally: [0; Level::COUNT],
            saved_stderr: None,
        };
        if let Some(directory) = directory {
            let path = directory.join(LOG_FILE_NAME);
            let opened = std::fs::create_dir_all(directory)
                .and_then(|()| OpenOptions::new().create(true).append(true).open(&path));
            match opened {
                Ok(file) => {
                    log.file = Some(file);
                    log.path = Some(path);
                }
                Err(error) => {
                    log.push(
                        Level::Warn,
                        "log",
                        format!("not writing {}: {error}", path.display()),
                    );
                }
            }
        }
        log.push(
            Level::Info,
            "studio",
            format!("opened - {}", rustel_runtime::product::engine_identity()),
        );
        log
    }

    pub fn push(&mut self, level: Level, kind: &str, text: impl Into<String>) {
        self.push_record(level, kind, text.into(), None);
    }

    /// [`Self::push`], or [`Self::push_alert`] when `alert` names one.
    pub fn push_with(
        &mut self,
        level: Level,
        kind: &str,
        text: impl Into<String>,
        alert: Option<String>,
    ) {
        self.push_record(level, kind, text.into(), alert);
    }

    /// Keep a warning in the log while giving its header badge an identity
    /// that the condition can resolve later.
    pub fn push_alert(
        &mut self,
        level: Level,
        kind: &str,
        text: impl Into<String>,
        alert: impl Into<String>,
    ) {
        self.push_record(level, kind, text.into(), Some(alert.into()));
    }

    fn push_record(&mut self, level: Level, kind: &str, text: String, alert: Option<String>) {
        let at = self.started.elapsed().as_secs_f64();
        if let Some(file) = self.file.as_mut() {
            // Whole seconds cannot separate a pad press from the install
            // it asked for: every line of a one-second burst shares a stamp.
            // Millis make the log a stopwatch.
            let unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_millis() as i64)
                .unwrap_or(0);
            // One record is one formatted string and one write: the studio's
            // own records can no longer splice into each other mid-line.
            // (Third-party writers hold their own file positions; nothing
            // here can promise what they do with them.)
            let line = file_record(unix, level, kind, &text);
            let _ = file.write_all(line.as_bytes());
        }
        let unseen = level >= Level::Warn;
        // The same line again, straight after itself: counted on the row it
        // already has. The file above still got its own record - the file
        // is the stopwatch, the panel is what a person reads.
        if let Some(last) = self.lines.back_mut()
            && last.level == level
            && last.kind == kind
            && last.text == text
            && last.alert == alert
        {
            last.repeats = last.repeats.saturating_add(1);
            last.at = at;
            // Said again after it resolved: the condition is back.
            if last.resolved {
                last.resolved = false;
                self.resolutions = self.resolutions.wrapping_add(1);
            }
            if unseen && !last.unseen {
                last.unseen = true;
                self.unseen = self.unseen.saturating_add(1);
            }
            return;
        }
        if unseen {
            self.unseen = self.unseen.saturating_add(1);
        }
        if self.lines.len() >= CAPACITY
            && let Some(dropped) = self.lines.pop_front()
        {
            self.tally[dropped.level as usize] -= 1;
            if dropped.unseen {
                self.unseen = self.unseen.saturating_sub(1);
            }
        }
        self.tally[level as usize] += 1;
        self.lines.push_back(Line {
            at,
            level,
            kind: kind.to_owned(),
            text,
            alert,
            unseen,
            resolved: false,
            repeats: 1,
        });
    }

    pub fn lines(&self) -> &VecDeque<Line> {
        &self.lines
    }

    /// Lines held at `floor` or louder - what a panel filtering there
    /// would show. Counted as the log changes, so the sheet can be sized
    /// before it has been synced.
    pub fn at_least(&self, floor: Level) -> usize {
        self.tally
            .iter()
            .enumerate()
            .filter(|(level, _)| *level >= floor as usize)
            .map(|(_, held)| held)
            .sum()
    }

    /// Point the process's standard error at the log file for as long as
    /// the studio owns the terminal. While the alternate screen is up,
    /// stderr *is* the stage: one stray `eprintln!` from any thread - a
    /// render's refused voice, a library's complaint - scrolls the whole
    /// screen up a row. Here they land in `studio.log` instead, where a
    /// panic message is also found afterwards.
    pub fn capture_stderr(&mut self) -> bool {
        if self.saved_stderr.is_some() {
            // Already pointing at the log; a second save would dup the log
            // itself and leak the original destination.
            return true;
        }
        let Some(file) = self.file.as_ref() else {
            return false;
        };
        // Do not steal standard error unless we can put it back.
        let Some(saved) = save_stderr() else {
            return false;
        };
        if !redirect_stderr_to(file) {
            let _ = restore_stderr_to(saved);
            return false;
        }
        self.saved_stderr = Some(saved);
        true
    }

    /// Put standard error back where it was before [`Self::capture_stderr`],
    /// once the alternate screen is gone and returned errors are the shell's
    /// to print. Safe to call when nothing was captured.
    pub fn restore_stderr(&mut self) -> bool {
        let Some(saved) = self.saved_stderr else {
            return false;
        };
        if restore_stderr_to(saved) {
            self.saved_stderr = None;
            true
        } else {
            false
        }
    }

    /// A normal shutdown writes a marker, so a completed session and an
    /// interrupted one do not look identical in `studio.log`.
    pub fn session_end(&mut self) {
        self.session_end_marker(Level::Info, "session ended - clean shutdown");
    }

    /// Any session-finish marker - the clean one, or the crash one written
    /// on the way to the crash report - flushed so it is on disk even if
    /// the process goes down right after.
    pub fn session_end_marker(&mut self, level: Level, text: &str) {
        self.push(level, "studio", text);
        if let Some(file) = self.file.as_mut() {
            let _ = file.flush();
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Warnings and errors nobody has looked at yet.
    pub fn unseen(&self) -> usize {
        self.unseen
    }

    pub fn mark_seen(&mut self) {
        for line in &mut self.lines {
            line.unseen = false;
        }
        self.unseen = 0;
    }

    /// Mark every line raised under `alert` resolved, and take its badge
    /// off the header. The lines stay as history; unrelated warnings keep
    /// their badges. Answers how many lines this resolved: an unknown key,
    /// or one already resolved, resolves none.
    pub fn resolve_alert(&mut self, alert: &str) -> usize {
        let mut resolved = 0usize;
        for line in &mut self.lines {
            if !line.resolved && line.alert.as_deref() == Some(alert) {
                line.resolved = true;
                resolved += 1;
                if line.unseen {
                    line.unseen = false;
                    self.unseen = self.unseen.saturating_sub(1);
                }
            }
        }
        if resolved > 0 {
            self.resolutions = self.resolutions.wrapping_add(1);
        }
        resolved
    }

    /// Take the header badges raised under `alert` down without calling
    /// its lines resolved: a passing message that timed out or gave way,
    /// whose cause may still stand. Answers how many badges came down.
    pub fn retire_alert(&mut self, alert: &str) -> usize {
        let mut retired = 0usize;
        for line in &mut self.lines {
            if line.unseen && line.alert.as_deref() == Some(alert) {
                line.unseen = false;
                retired += 1;
            }
        }
        self.unseen = self.unseen.saturating_sub(retired);
        retired
    }
}

impl Drop for StudioLog {
    fn drop(&mut self) {
        // A panic between capture and the explicit restore must not leave
        // descriptor 2 (or the Windows std-error slot) aimed at a file that
        // is about to close with us.
        let _ = self.restore_stderr();
    }
}

#[cfg(unix)]
fn redirect_stderr_to(file: &File) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: both descriptors are open and owned by this process; dup2
    // replaces fd 2 atomically and leaves `file`'s own descriptor intact.
    unsafe { libc::dup2(file.as_raw_fd(), 2) == 2 }
}

/// Windows has no `dup2` for the handle that matters: `std::io::stderr`
/// asks `GetStdHandle(STD_ERROR_HANDLE)` on every write, and the panic hook
/// goes the same way, so redirecting means replacing that handle.
///
/// The file handle is duplicated first, so standard error holds a reference
/// of its own and dropping the log file cannot leave it pointing at a closed
/// handle - the guarantee `dup2` gives for free on unix. The duplicate is
/// closed when standard error is restored, not while it is the live slot.
///
/// Only Rust's standard error moves. A C dependency writing to the CRT's
/// own descriptor 2 keeps going where it always did.
#[cfg(windows)]
fn redirect_stderr_to(file: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut owned: HANDLE = std::ptr::null_mut();
    // SAFETY: the source handle is open and owned by this process, and the
    // destination is a live local. A failure leaves `owned` null and is
    // reported rather than used.
    let duplicated = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            file.as_raw_handle() as HANDLE,
            GetCurrentProcess(),
            &mut owned,
            0,
            // Not inheritable: a child process of ours has no business
            // writing into this studio's log.
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated == 0 {
        return false;
    }
    // SAFETY: `owned` is a handle this process owns. On success it becomes
    // standard error until restore closes it; on failure it is closed here
    // so the duplicate does not leak.
    let replaced = unsafe { SetStdHandle(STD_ERROR_HANDLE, owned) != 0 };
    if !replaced {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(owned);
        }
    }
    replaced
}

#[cfg(not(any(unix, windows)))]
fn redirect_stderr_to(_file: &File) -> bool {
    false
}

/// The open panel: the log's lines in an editor of their own, so they can
/// be selected, dragged through, scrolled with the wheel and copied like
/// the score - the text read-only, the caret unseen.
#[derive(Debug)]
pub struct LogPanel {
    pub editor: Editor,
    /// How much of the log the editor holds, so a changed log is noticed:
    /// its length, its newest line's clock, its resolutions, and what was
    /// being shown.
    seen: (usize, Option<u64>, u64, bool),
    /// Whether the view follows the newest line. Scrolling up lets go of
    /// it; End, or scrolling back to the end, takes it up again.
    pub following: bool,
    /// Whether the running commentary is shown as well as the events.
    /// Filtering happens here rather than at the source, so `v` is
    /// instant in both directions and nothing is ever thrown away.
    pub verbose: bool,
    /// The lines the editor is currently showing, in its own row order.
    /// The colouring reads this: with a filter on, an editor row and a
    /// place in the ring are no longer the same number.
    shown: Vec<Line>,
    /// ⇧F9: the log is docked, like a visuals dock or the mixer, and is
    /// not a sheet that other sheets displace and `dismiss_dialogs`
    /// closes. Off by default: the log is then a floating sheet.
    pub sticky: bool,
    /// Which edge a sticky log docks at: the top or the bottom, a band
    /// the width of the screen, as the mixer is. Meaningless while
    /// `sticky` is false, but kept regardless so turning sticky back on
    /// does not forget where it was.
    pub edge: Edge,
    /// The rows a sticky log's band was made with `-` and `+`; unset, it
    /// takes [`default_dock_height`] of whatever terminal it is on.
    pub height: Option<u16>,
}

/// The rows a docked log takes until it is resized: a third of the
/// terminal, and never fewer than a visuals band's default.
///
/// The band default of nine rows is too short for a log. The two borders
/// and the hint take three rows and the engine's pressure block takes five
/// while a set plays, which leaves one line of log. A third of a forty-row
/// terminal is thirteen rows: five lines of log while a set plays and ten
/// when the engine is stopped, and two thirds of the screen stay with the
/// score. The layout still gives the panes their minimum first, so on a
/// short terminal, or beside the mixer, the band takes what is left.
pub fn default_dock_height(frame_height: u16) -> u16 {
    (frame_height / 3).clamp(BAND_DEFAULT_HEIGHT, BAND_MAX_HEIGHT)
}

impl Default for LogPanel {
    fn default() -> Self {
        let mut editor = Editor::new("").expect("an empty editor");
        editor.set_wrap(true);
        Self {
            editor,
            seen: (0, None, 0, false),
            following: true,
            verbose: false,
            shown: Vec::new(),
            sticky: false,
            // Bottom, not the docks' own default of Right: a sticky log
            // starts where the sheet it replaces already sat.
            edge: Edge::Bottom,
            height: None,
        }
    }
}

/// One line of the log as the panel prints it: the clock, the level, the
/// part that spoke, the text on one line.
fn format_line(line: &Line) -> String {
    let (mut printed, _) = line_prefix(line);
    printed.push_str(&line.text.replace('\n', " ⏎ "));
    if line.repeats > 1 {
        let _ = write!(printed, "  ×{}", line.repeats);
    }
    if line.resolved {
        printed.push_str("  ✓");
    }
    printed
}

/// Format the timestamp, level and source, returning the display-cell columns
/// of the level, source and message. The columns are measured from this line's
/// printed prefix, so they stay correct when a wide source name or a timestamp
/// beyond 99 minutes shifts the later parts of that line.
fn line_prefix(line: &Line) -> (String, (usize, usize, usize)) {
    let minutes = (line.at / 60.0) as u64;
    let seconds = line.at % 60.0;
    let mut prefix = format!("{minutes:02}:{seconds:04.1} ");
    let level = UnicodeWidthStr::width(prefix.as_str());
    let _ = write!(prefix, "{:<5} ", line.level.label());
    let kind = UnicodeWidthStr::width(prefix.as_str());
    let _ = write!(prefix, "{:<8} ", line.kind);
    let text = UnicodeWidthStr::width(prefix.as_str());
    (prefix, (level, kind, text))
}

/// Where a printed line's parts begin: the level, the part, the text.
fn line_columns(line: &Line) -> (usize, usize, usize) {
    line_prefix(line).1
}

impl LogPanel {
    /// Bring the editor's text up to the log. The log is small, so the
    /// text is rebuilt; a selection survives when the log only grew, and
    /// the view stays where it was unless it follows the newest line.
    pub fn sync(&mut self, log: &StudioLog, rows: usize) {
        let lines = log.lines();
        let newest = lines.back().map(|line| line.at.to_bits());
        let seen = (lines.len(), newest, log.resolutions, self.verbose);
        if self.seen == seen {
            return;
        }
        self.seen = seen;
        let floor = self.floor();
        self.shown = lines
            .iter()
            .filter(|line| line.level >= floor)
            .cloned()
            .collect();
        let text = self
            .shown
            .iter()
            .map(format_line)
            .collect::<Vec<_>>()
            .join("\n");
        let before = self.editor.source();
        let grew = text.starts_with(&before)
            && (before.is_empty()
                || text.len() == before.len()
                || text.as_bytes()[before.len()] == b'\n');
        let selection = self.editor.primary_selection();
        let viewport = self.editor.viewport();
        let Ok(mut editor) = Editor::new(&text) else {
            return;
        };
        editor.set_wrap_indents(self.shown.iter().map(|line| line_columns(line).2).collect());
        editor.set_wrap(true);
        editor.set_view_size(viewport.page_columns, viewport.page_rows);
        if grew {
            let _ = editor.set_selection(selection);
        } else if !selection.is_empty()
            // Only a drop of whole lines from the front corresponds to a
            // pure offset shift. Anything else - the verbose filter
            // changed, formatting moved, lines landed mid-selection - has
            // no honest mapping, and the selection is let go.
            && Self::dropped_prefix_length(&before, &text).is_some_and(|prefix_end| {
                selection.anchor.0 >= prefix_end && selection.head.0 >= prefix_end
            })
        {
            let prefix_end = Self::dropped_prefix_length(&before, &text).unwrap_or(0);
            // The ring let its oldest lines go, so the old byte offsets
            // point somewhere else. Keeping the selection whole would lie
            // about what ^C copies, and dropping it turned a drag on a
            // busy log into "nothing selected" by the time ^C arrived. So
            // the selection moves with the text: each end slides back by
            // the prefix the ring dropped, clamped into what remains.
            let shift = |offset: usize| -> ByteOffset {
                let moved = offset.saturating_sub(prefix_end).min(text.len());
                let moved = if text.is_char_boundary(moved) {
                    moved
                } else {
                    0
                };
                ByteOffset(moved)
            };
            let _ = editor.set_selection(Selection::range(
                shift(selection.anchor.0.min(before.len())),
                shift(selection.head.0.min(before.len())),
            ));
        }
        editor.set_viewport(viewport);
        self.editor = editor;
        if self.following {
            self.follow(rows);
        }
    }

    /// Where the log's former text ends inside its own ring-evicted
    /// successor, when the successor is that text with whole lines
    /// dropped from the front: the byte length of the dropped prefix.
    /// `None` when anything else changed - there is no honest mapping of
    /// offsets onto the new text. The ring only ever drops from the
    /// front, so the first line boundary whose remainder the new text
    /// still starts with is the eviction; candidates longer than the
    /// successor itself cannot be, and are skipped without a compare.
    fn dropped_prefix_length(before: &str, text: &str) -> Option<usize> {
        if text.starts_with(before) {
            return None; // pure growth - `grew` already handled it
        }
        let mut search = before;
        loop {
            let index = search.find('\n')?;
            search = &search[index + 1..];
            // The remainder of `before` past the eviction is part of
            // `text` (plus what arrived), so it can never be longer.
            if search.len() <= text.len() && text.starts_with(search) {
                return Some(before.len() - search.len());
            }
        }
    }

    /// A fresh panel, showing what the player last chose to see.
    pub fn opened(verbose: bool) -> Self {
        Self {
            verbose,
            ..Self::default()
        }
    }

    /// The rows the docked band asks the layout for on a terminal
    /// `frame_height` rows tall: what `-` and `+` made it, or the default,
    /// within the bounds every band keeps.
    pub fn dock_height(&self, frame_height: u16) -> u16 {
        self.height.map_or_else(
            || default_dock_height(frame_height),
            |rows| rows.clamp(BAND_MIN_HEIGHT, BAND_MAX_HEIGHT),
        )
    }

    /// The quietest level being shown.
    pub fn floor(&self) -> Level {
        if self.verbose {
            Level::Debug
        } else {
            Level::Info
        }
    }

    /// Show the commentary as well, or put it away again. Returns what the
    /// panel is now showing.
    pub fn toggle_verbose(&mut self) -> bool {
        self.verbose = !self.verbose;
        self.verbose
    }

    /// The lines on screen, in the editor's row order.
    pub fn shown(&self) -> &[Line] {
        &self.shown
    }

    /// Lines the filter is holding back, for the panel to own up to.
    /// Read from the log rather than from the last sync, so it is right on
    /// the frame the panel opens.
    pub fn hidden(&self, log: &StudioLog) -> usize {
        log.lines().len().saturating_sub(log.at_least(self.floor()))
    }

    fn line_count(&self) -> usize {
        self.editor.visual_row_count()
    }

    fn set_top(&mut self, top_row: usize, rows: usize) {
        let mut viewport = self.editor.viewport();
        viewport.top_row = top_row.min(self.line_count().saturating_sub(rows));
        self.editor.set_viewport(viewport);
        self.note_view(rows);
    }

    /// Positive is up, into older lines.
    pub fn scroll(&mut self, delta: isize, rows: usize) {
        let top = self.editor.viewport().top_row as isize - delta;
        self.set_top(top.max(0) as usize, rows);
    }

    /// The newest lines, and following them from here on.
    pub fn follow(&mut self, rows: usize) {
        self.set_top(self.line_count().saturating_sub(rows), rows);
        self.following = true;
    }

    pub fn top(&mut self, rows: usize) {
        self.set_top(0, rows);
    }

    /// After the view moved by other means - the wheel, a drag past the
    /// edge - whether it is at the end, and so follows.
    pub fn note_view(&mut self, rows: usize) {
        self.following = self.editor.viewport().top_row + rows >= self.line_count();
    }

    /// Size the view to its list and bring the text up to the log, before
    /// the frame is drawn - drawing then needs nothing mutable. The size
    /// comes first: following the newest line is measured in rows.
    pub fn prepare(&mut self, log: &StudioLog, list: Rect) {
        let rows = usize::from(list.height);
        self.editor.set_view_size(usize::from(list.width), rows);
        self.sync(log, rows);
        // A resize can change both wrapping and how much of the tail fits
        // even when the log itself did not change. Following is a view
        // promise, so honour it after every size update as well as a sync.
        if self.following {
            self.follow(rows);
        }
    }

    /// Lines below the window.
    pub fn more_below(&self, rows: usize) -> usize {
        self.line_count()
            .saturating_sub(self.editor.viewport().top_row + rows)
    }
}

/// What the sheet has to show, which is what decides how tall it is.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogExtent {
    /// Rows the engine's pressure block will print.
    pub details: u16,
    /// Lines the panel is showing, after its filter.
    pub lines: u16,
    /// The panel is stuck to an edge, so the room it was handed IS its
    /// room and it fills it.
    ///
    /// It travels with the extent because every site that asks the log
    /// where it is - drawing, scrolling, hit-testing - already passes one,
    /// so the answer cannot be given in one place and forgotten in
    /// another.
    pub docked: bool,
}

impl LogExtent {
    pub fn of(
        log: &StudioLog,
        panel: &LogPanel,
        pressure: Option<&EnginePressureSnapshot>,
    ) -> Self {
        let physical_lines = log.at_least(panel.floor());
        // Once the panel has synced, its editor knows how many terminal rows
        // wrapping actually consumes. Size the sheet from that rather than
        // hiding older entries whenever one long line wraps. Before the first
        // sync, the physical count gives `prepare` enough room to build the
        // map; the render that follows observes the visual count.
        let visual_lines = if panel.editor.source().is_empty() {
            0
        } else {
            panel.editor.visual_row_count()
        };
        Self {
            details: LogPanelView::detail_rows(pressure),
            lines: u16::try_from(physical_lines.max(visual_lines)).unwrap_or(u16::MAX),
            // Whether the layout found it a room is the app's question, not
            // the log's, so this starts false and `log_extent` says.
            docked: false,
        }
    }
}

pub struct LogPanelView<'a> {
    pub keybinds: &'a super::keybinds::Keybinds,
    pub panel: &'a LogPanel,
    pub log: &'a StudioLog,
    pub theme: &'a Theme,
    pub pressure: Option<&'a EnginePressureSnapshot>,
    pub device: Option<&'a StudioDeviceInfo>,
    pub memory: Option<&'a super::memory::MemoryFigures>,
    /// The layout reserved a room for the sticky log and `area` is that
    /// room, so the panel fills it. When false, the view sizes itself as a
    /// sheet.
    pub docked: bool,
}

impl LogPanelView<'_> {
    /// Rows the engine's pressure block prints while a set is running.
    const DETAIL_ROWS: u16 = 5;

    /// How many rows to keep for the block above the log.
    ///
    /// Five while the engine runs. With the engine stopped there is no
    /// pressure to report, and the header and the memory breakdown already
    /// show the process's cpu and memory, so the block keeps no row and the
    /// log starts at its first entry.
    pub fn detail_rows(pressure: Option<&EnginePressureSnapshot>) -> u16 {
        if pressure.is_some() {
            Self::DETAIL_ROWS
        } else {
            0
        }
    }

    /// A sheet across the bottom, under half the screen: the score stays
    /// in view above it.
    ///
    /// The sheet is as tall as its contents and no taller, up to its cap,
    /// so a short log leaves no blank rows above the hint. It is an
    /// overlay: when it grows, nothing else moves.
    pub fn geometry(available: Rect, extent: LogExtent) -> Option<(Rect, Rect)> {
        // Docked, it fills the room the layout reserved and nothing else:
        // no half-screen cap, and no minimum width borrowed from a sheet.
        // The sheet's rules return `None` below 40 columns or 8 rows. They
        // also cap the height and anchor the sheet at the bottom, which
        // leaves part of a tall room blank.
        if extent.docked {
            if available.height < 3 || available.width < 8 {
                return None;
            }
            let list = Rect::new(
                available.x + 2,
                available.y + 1,
                available.width.saturating_sub(4),
                available.height.saturating_sub(3),
            );
            return Some((available, list));
        }
        let cap = (available.height / 2).clamp(6, 18);
        // Border, hint, border: the three rows that are never the list.
        let wanted = extent
            .details
            .saturating_add(extent.lines)
            .saturating_add(3);
        // Never so short that the pressure block cannot be drawn beside a
        // line of log: a set in trouble is exactly when that block is worth
        // reading, and the log being empty is no reason to hide it. Half
        // the screen still wins on a terminal too small for both.
        let floor = extent.details.saturating_add(4);
        let height = wanted.clamp(floor.min(cap), cap);
        if available.height < 8 || available.width < 40 {
            return None;
        }
        let area = Rect::new(
            available.x + 1,
            available.bottom().saturating_sub(height),
            available.width.saturating_sub(2),
            height,
        );
        let list = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height.saturating_sub(3),
        );
        Some((area, list))
    }

    /// Rows the list shows, for scrolling.
    pub fn rows(available: Rect, extent: LogExtent) -> usize {
        Self::list_area(available, extent)
            .map(|list| usize::from(list.height))
            .unwrap_or(0)
    }

    /// Where the log's lines are, for the pointer.
    pub fn list_area(available: Rect, extent: LogExtent) -> Option<Rect> {
        Self::geometry(available, extent).map(|(panel, list)| {
            let (_, list) = Self::split_for_memory(panel, list);
            Self::log_list(list, extent.details)
        })
    }

    /// Share the log's bordered panel: memory gets 30% at the right and
    /// the log keeps the left 70%. All log mapping and pointer code uses this
    /// same split, so the memory column never selects a log entry.
    fn split_for_memory(panel: Rect, list: Rect) -> (Rect, Rect) {
        let divider = panel.x + panel.width * 7 / 10;
        let memory = Rect::new(
            divider + 2,
            panel.y + 1,
            panel.right().saturating_sub(divider + 4),
            panel.height.saturating_sub(2),
        );
        let log = Rect::new(
            panel.x + 2,
            list.y,
            divider.saturating_sub(panel.x + 3),
            list.height,
        );
        (memory, log)
    }

    /// What this view is showing, for its own geometry. Counted from the
    /// log rather than from the panel's last sync, so drawing, scrolling
    /// and hit-testing agree on the frame the sheet opens - before
    /// anything has been synced into it.
    fn extent(&self) -> LogExtent {
        LogExtent {
            docked: self.docked,
            ..LogExtent::of(self.log, self.panel, self.pressure)
        }
    }

    fn log_list(mut list: Rect, details: u16) -> Rect {
        // Room for the block and a line of log. The sheet is sized to fit
        // both, so this only bites on a terminal too small for either.
        if list.height > details {
            list.y = list.y.saturating_add(details);
            list.height = list.height.saturating_sub(details);
        }
        list
    }
}

impl Widget for LogPanelView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let Some((panel, full_list)) = Self::geometry(area, self.extent()) else {
            return;
        };
        let (memory_area, list) = Self::split_for_memory(panel, full_list);
        let theme = self.theme;
        super::view::clear_overlay(
            buffer,
            panel,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        draw_border(buffer, panel, theme);
        let divider = list.right() + 1;
        for y in panel.y + 1..panel.bottom().saturating_sub(1) {
            buffer.set_string(divider, y, "│", Style::default().fg(theme.rule));
        }
        buffer.set_stringn(
            memory_area.x,
            panel.y,
            " mem ",
            usize::from(memory_area.width),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        );
        if let Some(figures) = self.memory {
            super::memory::MemorySidecarView { figures, theme }.render(memory_area, buffer);
        }
        let title = match self.log.path() {
            Some(path) => format!(" log - {} ", path.display()),
            None => " log - not written to disk ".to_owned(),
        };
        buffer.set_stringn(
            list.x,
            panel.y,
            &title,
            usize::from(list.width),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        );
        let log_list = Self::log_list(list, self.extent().details);
        if log_list.y > list.y {
            render_pressure_details(
                buffer,
                Rect::new(list.x, list.y, list.width, log_list.y - list.y),
                self.pressure,
                self.device,
                theme,
            );
        }
        let lines = self.panel.shown();
        let rows = usize::from(log_list.height);
        if log_list.is_empty() {
            return;
        }
        let editor = &self.panel.editor;
        let grid = GridRect::new(log_list.x, log_list.y, log_list.width, log_list.height);
        let Ok(map) = editor.screen_map(grid) else {
            return;
        };
        // The selection's bytes, so the drag the mouse pulls is visible: a
        // band behind the text, the colours keeping their say.
        let selections: Vec<std::ops::Range<usize>> = editor
            .selections()
            .ranges()
            .iter()
            .filter(|selection| !selection.is_empty())
            .map(|selection| {
                let ordered = selection.ordered();
                ordered.start.0..ordered.end.0
            })
            .collect();
        for row in map.rows() {
            let ScreenRow::Text(row) = row else { continue };
            let Some(line) = lines.get(row.line) else {
                continue;
            };
            let (level_at, kind_at, text_at) = line_columns(line);
            let level_color = match line.level {
                Level::Debug => theme.rule,
                Level::Info => theme.muted,
                Level::Warn => theme.warn,
                Level::Error => theme.error,
            };
            // Every part that speaks keeps one colour of its own, so a
            // reader picks `samples` out of a page of `engine` without
            // reading a word of it. One accent for all of them made the
            // column a wall.
            let kind_color = kind_colour(&line.kind, theme);
            let text_color = match line.level {
                Level::Debug => theme.muted,
                Level::Info => theme.foreground,
                Level::Warn => theme.warn,
                Level::Error => theme.error,
            };
            // A resolved line is history: dim, whatever its level said.
            let resolved = |style: Style| {
                if line.resolved {
                    style.fg(theme.muted).add_modifier(Modifier::DIM)
                } else {
                    style
                }
            };
            for cell in &row.cells {
                let column = cell.columns.start;
                let colour = if column < level_at {
                    theme.rule
                } else if column < kind_at {
                    level_color
                } else if column < text_at {
                    kind_color
                } else {
                    text_color
                };
                let mut style = resolved(Style::default().fg(colour));
                if selections
                    .iter()
                    .any(|range| cell.bytes.start.0 < range.end && range.start < cell.bytes.end.0)
                {
                    style = style.bg(theme.selection).fg(theme.selection_text);
                }
                buffer.set_stringn(
                    cell.screen_x.start,
                    row.screen_y,
                    &cell.display,
                    usize::from(cell.screen_x.end.saturating_sub(cell.screen_x.start)),
                    style,
                );
            }
        }
        let below = self.panel.more_below(rows);
        let showing = if self.panel.verbose {
            "v quiets".to_owned()
        } else {
            match self.panel.hidden(self.log) {
                0 => "v shows more".to_owned(),
                held => format!("v shows {held} more"),
            }
        };
        let follows = if below > 0 {
            format!("End follows  ({below} more below)")
        } else {
            "following".to_owned()
        };
        let copy = self
            .keybinds
            .binding(super::keybinds::BindAction::Copy)
            .map(|binding| format!(" · {} copies", binding.hint()))
            .unwrap_or_default();
        // Docked, the band has keys of its own - `e` for the other edge,
        // `-` and `+` for its height - said early, where a narrow band
        // still shows them, and Esc leaves the log where it is.
        let (band, leave) = if self.docked {
            ("e top/bottom · -/+ height · ", "Esc to the score")
        } else {
            ("", "Esc closes")
        };
        let hint =
            format!("{band}{leave} · {follows} · ↑↓ wheel scroll · drag selects{copy} · {showing}");
        buffer.set_stringn(
            list.x,
            panel.bottom().saturating_sub(2),
            &hint,
            usize::from(log_list.width),
            Style::default().fg(theme.muted),
        );
    }
}

/// One colour a part of the studio keeps for as long as it speaks.
///
/// Taken from the theme rather than a fixed table, so it stays legible on
/// a light one, and chosen by hashing the name, so a part added later
/// needs registering nowhere and reads the same colour every session. Six
/// slots over a dozen or so names means some of them share, which is worth
/// it: the point is that `samples` and `engine` are visibly different down
/// a page, not that every name is unique by colour alone - the name is
/// right there.
fn kind_colour(kind: &str, theme: &Theme) -> Color {
    let palette = [
        theme.accent,
        theme.ok,
        theme.syntax.string,
        theme.syntax.number,
        theme.syntax.keyword.unwrap_or(theme.syntax.punctuation),
        theme.syntax.function.unwrap_or(theme.syntax.text),
    ];
    // FNV-1a: stable across runs and across machines, which a hasher from
    // the standard library's default state is deliberately not.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in kind.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    palette[(hash % palette.len() as u64) as usize]
}

fn render_pressure_details(
    buffer: &mut Buffer,
    area: Rect,
    pressure: Option<&EnginePressureSnapshot>,
    device: Option<&StudioDeviceInfo>,
    theme: &Theme,
) {
    // The process's cpu and memory are the header's, and where the memory
    // goes is the memory breakdown's; this block is the engine's own
    // health, and a stopped engine has none to report.
    let Some(pressure) = pressure else {
        return;
    };

    let load = |basis_points: u64| basis_points as f64 / 100.0;
    let milliseconds = |nanos: u64| nanos as f64 / 1_000_000.0;
    let callback = pressure.device.realtime_load;
    let realtime = pressure.device.realtime_pressure;
    let total = pressure.producer.phase(ProducerPhase::Total);
    let asset_depth = pressure
        .device
        .asset_queues
        .sample_installs
        .saturating_add(pressure.device.asset_queues.sample_returns)
        .saturating_add(pressure.device.asset_queues.orbit_reverb_installs)
        .saturating_add(pressure.device.asset_queues.orbit_reverb_returns)
        .saturating_add(pressure.device.asset_queues.fx_reverb_installs)
        .saturating_add(pressure.device.asset_queues.fx_reverb_returns);
    let (device_name, sample_rate, requested, actual) = device.map_or(
        (
            "-",
            callback.sample_rate_hz,
            callback.last_callback_frames,
            None,
        ),
        |device| {
            let output = device.audio.output();
            (
                output.device_id(),
                output.sample_rate_hz(),
                output.requested_buffer_frames(),
                output.reported_buffer_frames(),
            )
        },
    );
    let lines = [
        format!(
            "{} · DSP slow {:.1}% fast {:.1}% peak {:.1}% · callback {:.2}/{:.2}ms",
            pressure.cause.label(),
            load(pressure.dsp_load_basis_points()),
            load(pressure.dsp_fast_load_basis_points()),
            load(pressure.dsp_peak_load_basis_points()),
            milliseconds(callback.last_callback_busy_nanos),
            milliseconds(pressure.callback_period_nanos()),
        ),
        format!(
            "sched slow {:.1}% fast {:.1}% peak {:.1}% · turn p95 {:.2}ms · cover {}ms low {}ms",
            load(pressure.scheduler_load_basis_points()),
            load(pressure.scheduler_fast_load_basis_points()),
            load(pressure.scheduler_peak_load_basis_points()),
            milliseconds(total.p95_nanos),
            pressure.cover_millis(),
            pressure.producer.cover_low_water_nanos / 1_000_000,
        ),
        format!(
            "voices {} peak {} /{} semantic /{} hard · pending {} peak {} /{} · ring {}/{}/{}",
            pressure.active_voices(),
            pressure.peak_active_voices(),
            pressure.semantic_voice_capacity(),
            pressure.hard_voice_capacity(),
            realtime.pending_events,
            realtime.peak_pending_events,
            pressure.pending_capacity(),
            pressure.device.ring_depth,
            pressure.device.ring_peak_depth,
            pressure.device.ring_capacity,
        ),
        format!(
            "recent: deadline {} late {} refused {} fades {} drops {} pool misses {} · max host gap {:.2}ms · trace drops {} · assets {}/{}",
            pressure.callback_deadline_misses,
            realtime.window_late_events,
            realtime
                .window_refused_voices
                .saturating_add(pressure.producer_refusals),
            realtime.window_semantic_polyphony_fades,
            pressure.window_hard_drops(),
            pressure.window_pool_misses(),
            milliseconds(pressure.device.max_callback_gap_nanos),
            pressure.producer.scheduler_trace_drops,
            asset_depth,
            pressure.device.asset_queues.capacity,
        ),
        format!(
            "device {device_name} · {sample_rate}Hz · buffer requested {requested} actual {}",
            actual
                .map(|frames| frames.to_string())
                .unwrap_or_else(|| "-".to_owned()),
        ),
    ];
    for (row, line) in lines.into_iter().enumerate() {
        let style = if row == 0 {
            let color = match pressure.level {
                rustel_runtime::EnginePressureLevel::Normal => theme.ok,
                rustel_runtime::EnginePressureLevel::Caution => theme.accent,
                rustel_runtime::EnginePressureLevel::Warning => theme.warn,
                rustel_runtime::EnginePressureLevel::Error => theme.error,
            };
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        buffer.set_stringn(
            area.x,
            area.y + row as u16,
            line,
            usize::from(area.width),
            style,
        );
    }
}

#[cfg(test)]
mod resolved_tests {
    //! Tests for resolvable log alerts: a keyed warning counts on the header
    //! until its condition clears, and stays in the log as resolved history.

    use super::*;

    fn panel_text(log: &StudioLog, panel: &mut LogPanel) -> (Buffer, Rect) {
        let theme = Theme::default();
        let area = Rect::new(0, 0, 100, 20);
        let extent = LogExtent {
            details: LogPanelView::detail_rows(None),
            lines: u16::try_from(log.lines().len()).unwrap_or(u16::MAX),
            docked: false,
        };
        let list = LogPanelView::list_area(area, extent).expect("a list");
        panel.prepare(log, list);
        let mut buffer = Buffer::empty(area);
        LogPanelView {
            keybinds: &crate::keybinds::Keybinds::default(),
            panel,
            log,
            theme: &theme,
            pressure: None,
            device: None,
            memory: None,
            docked: false,
        }
        .render(area, &mut buffer);
        (buffer, area)
    }

    /// A keyed warning counts until its key resolves; the line stays, marked
    /// resolved, and unkeyed warnings keep counting as before.
    #[test]
    fn a_resolved_warning_stops_counting_and_stays_as_history() {
        let mut log = StudioLog::open(None);
        log.push_alert(Level::Warn, "samples", "bd came late", "late:1");
        log.push(Level::Warn, "device", "unkeyed");
        assert_eq!(log.unseen(), 2);

        assert_eq!(log.resolve_alert("late:1"), 1);
        assert_eq!(log.unseen(), 1, "the unkeyed warning still counts");
        let line = log
            .lines()
            .iter()
            .find(|line| line.text == "bd came late")
            .expect("the resolved line is kept");
        assert!(line.resolved);
        assert!(format_line(line).ends_with("bd came late  ✓"), "{line:?}");
        let unkeyed = log.lines().back().expect("the unkeyed line");
        assert!(!unkeyed.resolved, "an unkeyed warning never resolves");
    }

    /// Resolving twice, or a key nobody raised, changes nothing.
    #[test]
    fn resolving_twice_or_an_unknown_key_is_harmless() {
        let mut log = StudioLog::open(None);
        log.push_alert(Level::Warn, "samples", "late", "late:1");
        assert_eq!(log.resolve_alert("late:1"), 1);
        assert_eq!(log.resolve_alert("late:1"), 0);
        assert_eq!(log.resolve_alert("nobody"), 0);
        assert_eq!(log.unseen(), 0);
    }

    /// A seen warning that resolves is still shown resolved, and resolving it
    /// does not take a count some other warning holds.
    #[test]
    fn a_seen_warning_resolves_without_touching_the_count() {
        let mut log = StudioLog::open(None);
        log.push_alert(Level::Warn, "samples", "first", "late:1");
        log.mark_seen();
        log.push(Level::Warn, "device", "second");
        assert_eq!(log.unseen(), 1);
        assert_eq!(log.resolve_alert("late:1"), 1);
        assert_eq!(log.unseen(), 1, "the other warning's count is its own");
        assert!(log.lines().iter().any(|line| line.resolved));
    }

    /// A retired alert takes its badge down and leaves its line unresolved:
    /// a passing message timing out says nothing about its cause.
    #[test]
    fn a_retired_alert_stops_counting_without_resolving_its_line() {
        let mut log = StudioLog::open(None);
        log.push_alert(Level::Error, "editor", "clipboard failed", "error:editor");
        assert_eq!(log.retire_alert("error:editor"), 1);
        assert_eq!(log.unseen(), 0);
        let line = log.lines().back().expect("the line is kept");
        assert!(!line.resolved);
        assert!(!format_line(line).contains('✓'), "{line:?}");
    }

    /// The condition coming back is news again: said straight after itself,
    /// or after other lines, it counts once more.
    #[test]
    fn a_warning_raised_again_after_it_resolved_counts_again() {
        let mut log = StudioLog::open(None);
        log.push_alert(Level::Warn, "samples", "gone", "samples:/kit");
        log.resolve_alert("samples:/kit");
        assert_eq!(log.unseen(), 0);

        log.push_alert(Level::Warn, "samples", "gone", "samples:/kit");
        assert_eq!(log.unseen(), 1, "the same line again counts again");
        let line = log.lines().back().expect("one row, counted");
        assert!(!line.resolved);
        assert_eq!(line.repeats, 2);

        log.resolve_alert("samples:/kit");
        log.push(Level::Info, "samples", "something else");
        log.push_alert(Level::Warn, "samples", "gone", "samples:/kit");
        assert_eq!(log.unseen(), 1, "and so does a new line under the key");
    }

    /// The open panel follows a resolution, which leaves the ring's length
    /// alone, and draws the resolved line dim with its ✓.
    #[test]
    fn the_panel_redraws_a_line_that_resolved_dim_with_a_check() {
        let mut log = StudioLog::open(None);
        log.push_alert(Level::Warn, "samples", "bd came late", "late:1");
        let mut panel = LogPanel::default();
        let (buffer, area) = panel_text(&log, &mut panel);
        let before = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .find(|row| row.contains("bd came late"))
            .expect("the warning is drawn");
        assert!(!before.contains('✓'), "{before}");

        log.resolve_alert("late:1");
        let (buffer, area) = panel_text(&log, &mut panel);
        let (y, row) = (0..area.height)
            .map(|y| {
                (
                    y,
                    (0..area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>(),
                )
            })
            .find(|(_, row)| row.contains("bd came late"))
            .expect("the resolved line is still drawn");
        assert!(row.contains('✓'), "{row}");
        let x = u16::try_from(row.find("bd").expect("text")).expect("column");
        let x = area.x + u16::try_from(row[..x as usize].chars().count()).expect("column");
        let cell = &buffer[(x, y)];
        assert!(cell.modifier.contains(Modifier::DIM), "{cell:?}");
        assert_eq!(cell.fg, Theme::default().muted);
    }
}

#[cfg(test)]
mod tests {
    /// Nine refusals a second of the same missing sound were nine rows a
    /// second, and the log showed nothing else. The same line straight after
    /// itself is one row, counted; anything in between starts a new one.
    #[test]
    fn the_same_line_in_a_row_is_one_row_counted() {
        let mut log = StudioLog::open(None);
        let before = log.lines().len();
        for _ in 0..9 {
            log.push(
                Level::Warn,
                "voice-refused",
                "unknown wavetable \"wt_dbass\"",
            );
        }
        assert_eq!(log.lines().len(), before + 1);
        let line = log.lines().back().expect("one row");
        assert_eq!(line.repeats, 9);
        assert!(format_line(line).ends_with("unknown wavetable \"wt_dbass\"  \u{d7}9"));

        log.push(Level::Warn, "check", "refused");
        log.push(
            Level::Warn,
            "voice-refused",
            "unknown wavetable \"wt_dbass\"",
        );
        assert_eq!(
            log.lines().len(),
            before + 3,
            "a line in between starts a new row"
        );
        assert_eq!(log.lines().back().expect("row").repeats, 1);
        assert!(!format_line(log.lines().back().expect("row")).contains('\u{d7}'));
    }

    /// A docked log fills the room the layout reserved for it, including a
    /// side dock of `VIZ_WIDTH` (36) columns. The sheet form refuses
    /// anything under 40 columns.
    #[test]
    fn a_docked_log_fills_its_room_where_a_sheet_would_refuse_it() {
        let column = Rect::new(0, 0, 36, 30);
        let extent = LogExtent {
            details: 0,
            lines: 4,
            docked: false,
        };
        assert!(
            LogPanelView::geometry(column, extent).is_none(),
            "as a sheet, 36 columns is under the floor"
        );

        let docked = LogExtent {
            docked: true,
            ..extent
        };
        let (area, list) =
            LogPanelView::geometry(column, docked).expect("a dock is the room it was given");
        assert_eq!(area, column, "all of it, not a sheet inside it");
        assert!(list.width > 0 && list.height > 0, "{list:?}");
        assert!(
            column.union(list) == column,
            "the list stays inside: {list:?}"
        );

        // And a band across the bottom fills its full height rather than
        // anchoring a short sheet at the bottom of it.
        let band = Rect::new(0, 21, 120, 9);
        let (area, _) = LogPanelView::geometry(band, docked).expect("a band");
        assert_eq!(area, band);
    }

    #[test]
    fn memory_and_log_share_the_panel_at_thirty_to_seventy() {
        let panel = Rect::new(0, 0, 100, 12);
        let list = Rect::new(2, 1, 96, 9);
        let (memory, log) = LogPanelView::split_for_memory(panel, list);
        assert_eq!(memory, Rect::new(72, 1, 26, 10));
        assert_eq!(log, Rect::new(2, 1, 67, 9));
        assert_eq!(log.right() + 1, 70, "divider at seventy percent");
        assert!(log.right() < memory.x);
    }

    use super::*;

    /// Drawn docked, the panel is the band: its title on the band's top
    /// row and its list down to the hint, however tall the band is. Its
    /// hint names the band's keys, where a sheet's says Esc closes.
    #[test]
    fn a_docked_log_is_drawn_the_height_of_its_band_and_names_its_keys() {
        let theme = Theme::built_in_default();
        let mut log = StudioLog::open(None);
        for index in 0..40 {
            log.push(Level::Info, "engine", format!("line {index}"));
        }
        let band = Rect::new(0, 0, 120, 13);
        let draw = |docked: bool| {
            let mut panel = LogPanel {
                sticky: docked,
                ..LogPanel::default()
            };
            let extent = LogExtent {
                docked,
                ..LogExtent::of(&log, &panel, None)
            };
            let list = LogPanelView::list_area(band, extent).expect("a list");
            panel.prepare(&log, list);
            let mut buffer = Buffer::empty(band);
            LogPanelView {
                keybinds: &crate::keybinds::Keybinds::default(),
                panel: &panel,
                log: &log,
                theme: &theme,
                pressure: None,
                device: None,
                memory: None,
                docked,
            }
            .render(band, &mut buffer);
            text_of(&buffer, band)
        };

        let docked = draw(true);
        let rows = docked.lines().collect::<Vec<_>>();
        assert!(
            rows[0].contains(" log - "),
            "the title is the band's top: {docked}"
        );
        // Border, the stopped engine's one line, then nine lines of log.
        for index in 31..40 {
            assert!(docked.contains(&format!("line {index}")), "{docked}");
        }
        assert!(docked.contains("e top/bottom · -/+ height"), "{docked}");
        assert!(docked.contains("Esc to the score"), "{docked}");

        let sheet = draw(false);
        assert!(
            !sheet.contains("line 31"),
            "a sheet in the band is short: {sheet}"
        );
        assert!(sheet.contains("Esc closes"), "{sheet}");
    }

    /// Until `-` or `+` sets it, a docked log is a third of the terminal:
    /// never under a visuals band's default, never over a band's most.
    #[test]
    fn a_docked_log_defaults_to_a_third_of_the_terminal() {
        assert_eq!(default_dock_height(40), 13);
        assert_eq!(default_dock_height(24), BAND_DEFAULT_HEIGHT);
        assert_eq!(default_dock_height(200), BAND_MAX_HEIGHT);
        let mut panel = LogPanel::default();
        assert_eq!(panel.dock_height(60), 20);
        panel.height = Some(7);
        assert_eq!(panel.dock_height(60), 7, "what a resize set");
        panel.height = Some(99);
        assert_eq!(
            panel.dock_height(60),
            BAND_MAX_HEIGHT,
            "within a band's bounds"
        );
    }

    fn text_of(buffer: &Buffer, area: Rect) -> String {
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The `studio.log` line that docs/studio.md quotes has the shape of a
    /// record this file writes. A layout change fails until the docs follow.
    #[test]
    fn the_documented_studio_log_line_is_one_the_file_writes() {
        let docs = include_str!("../../../docs/studio.md");
        let text = "refused - line 2: unknown sound \"abcde\"";
        let quoted = docs
            .split('`')
            .find(|span| span.ends_with(text))
            .expect("docs/studio.md quotes a studio.log line");
        let written = file_record(0, Level::Warn, "check", text);
        // Any instant: the digits of the stamp are the reader's example.
        let shape = |line: &str| {
            line.trim_end_matches('\n')
                .chars()
                .map(|c| if c.is_ascii_digit() { '9' } else { c })
                .collect::<String>()
        };
        let quoted = quoted.replace("pid 1234", &format!("pid {}", std::process::id()));
        assert_eq!(shape(&quoted), shape(&written), "{quoted}");
        assert!(
            quoted.contains(&format!(" {} [check] ", Level::Warn.label())),
            "{quoted}"
        );
    }

    #[test]
    fn the_log_keeps_a_ring_counts_unseen_warnings_and_writes_its_file() {
        let dir = std::env::temp_dir().join(format!("rustel-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut log = StudioLog::open(Some(&dir));
        assert_eq!(log.unseen(), 0, "the opening line is information");
        log.push_alert(
            Level::Warn,
            "check",
            "refused - line 2: unknown sound \"abcde\"",
            "lint:scene-1",
        );
        log.push(Level::Error, "engine", "device lost\nsecond line");
        log.push(Level::Info, "update", "generation 3");
        assert_eq!(log.unseen(), 2);
        assert_eq!(log.resolve_alert("lint:scene-1"), 1);
        assert_eq!(log.unseen(), 1, "the unrelated engine error remains");
        assert_eq!(log.resolve_alert("lint:scene-1"), 0, "resolved once");
        assert!(
            log.lines()
                .iter()
                .any(|line| line.text.contains("unknown sound")),
            "resolving the badge keeps its history"
        );
        log.mark_seen();
        assert_eq!(log.unseen(), 0);
        assert_eq!(log.lines().len(), 4);
        let text = std::fs::read_to_string(dir.join(LOG_FILE_NAME)).unwrap();
        assert!(text.contains("warn  [check] refused"), "{text}");
        assert!(text.contains("device lost ⏎ second line"), "{text}");
        assert_eq!(text.lines().count(), 4);
        for index in 0..CAPACITY + 5 {
            log.push(Level::Info, "x", index.to_string());
        }
        assert_eq!(log.lines().len(), CAPACITY);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sheet is as tall as its contents: a stopped engine's pressure
    /// block keeps no row, and a short log leaves no blank rows.
    #[test]
    fn the_sheet_is_as_tall_as_what_it_shows() {
        let area = Rect::new(0, 0, 100, 30);
        // With the engine stopped there is no pressure to report, so the
        // block keeps no row. It always kept five, blank but for a line of
        // the header's own figures.
        assert_eq!(LogPanelView::detail_rows(None), 0);
        assert_eq!(
            LogPanelView::DETAIL_ROWS,
            5,
            "and five while one is running"
        );
        let full = LogExtent {
            details: 5,
            lines: 64,
            docked: false,
        };
        assert_eq!(
            LogPanelView::rows(area, LogExtent { details: 0, ..full }),
            LogPanelView::rows(area, full) + 5,
            "five rows the log gets back with the engine stopped"
        );

        // And the sheet itself stops where the log does. Six lines drew
        // six lines and then four blank rows above the hint.
        let six = LogExtent {
            details: 2,
            lines: 6,
            docked: false,
        };
        let (sheet, _) = LogPanelView::geometry(area, six).expect("a sheet");
        assert_eq!(
            sheet.height, 11,
            "one printed line, its rule, six log lines, three edges"
        );
        assert_eq!(LogPanelView::rows(area, six), 6, "and no blank rows");
        let (grown, _) = LogPanelView::geometry(area, full).expect("a sheet");
        assert_eq!(grown.height, 15, "and grows with the log, to the same cap");
        assert!(grown.bottom() == sheet.bottom(), "anchored at the bottom");
    }

    /// `v` shows the running commentary and hides it again. The filter is
    /// on the view: nothing is dropped, so the same press brings it back
    /// and `studio.log` on disk has everything either way.
    #[test]
    fn the_commentary_waits_behind_v_and_is_never_thrown_away() {
        let dir = std::env::temp_dir().join(format!("rustel-log-v-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut log = StudioLog::open(Some(&dir));
        log.push(Level::Debug, "focus", "reference panel");
        log.push(Level::Info, "samples", "imported /packs/kicks - 12 bank(s)");
        log.push(
            Level::Warn,
            "drop",
            "not audio, a folder of audio, or a score",
        );
        assert_eq!(log.unseen(), 1, "the commentary interrupts nobody");

        let mut panel = LogPanel::opened(false);
        panel.sync(&log, 20);
        let quiet: Vec<&str> = panel
            .shown()
            .iter()
            .map(|line| line.kind.as_str())
            .collect();
        assert_eq!(
            quiet,
            ["studio", "samples", "drop"],
            "the opening line, the import, the refusal"
        );
        assert_eq!(panel.hidden(&log), 1);

        assert!(panel.toggle_verbose());
        panel.sync(&log, 20);
        assert_eq!(panel.shown().len(), 4);
        assert_eq!(panel.hidden(&log), 0);
        assert!(!panel.toggle_verbose());
        panel.sync(&log, 20);
        assert_eq!(panel.shown().len(), 3, "and it comes back");

        // The file keeps every line whatever the panel is showing.
        let text = std::fs::read_to_string(dir.join(LOG_FILE_NAME)).unwrap();
        assert!(text.contains("debug [focus] reference panel"), "{text}");
        assert_eq!(text.lines().count(), 4);
        let _ = std::fs::remove_dir_all(&dir);

        // The running tally must agree with the lines held, also after the
        // ring is full and drops its oldest lines.
        for index in 0..CAPACITY + 5 {
            log.push(
                if index % 3 == 0 {
                    Level::Debug
                } else {
                    Level::Info
                },
                "engine",
                index.to_string(),
            );
        }
        assert_eq!(log.lines().len(), CAPACITY);
        for verbose in [false, true] {
            panel.verbose = verbose;
            panel.sync(&log, 20);
            assert_eq!(
                panel.shown().len(),
                log.at_least(panel.floor()),
                "the tally counts what the filter shows, verbose {verbose}"
            );
            assert_eq!(panel.hidden(&log), log.lines().len() - panel.shown().len());
        }
    }

    #[test]
    fn the_log_wraps_long_lines_and_follows_visual_rows() {
        let mut log = StudioLog::open(None);
        log.push(Level::Info, "engine", "a very long message ".repeat(12));
        let mut panel = LogPanel::default();
        panel.editor.set_view_size(24, 3);
        panel.sync(&log, 3);

        assert!(panel.editor.viewport().wrap);
        assert_eq!(panel.editor.horizontal_scroll_extent(), None);
        assert!(
            panel.editor.visual_row_count() > panel.editor.document().line_count(),
            "the long physical line occupies several visible rows"
        );
        assert_eq!(
            usize::from(LogExtent::of(&log, &panel, None).lines),
            panel.editor.visual_row_count(),
            "the sheet grows to the wrapped rows instead of hiding older entries"
        );
        assert_eq!(
            panel.editor.viewport().top_row,
            panel.editor.visual_row_count().saturating_sub(3),
            "follow mode measures the wrapped rows"
        );

        log.push(Level::Info, "engine", "another long message ".repeat(12));
        panel.sync(&log, 3);
        assert!(panel.editor.viewport().wrap, "rebuilding keeps wrapping on");
        assert_eq!(panel.editor.horizontal_scroll_extent(), None);
        assert_eq!(
            panel.editor.viewport().top_row,
            panel.editor.visual_row_count().saturating_sub(3),
            "the newest wrapped row remains visible"
        );
    }

    #[test]
    fn wrapped_log_messages_align_and_keep_copy_coordinates_after_resize() {
        use super::super::editor::{CellPoint, Hit};

        let mut log = StudioLog::open(None);
        let message = "iTerm2 · truecolor ✓ · keyboard ✓ · drawing cells ".repeat(4);
        for kind in ["terminal", "longer-source", "terminal"] {
            log.push(Level::Info, kind, &message);
        }
        // Past 100 minutes the clock prints a third digit of minutes, and
        // the message starts a column further in.
        log.lines.back_mut().expect("the last line").at = 6_001.0;
        let mut panel = LogPanel::default();
        for width in [80, 40, 12, 100] {
            let area = Rect::new(3, 2, width, 200);
            panel.prepare(&log, area);
            panel.top(200);
            let map = panel
                .editor
                .screen_map(GridRect::new(area.x, area.y, area.width, area.height))
                .unwrap();
            // Where each line's text starts in what was printed, in cells.
            let text_columns = panel
                .shown()
                .iter()
                .map(|line| {
                    let printed = format_line(line);
                    let prefix = printed
                        .strip_suffix(line.text.as_str())
                        .expect("the text ends the printed line");
                    UnicodeWidthStr::width(prefix)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                text_columns.last(),
                text_columns
                    .iter()
                    .zip(panel.shown())
                    .find(|(_, line)| line.text == message)
                    .map(|(column, _)| column + 1)
                    .as_ref()
            );
            let rows = map
                .rows()
                .iter()
                .filter_map(|row| match row {
                    ScreenRow::Text(row) => Some(row),
                    ScreenRow::Virtual(_) => None,
                })
                .collect::<Vec<_>>();
            let mut continuations = 0;
            for row in rows.iter().filter(|row| row.segment > 0) {
                continuations += 1;
                let expected = text_columns[row.line].min(usize::from(width).saturating_sub(9));
                assert_eq!(usize::from(row.hanging), expected);
                let cell = row.cells.first().expect("continuation text");
                assert_eq!(usize::from(cell.screen_x.start - area.x), expected);
                assert!(matches!(
                    map.hit_test(CellPoint::new(cell.screen_x.start, row.screen_y)),
                    Some(Hit::Text { offset, .. }) if offset == cell.bytes.start
                ));
            }
            assert!(continuations > 0);

            // Drag from the indent in front of a continuation row of the
            // message to three cells into the row under it, and copy.
            let source = panel.editor.source();
            let message_start = ByteOffset(source.find(&message).expect("message"));
            let (row, next) = rows
                .windows(2)
                .map(|pair| (pair[0], pair[1]))
                .find(|(row, next)| {
                    row.segment > 0
                        && row.hanging > 0
                        && row.cells[0].bytes.start >= message_start
                        && next.line == row.line
                })
                .expect("two continuation rows of the message");
            let press = map
                .hit_test_within(CellPoint::new(area.x, row.screen_y))
                .expect("inside the grid")
                .selection_offset();
            assert_eq!(
                press, row.cells[0].bytes.start,
                "the indent reaches the row's first character"
            );
            let target = &next.cells[2];
            let release = map
                .hit_test_within(CellPoint::new(target.screen_x.start, next.screen_y))
                .expect("inside the grid")
                .selection_offset();
            assert_eq!(
                release, target.bytes.start,
                "the cell drawn under the pointer"
            );
            panel
                .editor
                .set_selection(Selection::range(press, release))
                .unwrap();
            let copied = panel.editor.selected_text().unwrap();
            assert_eq!(copied, source[press.0..release.0]);
            assert!(message.contains(&copied), "{copied:?}");
            assert!(copied.chars().count() > 3, "{copied:?}");
        }
    }

    /// When the full ring drops its oldest line, a selection shifts back
    /// with the text and still holds the same lines.
    #[test]
    fn a_selection_survives_the_ring_dropping_its_oldest_lines() {
        let mut log = StudioLog::open(None);
        for index in 0..CAPACITY {
            log.push(Level::Info, "engine", format!("line {index}"));
        }
        let area = Rect::new(0, 0, 100, 30);
        let mut panel = LogPanel::default();
        let extent = LogExtent {
            details: LogPanelView::detail_rows(None),
            lines: u16::try_from(log.lines().len()).unwrap_or(u16::MAX),
            docked: false,
        };
        let list = LogPanelView::list_area(area, extent).expect("a list");
        panel.prepare(&log, list);

        // Select the middle of the second line ("line 1" is 6 cells).
        let text = panel.editor.source();
        let start = text.find("line 1\n").expect("the second line");
        let _ = panel
            .editor
            .set_selection(super::super::editor::Selection::range(
                super::super::editor::ByteOffset(start),
                super::super::editor::ByteOffset(start + 7),
            ));
        assert_eq!(
            panel.editor.selected_text().unwrap(),
            "line 1\n",
            "the drag held what it dragged"
        );

        // One more line arrives: the ring is full, so the oldest line
        // goes, the text no longer merely grew - and the selection stays.
        log.push(Level::Info, "engine", "line 2000");
        panel.sync(&log, 20);
        assert_eq!(
            panel.editor.selected_text().unwrap(),
            "line 1\n",
            "the selection rode the eviction"
        );

        // A selection reaching into the dropped line cannot be preserved
        // honestly; it is let go rather than lied about.
        let _ = panel
            .editor
            .set_selection(super::super::editor::Selection::range(
                super::super::editor::ByteOffset(0),
                super::super::editor::ByteOffset(20),
            ));
        log.push(Level::Info, "engine", "line 2001");
        panel.sync(&log, 20);
        assert!(
            panel.editor.selected_text().unwrap().is_empty(),
            "a half-dropped selection is not faked"
        );
    }

    /// The eviction helper agrees with itself in every direction: growth
    /// is not an eviction, a whole-line front drop is found from anywhere,
    /// a line dropped mid-selection is refused, and anything reshaping -
    /// a filter toggle, reflowed text - has no mapping to offer.
    #[test]
    fn eviction_is_only_a_whole_line_front_drop() {
        use super::LogPanel;
        // Pure growth: not an eviction.
        assert_eq!(
            LogPanel::dropped_prefix_length("aa\nbb", "aa\nbb\ncc"),
            None
        );
        // One line, then three, dropped from the front.
        assert_eq!(
            LogPanel::dropped_prefix_length("a\nbb\ncc\ndd", "bb\ncc\ndd"),
            Some(2)
        );
        assert_eq!(
            LogPanel::dropped_prefix_length("a\nbb\ncc\ndd", "dd"),
            Some(8)
        );
        // At the cap the line count does not change: one in, one out.
        assert_eq!(
            LogPanel::dropped_prefix_length("a\nbb\ncc", "bb\ncc\nddd"),
            Some(2)
        );
        // Mid-text change is not a front drop.
        assert_eq!(
            LogPanel::dropped_prefix_length("aa\nbb\ncc", "aa\nXX\ncc"),
            None
        );
        // A front drop that also changed the tail is refused outright.
        assert_eq!(
            LogPanel::dropped_prefix_length("aa\nbb\ncc", "bb\nXX"),
            None
        );
        // Empty before: nothing to map.
        assert_eq!(LogPanel::dropped_prefix_length("", "anything"), None);
    }

    /// Each part that speaks keeps a colour of its own, so a page of
    /// `engine` and a page of `samples` are told apart without reading.
    #[test]
    fn each_part_of_the_studio_keeps_its_own_colour() {
        let theme = Theme::default();
        assert_eq!(
            kind_colour("samples", &theme),
            kind_colour("samples", &theme),
            "and keeps it"
        );
        let named = ["samples", "engine", "drop", "transport", "focus", "take"];
        let colours: std::collections::BTreeSet<String> = named
            .iter()
            .map(|kind| format!("{:?}", kind_colour(kind, &theme)))
            .collect();
        assert!(
            colours.len() > 1,
            "one accent for every part made the column a wall"
        );
    }

    #[test]
    fn the_panel_draws_newest_last_and_scrolls_up() {
        let theme = Theme::default();
        let mut log = StudioLog::open(None);
        for index in 0..30 {
            log.push(
                if index % 10 == 0 {
                    Level::Warn
                } else {
                    Level::Info
                },
                "engine",
                format!("line {index}"),
            );
        }
        let area = Rect::new(0, 0, 100, 30);
        let mut panel = LogPanel::default();
        let extent = LogExtent {
            details: LogPanelView::detail_rows(None),
            lines: u16::try_from(log.lines().len()).unwrap_or(u16::MAX),
            docked: false,
        };
        let rows = LogPanelView::rows(area, extent);
        let list = LogPanelView::list_area(area, extent).expect("a list");
        assert!(rows >= 3);
        panel.prepare(&log, list);
        let mut buffer = Buffer::empty(area);
        LogPanelView {
            keybinds: &crate::keybinds::Keybinds::default(),
            panel: &panel,
            log: &log,
            theme: &theme,
            pressure: None,
            device: None,
            memory: None,
            docked: false,
        }
        .render(area, &mut buffer);
        let text = text_of(&buffer, area);
        assert!(text.contains("line 29"), "{text}");
        assert!(text.contains("not written to disk"), "{text}");
        assert!(panel.following);
        panel.scroll(5, rows);
        assert!(!panel.following, "scrolling up lets go of the newest line");
        assert_eq!(panel.more_below(rows), 5);
        panel.prepare(&log, list);
        let mut buffer = Buffer::empty(area);
        LogPanelView {
            keybinds: &crate::keybinds::Keybinds::default(),
            panel: &panel,
            log: &log,
            theme: &theme,
            pressure: None,
            device: None,
            memory: None,
            docked: false,
        }
        .render(area, &mut buffer);
        let text = text_of(&buffer, area);
        assert!(!text.contains("line 29"), "{text}");
        assert!(text.contains("line 24"), "{text}");
        assert!(text.contains("5 more below"), "{text}");
        // A line arriving while the view is held keeps the view; the
        // selection over what was there survives, since the log only grew.
        let _ = panel
            .editor
            .set_selection(super::super::editor::Selection::range(
                super::super::editor::ByteOffset(0),
                super::super::editor::ByteOffset(5),
            ));
        log.push(Level::Error, "engine", "line 30");
        panel.sync(&log, rows);
        assert_eq!(panel.more_below(rows), 6);
        assert_eq!(panel.editor.selected_text().unwrap().len(), 5);
        panel.follow(rows);
        assert!(panel.following);
        assert_eq!(panel.more_below(rows), 0);
        assert!(panel.editor.source().ends_with("line 30"));
    }

    #[test]
    fn the_panel_explains_engine_pressure_without_taking_log_rows() {
        let theme = Theme::default();
        let log = StudioLog::open(None);
        let area = Rect::new(0, 0, 160, 40);
        let mut pressure = EnginePressureSnapshot {
            cause: rustel_runtime::EnginePressureCause::ProducerOverload,
            level: rustel_runtime::EnginePressureLevel::Error,
            callback_deadline_misses: 2,
            producer_refusals: 3,
            host_gap_nanos: 8_000_000,
            ..EnginePressureSnapshot::default()
        };
        pressure.device.realtime_load.sample_rate_hz = 48_000;
        pressure.device.realtime_load.last_callback_frames = 128;
        pressure.device.realtime_load.last_callback_busy_nanos = 2_000_000;
        pressure.device.realtime_load.last_callback_period_nanos = 2_666_666;
        pressure.device.realtime_load.slow_load_basis_points = 2_400;
        pressure.device.realtime_load.fast_load_basis_points = 3_100;
        pressure.device.realtime_load.peak_load_basis_points = 8_800;
        pressure.device.max_callback_gap_nanos = 8_000_000;
        pressure.device.realtime_pressure.active_voices = 18;
        pressure.device.realtime_pressure.peak_active_voices = 23;
        pressure.device.realtime_pressure.pending_events = 34;
        pressure.device.realtime_pressure.peak_pending_events = 55;
        pressure.device.ring_depth = 7;
        pressure.device.ring_peak_depth = 11;
        pressure.device.ring_capacity = 64;
        pressure.producer.slow_load_basis_points = 1_100;
        pressure.producer.fast_load_basis_points = 1_900;
        pressure.producer.peak_load_basis_points = 4_400;
        pressure.producer.cover_end_nanos = 420_000_000;
        pressure.producer.cover_low_water_nanos = 250_000_000;
        let total = ProducerPhase::ALL
            .iter()
            .position(|phase| *phase == ProducerPhase::Total)
            .unwrap();
        pressure.producer.phases[total].p95_nanos = 1_250_000;
        let device = StudioDeviceInfo {
            stream_id: 7,
            registry: std::sync::Arc::new(rustel_runtime::capability_registry().clone()),
            audio: rustel_runtime::AudioStreamFacts::new(
                rustel_runtime::AudioOutputFacts::new(
                    rustel_runtime::AudioHost::cpal("alsa"),
                    "Test Interface",
                    48_000,
                    2,
                    Some(rustel_runtime::AudioSampleFormat::F32),
                    128,
                    Some(144),
                ),
                None,
            ),
            allocator_tripwire_armed: true,
        };
        let rows = LogPanelView::rows(
            area,
            LogExtent {
                details: LogPanelView::detail_rows(Some(&pressure)),
                lines: 64,
                docked: false,
            },
        );
        assert_eq!(rows, 10, "five detail rows remain separate from log rows");
        let mut buffer = Buffer::empty(area);
        LogPanelView {
            keybinds: &crate::keybinds::Keybinds::default(),
            panel: &LogPanel::default(),
            log: &log,
            theme: &theme,
            pressure: Some(&pressure),
            device: Some(&device),
            memory: None,
            docked: false,
        }
        .render(area, &mut buffer);
        let text = text_of(&buffer, area);
        assert!(
            text.contains("scheduler fell behind · DSP slow 24.0%"),
            "{text}"
        );
        assert!(text.contains("sched slow 11.0%"), "{text}");
        assert!(
            text.contains("voices 18 peak 23 /128 semantic /512 hard"),
            "{text}"
        );
        assert!(text.contains("deadline 2 late 0"), "{text}");
        assert!(text.contains("max host gap 8.00ms"), "{text}");
        assert!(text.contains("device Test Interface · 48000Hz"), "{text}");
        assert!(
            !text.contains("process cpu") && !text.contains("sounds live"),
            "the header's figures and the memory breakdown's stay theirs: {text}"
        );
    }

    #[test]
    fn log_footer_tracks_copy_rebinding_and_unbinding() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds};
        let theme = Theme::built_in_default();
        let log = StudioLog::open(None);
        let panel = LogPanel::default();
        let draw = |keybinds: &Keybinds| {
            let area = Rect::new(0, 0, 160, 30);
            let mut buffer = Buffer::empty(area);
            LogPanelView {
                keybinds,
                panel: &panel,
                log: &log,
                theme: &theme,
                pressure: None,
                device: None,
                memory: None,
                docked: false,
            }
            .render(area, &mut buffer);
            text_of(&buffer, area)
        };
        let mut bindings = Keybinds::default();
        bindings.learn(BindAction::Copy, KeyCombo::parse("f2"));
        assert!(draw(&bindings).contains("F2 copies"));
        assert!(!draw(&bindings).contains("^C copies"));
        bindings.unbind(BindAction::Copy);
        assert!(!draw(&bindings).contains("copies"));
    }
}

#[cfg(test)]
mod file_record_tests {
    use super::*;

    #[test]
    fn a_file_record_identifies_the_current_studio_process() {
        let record = file_record(0, Level::Info, "engine", "ready\nnext line");
        assert!(record.contains(&format!(" pid {} info  [engine] ", std::process::id())));
        assert!(record.ends_with("ready ⏎ next line\n"));
        assert_eq!(record.lines().count(), 1);
    }
}
