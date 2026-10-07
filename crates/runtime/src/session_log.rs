//! The tape of a live-coding set: every save, when it happened, and what the
//! engine did with it.
//!
//! A performance is a sequence of edits against a clock, so it can be recorded
//! as one - no audio, no screen capture, just the text and the timing. That
//! makes a set reproducible three ways at once: an artist can replay a
//! performance, a listener can watch the code arrive live in an editor, and a
//! bug that took thirty minutes of playing to reach can be handed over as a
//! file that reproduces it exactly.
//!
//! The format is JSON Lines so it stays readable, greppable, and appendable
//! after a crash: one header line, then one line per event. Sources are
//! base64 so a score with any quoting, newline, or unicode survives the round
//! trip intact - and so a single line can be lifted straight out of the file.
//!
//! `Normal` keeps only the saves that installed, which is the artist's tape.
//! `Debug` keeps rejected saves and engine diagnostics too, which is the bug
//! report: the save that broke a set is usually the one that did not install.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::product;
use crate::ui_events::source_revision;

/// How much of a set to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// Only the saves that installed: a clean, replayable performance.
    Normal,
    /// Every save including rejections, plus engine diagnostics.
    Debug,
}

impl SessionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionMode::Normal => "normal",
            SessionMode::Debug => "debug",
        }
    }

    /// What the tape holds, in words that mean something to whoever opens it.
    pub fn keeps(self) -> &'static str {
        match self {
            SessionMode::Normal => "installed-saves",
            SessionMode::Debug => "all-saves-and-diagnostics",
        }
    }
}

/// What the engine did with a save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStatus {
    Installed,
    Rejected,
}

impl SaveStatus {
    /// The word the tape records for this status.
    pub fn as_str(self) -> &'static str {
        match self {
            SaveStatus::Installed => "installed",
            SaveStatus::Rejected => "rejected",
        }
    }
}

/// The score a stop installs, recorded with `via: "stop"`.
pub const STOP_SOURCE: &str = "silence";

// A 16 MiB source (the session confirmation limit) expands to less than
// 22 MiB in base64. Leave room for JSON metadata without allowing an
// untrusted, newline-free tape to grow a line buffer without bound.
const MAX_SESSION_LINE_BYTES: usize = 32 * 1024 * 1024;

fn read_session_line(
    reader: &mut impl BufRead,
    path: &Path,
    line_number: usize,
) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_SESSION_LINE_BYTES + 2) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > MAX_SESSION_LINE_BYTES {
        return Err(format!(
            "{} line {line_number} exceeds the {} byte session line limit",
            path.display(),
            MAX_SESSION_LINE_BYTES
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// Appends a set to disk as it is performed.
pub struct SessionRecorder {
    file: File,
    path: PathBuf,
    mode: SessionMode,
    saves: usize,
    /// Where each distinct source first appeared on the tape, so a set that
    /// returns to a scene it already played writes a reference rather than
    /// the score again.
    seen: HashMap<String, usize>,
}

impl SessionRecorder {
    /// Start a recording. The header is flushed immediately so an interrupted
    /// set still leaves a readable file.
    pub fn create(
        path: PathBuf,
        mode: SessionMode,
        baseline_cps: Option<f64>,
    ) -> Result<Self, String> {
        let file = File::create(&path)
            .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        Self::from_file(file, path, mode, baseline_cps)
    }

    /// Start a fresh tape without replacing an existing file. Collisions
    /// become `name-2.rustel-session`, `name-3.rustel-session`, and so on;
    /// exclusive creation also protects simultaneous recording starts.
    pub fn create_unique(
        path: PathBuf,
        mode: SessionMode,
        baseline_cps: Option<f64>,
    ) -> Result<Self, String> {
        let mut number = 1_usize;
        loop {
            let candidate = if number == 1 {
                path.clone()
            } else {
                let mut name = path.file_stem().unwrap_or_default().to_os_string();
                name.push(format!("-{number}"));
                if let Some(extension) = path.extension() {
                    name.push(".");
                    name.push(extension);
                }
                path.with_file_name(name)
            };
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => {
                    let recorder = Self::from_file(file, candidate.clone(), mode, baseline_cps);
                    if recorder.is_err() {
                        // Initialization has dropped the file handle, also
                        // allowing removal on Windows. Only the file this
                        // call exclusively created is ours to remove.
                        let _ = std::fs::remove_file(&candidate);
                    }
                    return recorder;
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    number = number.checked_add(1).ok_or_else(|| {
                        format!("cannot find an unused session name for {}", path.display())
                    })?;
                }
                Err(error) => {
                    return Err(format!("cannot create {}: {error}", candidate.display()));
                }
            }
        }
    }

    fn from_file(
        mut file: File,
        path: PathBuf,
        mode: SessionMode,
        baseline_cps: Option<f64>,
    ) -> Result<Self, String> {
        // The header holds only fields that change how a reader interprets
        // the tape:
        //
        // - `version` is the format's number, so a wrong file is rejected
        //   with a clear message and the format can change later.
        // - `recorded` is the UTC time the recording started.
        // - `keeps` says what was thrown away, which decides whether a tape
        //   can be trusted as a complete record.
        //
        // Nothing in the header names this engine, so another implementation
        // of the format can write it. Which engine recorded a tape is a
        // diagnostic, so it goes with the diagnostics below: present in
        // debug tapes, absent from the header.
        //
        // The header has no magic marker: the filename suffix says what the
        // file is, and a reader that needs certainty reads `version`.
        //
        // The header also omits the score's path (it would put a home
        // directory into every bug report), the score's name (the filename
        // carries it) and the tempo (a score sets its own with `setCpm`).
        // `baseline_cps` appears only when the CLI was started with a tempo
        // that is not the engine default, the one case a replay cannot
        // otherwise reconstruct.
        let mut header = serde_json::json!({
            "version": 2,
            "recorded": iso8601_utc(unix_now()),
            "keeps": mode.keeps(),
        });
        if let Some(cps) = baseline_cps {
            header["baseline_cps"] = serde_json::json!(cps);
        }
        writeln!(file, "{header}").map_err(|error| format!("cannot write session: {error}"))?;
        file.flush()
            .map_err(|error| format!("cannot write session: {error}"))?;
        let mut recorder = Self {
            file,
            path,
            mode,
            saves: 0,
            seen: HashMap::new(),
        };
        // Which engine produced the tape matters for a bug report and not at
        // all for a replay, so it rides with the diagnostics rather than in
        // the header every implementation has to agree on.
        recorder.try_record_log(
            0.0,
            &serde_json::json!({ "engine": crate::product::engine_identity() }).to_string(),
        )?;
        Ok(recorder)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The tape's folder moved under it: the open file goes on being
    /// written where it is, and this is where it is now.
    pub fn relocate(&mut self, path: PathBuf) {
        self.path = path;
    }

    pub fn mode(&self) -> SessionMode {
        self.mode
    }

    pub fn saves(&self) -> usize {
        self.saves
    }

    /// Record one save at `at` seconds from the start of the set.
    ///
    /// A rejected save is the interesting one for a bug report and noise for a
    /// performance, so it is kept only in `Debug`.
    ///
    /// A source already on the tape is written as `{"ref": n}` naming the
    /// save it first appeared in. Switching between scenes repeats sources
    /// constantly, and a reference costs a few bytes where the score would
    /// cost kilobytes - while keeping every line independently decodable,
    /// which a diff never would.
    pub fn record_save(&mut self, at: f64, status: SaveStatus, source: &str, error: Option<&str>) {
        self.record_save_via(at, status, source, error, None);
    }

    /// A save that no keystroke made: `via` says what did - `"slider"` for a
    /// control that moved the running score's literal, `"stop"` for the
    /// silence a stop installs. A reader that does not care treats it as
    /// any other save, which is what it sounded like.
    pub fn record_save_via(
        &mut self,
        at: f64,
        status: SaveStatus,
        source: &str,
        error: Option<&str>,
        via: Option<&str>,
    ) {
        let _ = self.try_record_save_via(at, status, source, error, via);
    }

    /// Checked recording for the initial snapshot of a new tape. A caller
    /// can retain its old recorder until the new snapshot reaches disk.
    pub fn try_record_save_via(
        &mut self,
        at: f64,
        status: SaveStatus,
        source: &str,
        error: Option<&str>,
        via: Option<&str>,
    ) -> Result<(), String> {
        if status == SaveStatus::Rejected && self.mode == SessionMode::Normal {
            return Ok(());
        }
        let mut record = serde_json::json!({
            "t": round_millis(at),
            "kind": "save",
            "status": status.as_str(),
        });
        if let Some(via) = via {
            record["via"] = serde_json::Value::String(via.to_owned());
        }
        let hash = source_revision(source);
        match self.seen.get(&hash) {
            Some(index) => record["ref"] = serde_json::Value::from(*index),
            None => {
                record["source"] = serde_json::Value::String(encode_base64(source.as_bytes()));
            }
        }
        if let (Some(error), SessionMode::Debug) = (error, self.mode) {
            record["error"] = serde_json::Value::String(error.to_string());
        }
        self.write(&record)?;
        self.seen.entry(hash).or_insert(self.saves);
        self.saves += 1;
        Ok(())
    }

    /// Record a gesture that changed the sound without changing the score:
    /// the master fader, a take starting or ending. `fields` are merged into
    /// the line. Replay does not act on these; they are what a listener needs
    /// to line a take up with its tape.
    pub fn record_control(&mut self, at: f64, fields: serde_json::Value) {
        let _ = self.try_record_control(at, fields);
    }

    /// Checked counterpart used while preparing a new tape's initial
    /// controls, before making it the active recorder.
    pub fn try_record_control(&mut self, at: f64, fields: serde_json::Value) -> Result<(), String> {
        let mut record = serde_json::json!({
            "t": round_millis(at),
            "kind": "control",
        });
        if let (Some(record), Some(fields)) = (record.as_object_mut(), fields.as_object()) {
            for (key, value) in fields {
                record.insert(key.clone(), value.clone());
            }
        }
        self.write(&record)
    }

    /// Record one engine diagnostic line. Ignored outside `Debug`.
    pub fn record_log(&mut self, at: f64, line: &str) {
        let _ = self.try_record_log(at, line);
    }

    fn try_record_log(&mut self, at: f64, line: &str) -> Result<(), String> {
        if self.mode != SessionMode::Debug {
            return Ok(());
        }
        // Engine diagnostics are already JSON; keep them as structured values
        // rather than a string holding escaped JSON, so the file stays
        // greppable by field.
        let payload = serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|_| serde_json::Value::String(line.to_string()));
        self.write(&serde_json::json!({
            "t": round_millis(at),
            "kind": "log",
            "line": payload,
        }))
    }

    fn write(&mut self, record: &serde_json::Value) -> Result<(), String> {
        // Ordinary recording ignores these errors so a full disk cannot
        // stop a performance. Recorder handoff uses the checked APIs.
        writeln!(self.file, "{record}")
            .map_err(|error| format!("cannot write session: {error}"))?;
        self.file
            .flush()
            .map_err(|error| format!("cannot write session: {error}"))
    }
}

/// One recorded save.
#[derive(Debug, Clone)]
pub struct SessionSave {
    pub at: f64,
    pub status: String,
    /// The score. A save read from a `ref` line shares the body of the save
    /// it names.
    pub source: Arc<str>,
    pub error: Option<String>,
    /// What made the save when no keystroke did: `slider`, `stop`, `replay`.
    pub via: Option<String>,
}

impl SessionSave {
    /// Whether the engine installed this save when it was recorded. Only a
    /// save marked `rejected` did not.
    pub fn installed(&self) -> bool {
        self.status != SaveStatus::Rejected.as_str()
    }
}

/// A recorded set, ready to replay.
#[derive(Debug, Clone)]
pub struct SessionScript {
    /// What the tape kept: `installed-saves` or `all-saves-and-diagnostics`.
    pub keeps: String,
    /// When the tape was started, as the header says it.
    pub recorded: Option<String>,
    /// The tempo the CLI started with, if the recording carried one. A score
    /// that calls `setCpm` sets its own and this is not it.
    pub baseline_cps: Option<f64>,
    pub saves: Vec<SessionSave>,
    /// Diagnostics kept alongside the saves (debug recordings only).
    pub logs: usize,
}

impl SessionScript {
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_with_empty(path, false)
    }

    /// The Studio can open an empty tape to add its first block. Playback
    /// callers retain `load`, which still requires something to replay.
    pub fn load_for_editing(path: &Path) -> Result<Self, String> {
        Self::load_with_empty(path, true)
    }

    fn load_with_empty(path: &Path, allow_empty: bool) -> Result<Self, String> {
        let file =
            File::open(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let mut reader = BufReader::new(file);
        let header = read_session_line(&mut reader, path, 1)?
            .ok_or_else(|| format!("{} is empty", path.display()))?;
        let header: serde_json::Value = serde_json::from_str(&header)
            .map_err(|error| format!("{} has no session header: {error}", path.display()))?;
        // A version number is the whole check: a file that opens with a JSON
        // object carrying one is a session, and anything else says so plainly
        // rather than failing later on a missing field. Earlier headers are
        // still accepted - tapes were already recorded with them, and refusing
        // to replay someone's set over a renamed field would be a poor trade.
        let legacy_format = header.get("format").and_then(|format| format.as_str());
        let recognised = header.get("version").and_then(|v| v.as_u64()).is_some()
            || header.get("strudel_session").is_some()
            || matches!(legacy_format, Some("strudel-session" | "rustel-session"));
        if !recognised {
            return Err(format!(
                "{} is not a {} session file",
                path.display(),
                product::NAME
            ));
        }

        let mut saves = Vec::new();
        let mut logs = 0usize;
        for line_number in 2.. {
            let Some(line) = read_session_line(&mut reader, path, line_number)? else {
                break;
            };
            if line.trim().is_empty() {
                continue;
            }
            // A set interrupted mid-write leaves a partial last line; the rest
            // of the tape is still perfectly good.
            let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match record.get("kind").and_then(|kind| kind.as_str()) {
                Some("save") => {
                    let source = if let Some(reference) =
                        record.get("ref").and_then(|reference| reference.as_u64())
                    {
                        // A reference names an earlier save by position on
                        // the tape and shares its body; a dangling one is a
                        // corrupt line.
                        let Some(earlier) = saves.get(reference as usize) else {
                            return Err(format!(
                                "{} line {}: ref {reference} points past the saves before it",
                                path.display(),
                                line_number
                            ));
                        };
                        let earlier: &SessionSave = earlier;
                        Arc::clone(&earlier.source)
                    } else {
                        let Some(source) = record.get("source").and_then(|s| s.as_str()) else {
                            continue;
                        };
                        let decoded = decode_base64(source).map_err(|error| {
                            format!("{} line {}: {error}", path.display(), line_number)
                        })?;
                        let decoded = String::from_utf8(decoded).map_err(|_| {
                            format!(
                                "{} line {}: source is not UTF-8",
                                path.display(),
                                line_number
                            )
                        })?;
                        Arc::from(decoded)
                    };
                    saves.push(SessionSave {
                        at: record.get("t").and_then(|t| t.as_f64()).unwrap_or(0.0),
                        status: record
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or(SaveStatus::Installed.as_str())
                            .to_string(),
                        source,
                        error: record
                            .get("error")
                            .and_then(|e| e.as_str())
                            .map(str::to_string),
                        via: record
                            .get("via")
                            .and_then(|via| via.as_str())
                            .map(str::to_string),
                    });
                }
                Some("log") => logs += 1,
                _ => {}
            }
        }
        if saves.is_empty() && !allow_empty {
            return Err(format!("{} holds no saves to replay", path.display()));
        }
        Ok(Self {
            keeps: header
                .get("keeps")
                .or_else(|| header.get("mode"))
                .and_then(|m| m.as_str())
                .unwrap_or(SessionMode::Normal.keeps())
                .to_string(),
            recorded: header
                .get("recorded")
                .and_then(|recorded| recorded.as_str())
                .map(str::to_string),
            baseline_cps: header
                .get("baseline_cps")
                .or_else(|| header.get("cps"))
                .and_then(|c| c.as_f64()),
            saves,
            logs,
        })
    }

    /// Wall-clock length of the recorded set.
    pub fn duration(&self) -> f64 {
        self.saves.last().map(|save| save.at).unwrap_or(0.0)
    }

    /// Write the script as a tape: the header, then a line per save, a
    /// source that already appeared written as a reference to it, as the
    /// recorder writes them. Diagnostics are not carried, so a debug tape
    /// written back is a normal one; the caller decides whether that is
    /// acceptable.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let mut header = serde_json::json!({
            "keeps": self.keeps,
            "version": 2,
        });
        if let Some(recorded) = &self.recorded {
            header["recorded"] = serde_json::Value::String(recorded.clone());
        }
        if let Some(cps) = self.baseline_cps {
            header["baseline_cps"] = serde_json::Value::from(cps);
        }
        let mut text = header.to_string();
        text.push('\n');
        let mut seen: HashMap<String, usize> = HashMap::new();
        for (index, save) in self.saves.iter().enumerate() {
            let mut record = serde_json::json!({
                // Edited block lengths can be finer than the recorder's
                // millisecond clock. A rewrite preserves their exact f64.
                "t": save.at,
                "kind": "save",
                "status": save.status,
            });
            if let Some(via) = &save.via {
                record["via"] = serde_json::Value::String(via.clone());
            }
            let hash = source_revision(&save.source);
            match seen.get(&hash) {
                Some(earlier) => record["ref"] = serde_json::Value::from(*earlier),
                None => {
                    seen.insert(hash, index);
                    record["source"] =
                        serde_json::Value::String(encode_base64(save.source.as_bytes()));
                }
            }
            if let Some(error) = &save.error {
                record["error"] = serde_json::Value::String(error.clone());
            }
            text.push_str(&record.to_string());
            text.push('\n');
        }
        crate::atomic_file::replace_file(
            path,
            ".rustel-session-",
            text.as_bytes(),
            |pending, target| std::fs::rename(pending, target),
        )
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
    }

    /// Most recent installed save at or before `at`, with its position in the
    /// recording.
    pub fn installed_save_at(&self, at: f64) -> Option<(usize, &SessionSave)> {
        self.saves
            .iter()
            .enumerate()
            .rev()
            .find(|(_, save)| save.at <= at && save.installed())
    }
}

/// Seconds since the Unix epoch, or zero before it.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// `2026-08-19T18:18:04.123Z` - the whole-second form with the
/// milliseconds kept, for the studio's log file, where every line of a
/// one-second burst would otherwise share one stamp. Still sortable as
/// text: the millis are always three digits.
pub fn iso8601_utc_millis(unix_millis: i64) -> String {
    let (year, month, day, hour, minute, second) = civil_from_unix(unix_millis.div_euclid(1000));
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        unix_millis.rem_euclid(1000)
    )
}

/// `2026-08-19T18:18:04Z` - sortable, unambiguous, and the same clock the
/// filename uses.
pub fn iso8601_utc(unix_seconds: i64) -> String {
    let (year, month, day, hour, minute, second) = civil_from_unix(unix_seconds);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn round_millis(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding - the same encoding a strudel.cc link carries,
/// so a recorded save can be pasted between the two.
pub fn encode_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
        out.push(ALPHABET[b0 >> 2] as char);
        out.push(ALPHABET[((b0 & 0b11) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((b1 & 0b1111) << 2) | (b2 >> 6)] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[b2 & 0b111111] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(format!("invalid base64 character {:?}", byte as char)),
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Ok(out)
}

/// Where tapes land when no file is named: `$<PRODUCT>_SESSION_DIR`, else
/// `sessions/` in rustel's own folder - `~/<data dir>/sessions`, unless
/// `RUSTEL_CONFIG_DIR` moves the folder - else the working directory.
pub fn default_session_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(product::SESSION_DIRECTORY_ENV) {
        return PathBuf::from(dir);
    }
    // The same folder the settings, sets and sample cache are in: resolved
    // here on its own, it once put the tapes somewhere the settings were not.
    match crate::config_dir::canonical() {
        Some(directory) => directory.join(product::SESSIONS_DIRECTORY_NAME),
        // No home to write to: keep the tape beside the score rather than
        // silently dropping it.
        None => PathBuf::from("."),
    }
}

/// The score's name and UTC recording time form a sortable session filename.
/// The header carries neither the name nor the score's path: the filename
/// holds the name, and the path must not travel with the tape.
///
/// Colons are not legal in a Windows filename, and tapes are copied between
/// machines, so the timestamp uses dashes throughout.
pub fn default_session_filename(unix_seconds: i64, score: Option<&Path>) -> String {
    let stem = score
        .and_then(|path| path.file_stem())
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "session".to_string());
    format!(
        "{stem}-{}{}",
        dashed_timestamp(unix_seconds),
        product::SESSION_FILE_SUFFIX
    )
}

/// The name a studio launch without an explicit score writes: just the UTC
/// time, so the newest set sorts last in a folder full of them. Same dash-only
/// timestamp as the tapes above, for the same cross-platform reason.
pub fn default_score_filename(unix_seconds: i64) -> String {
    format!(
        "{}{}",
        dashed_timestamp(unix_seconds),
        product::SCORE_FILE_SUFFIX
    )
}

/// `2026-08-18T14-32-05` - the sortable, colon-free form shared by tape and
/// score filenames.
pub fn dashed_timestamp(unix_seconds: i64) -> String {
    let (year, month, day, hour, minute, second) = civil_from_unix(unix_seconds);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{minute:02}-{second:02}")
}

/// Days-since-epoch → civil date (Howard Hinnant's `civil_from_days`).
fn civil_from_unix(unix_seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix_seconds.div_euclid(86_400);
    let seconds = unix_seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (
        year,
        month,
        day,
        (seconds / 3600) as u32,
        ((seconds % 3600) / 60) as u32,
        (seconds % 60) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    mod line_reads {
        use super::*;

        #[test]
        fn a_large_recorded_source_still_replays() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("large.rustel-session");
            let source = "x".repeat(16 * 1024 * 1024);
            let mut recorder =
                SessionRecorder::create(path.clone(), SessionMode::Normal, None).unwrap();
            recorder.record_save(0.0, SaveStatus::Installed, &source, None);
            drop(recorder);

            let script = SessionScript::load(&path).unwrap();
            assert_eq!(&*script.saves[0].source, source);
        }

        #[test]
        fn a_limit_sized_line_accepts_lf_crlf_or_eof() {
            let path = Path::new("boundary.rustel-session");
            for ending in ["\n", "\r\n", ""] {
                let mut input = vec![b'x'; MAX_SESSION_LINE_BYTES];
                input.extend_from_slice(ending.as_bytes());
                if !ending.is_empty() {
                    input.extend_from_slice(b"next\n");
                }
                let mut reader = std::io::Cursor::new(input);
                let line = read_session_line(&mut reader, path, 1).unwrap().unwrap();
                assert_eq!(line.len(), MAX_SESSION_LINE_BYTES);
                let next = read_session_line(&mut reader, path, 2).unwrap();
                assert_eq!(next.as_deref(), (!ending.is_empty()).then_some("next"));
                assert!(read_session_line(&mut reader, path, 3).unwrap().is_none());
            }
        }

        #[test]
        fn a_newline_free_oversized_event_is_rejected_with_its_line_number() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("oversized.rustel-session");
            let mut recorder =
                SessionRecorder::create(path.clone(), SessionMode::Normal, None).unwrap();
            recorder.record_save(0.0, SaveStatus::Installed, "s(\"bd\")", None);
            drop(recorder);

            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            let chunk = [b'x'; 1024 * 1024];
            for _ in 0..(MAX_SESSION_LINE_BYTES / chunk.len()) {
                file.write_all(&chunk).unwrap();
            }
            file.write_all(b"x").unwrap();
            drop(file);

            let error = SessionScript::load(&path).unwrap_err();
            assert!(error.contains("line 3"), "{error}");
            assert!(
                error.contains("33554432 byte session line limit"),
                "{error}"
            );
        }
    }

    /// Only a save marked `rejected` reads as not installed.
    #[test]
    fn only_a_rejected_save_is_not_installed() {
        let save = |status: &str| SessionSave {
            at: 0.0,
            status: status.into(),
            source: "".into(),
            error: None,
            via: None,
        };
        assert!(save("installed").installed());
        assert!(!save("rejected").installed());
        assert!(save("pending").installed());
    }

    #[test]
    fn a_valid_empty_tape_opens_for_editing_but_not_cli_playback() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.rustel-session");
        let recorder =
            SessionRecorder::create_unique(path.clone(), SessionMode::Normal, None).unwrap();
        drop(recorder);
        assert!(SessionScript::load(&path).is_err());
        assert!(
            SessionScript::load_for_editing(&path)
                .unwrap()
                .saves
                .is_empty()
        );
        std::fs::write(&path, "{}\n").unwrap();
        assert!(
            SessionScript::load_for_editing(&path).is_err(),
            "editing still requires a recognized header"
        );
    }

    #[test]
    fn unique_creation_preserves_collisions_and_flushes_a_complete_header() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live.set.rustel-session");
        let second = directory.path().join("live.set-2.rustel-session");
        std::fs::write(&path, "first tape must survive\n").unwrap();
        std::fs::write(&second, "second tape must survive\n").unwrap();
        let mut recorder =
            SessionRecorder::create_unique(path.clone(), SessionMode::Normal, Some(0.75)).unwrap();
        assert_eq!(
            recorder.path(),
            directory.path().join("live.set-3.rustel-session")
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first tape must survive\n"
        );
        assert_eq!(
            std::fs::read_to_string(&second).unwrap(),
            "second tape must survive\n"
        );
        let header = std::fs::read_to_string(recorder.path()).unwrap();
        assert!(header.ends_with('\n'));
        let header: serde_json::Value = serde_json::from_str(&header).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["baseline_cps"], 0.75);
        recorder
            .try_record_save_via(
                0.0,
                SaveStatus::Installed,
                "$: s(\"bd\")",
                None,
                Some("session"),
            )
            .unwrap();
        recorder
            .try_record_control(0.0, serde_json::json!({ "fader_db": -6.0 }))
            .unwrap();
        let script = SessionScript::load(recorder.path()).unwrap();
        assert_eq!(&*script.saves[0].source, "$: s(\"bd\")");
        assert_eq!(script.saves[0].via.as_deref(), Some("session"));
    }

    #[test]
    fn simultaneous_unique_recorders_never_share_or_truncate_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("same-second.rustel-session");
        let barrier = std::sync::Barrier::new(4);
        let paths = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|index| {
                    let barrier = &barrier;
                    let path = &path;
                    scope.spawn(move || {
                        barrier.wait();
                        let mut recorder =
                            SessionRecorder::create_unique(path.clone(), SessionMode::Debug, None)
                                .unwrap();
                        recorder
                            .try_record_save_via(
                                0.0,
                                SaveStatus::Installed,
                                &format!("source {index}"),
                                None,
                                Some("session"),
                            )
                            .unwrap();
                        (recorder.path().to_owned(), index)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let distinct: std::collections::HashSet<_> = paths.iter().map(|(path, _)| path).collect();
        assert_eq!(distinct.len(), 4);
        for (path, index) in paths {
            let script = SessionScript::load(&path).unwrap();
            assert_eq!(script.saves.len(), 1);
            assert_eq!(&*script.saves[0].source, format!("source {index}"));
            assert_eq!(
                script.logs, 1,
                "the debug identity is initialized before success"
            );
        }
    }

    #[test]
    fn unique_creation_returns_parent_errors_without_trying_another_name() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("a-file");
        std::fs::write(&parent, "unchanged").unwrap();
        let path = parent.join("new.rustel-session");
        assert!(SessionRecorder::create_unique(path, SessionMode::Normal, None).is_err());
        assert_eq!(std::fs::read_to_string(&parent).unwrap(), "unchanged");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn recorder_initialization_propagates_a_header_write_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("read-only-handle.rustel-session");
        std::fs::write(&path, "unchanged").unwrap();
        // A read-only handle reliably refuses writes on every platform,
        // including privileged test runners that bypass file mode bits.
        let file = File::open(&path).unwrap();
        assert!(SessionRecorder::from_file(file, path.clone(), SessionMode::Normal, None).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "unchanged");
    }

    #[test]
    fn failed_checked_and_ordinary_saves_never_advance_reference_indices() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("checked.rustel-session");
        let mut recorder =
            SessionRecorder::create_unique(path.clone(), SessionMode::Normal, None).unwrap();
        let header = std::fs::read(&path).unwrap();
        recorder.file = File::open(&path).unwrap();
        assert!(
            recorder
                .try_record_save_via(
                    0.0,
                    SaveStatus::Installed,
                    "same source",
                    None,
                    Some("session")
                )
                .is_err()
        );
        recorder.record_save_via(0.0, SaveStatus::Installed, "another source", None, None);
        assert!(
            recorder
                .try_record_control(0.0, serde_json::json!({ "fader_db": 0.0 }))
                .is_err()
        );
        assert_eq!(recorder.saves(), 0);
        assert!(recorder.seen.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), header);

        recorder.file = OpenOptions::new().append(true).open(&path).unwrap();
        recorder
            .try_record_save_via(1.0, SaveStatus::Installed, "same source", None, None)
            .unwrap();
        recorder
            .try_record_save_via(2.0, SaveStatus::Installed, "same source", None, None)
            .unwrap();
        assert_eq!(recorder.saves(), 2);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("\"source\"").count(), 1);
        assert!(text.contains("\"ref\":0"));
        let script = SessionScript::load(&path).unwrap();
        assert_eq!(script.saves.len(), 2);
        assert_eq!(&*script.saves[1].source, "same source");
    }

    #[test]
    fn explicitly_named_cli_creation_retains_its_existing_replace_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("explicit.rustel-session");
        std::fs::write(
            &path,
            "old bytes that must not remain after explicit creation",
        )
        .unwrap();
        let recorder = SessionRecorder::create(path.clone(), SessionMode::Normal, None).unwrap();
        assert_eq!(recorder.path(), path);
        let text = std::fs::read_to_string(&path).unwrap();
        let header: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(header["version"], 2);
        assert!(!text.contains("old bytes"));
    }

    #[test]
    fn a_repeated_source_is_written_as_a_reference_and_read_back_whole() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("set.rustel-session");
        let mut recorder =
            SessionRecorder::create(path.clone(), SessionMode::Normal, None).unwrap();
        recorder.record_save(1.0, SaveStatus::Installed, "$: s(\"bd\")", None);
        recorder.record_save(2.0, SaveStatus::Installed, "$: s(\"hh\")", None);
        recorder.record_save(3.0, SaveStatus::Installed, "$: s(\"bd\")", None);
        drop(recorder);

        let text = std::fs::read_to_string(&path).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert!(lines[3].contains("\"ref\":0"), "{}", lines[3]);
        assert!(!lines[3].contains("source"), "{}", lines[3]);
        assert_eq!(text.matches("\"source\"").count(), 2);

        let script = SessionScript::load(&path).unwrap();
        let sources = script
            .saves
            .iter()
            .map(|save| &*save.source)
            .collect::<Vec<_>>();
        assert_eq!(sources, ["$: s(\"bd\")", "$: s(\"hh\")", "$: s(\"bd\")"]);
        assert_eq!(script.saves[2].at, 3.0);
    }

    /// A reference shares the body it names, so a small tape of references to
    /// one large source loads in the memory of that source.
    #[test]
    fn a_reference_shares_the_body_it_names() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("references.rustel-session");
        let source = "x".repeat(1024 * 1024);
        let mut tape = format!(
            "{{\"version\":2}}\n{{\"t\":0,\"kind\":\"save\",\"source\":\"{}\"}}\n",
            encode_base64(source.as_bytes())
        );
        // Each reference names the save before it: the first names the source,
        // every later one names another reference.
        for index in 1..1024 {
            tape.push_str(&format!(
                "{{\"t\":{index},\"kind\":\"save\",\"ref\":{}}}\n",
                index - 1
            ));
        }
        std::fs::write(&path, tape).unwrap();

        let script = SessionScript::load(&path).unwrap();
        assert_eq!(script.saves.len(), 1024);
        assert_eq!(&*script.saves[0].source, source);
        let body = script.saves[0].source.as_ptr();
        for (index, save) in script.saves.iter().enumerate() {
            assert_eq!(save.source.as_ptr(), body, "save {index} copied its body");
        }
    }

    #[test]
    fn base64_round_trips_every_byte_and_length() {
        for length in 0..=32usize {
            let bytes: Vec<u8> = (0..length).map(|i| (i * 7 + 1) as u8).collect();
            let encoded = encode_base64(&bytes);
            assert_eq!(
                decode_base64(&encoded).expect("decode"),
                bytes,
                "round trip failed at length {length}"
            );
        }
        // Every byte value, so a score with any unicode survives.
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode_base64(&encode_base64(&all)).expect("decode"), all);
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(encode_base64(b"hello"), "aGVsbG8=");
        assert_eq!(encode_base64(b"hi"), "aGk=");
        assert_eq!(
            encode_base64(b"any carnal pleas"),
            "YW55IGNhcm5hbCBwbGVhcw=="
        );
        assert_eq!(decode_base64("aGVsbG8=").expect("decode"), b"hello");
    }

    #[test]
    fn a_score_with_quotes_and_unicode_survives_the_tape() {
        let score = "setCpm(150/4)\n$: s(\"bd*4\").gain(0.6) // \u{2728} ünïcode\n";
        let encoded = encode_base64(score.as_bytes());
        let decoded = String::from_utf8(decode_base64(&encoded).expect("decode")).expect("utf8");
        assert_eq!(decoded, score);
    }

    #[test]
    fn the_default_filename_is_sortable_and_filesystem_safe() {
        // Reference points spanning the leap-year arithmetic.
        assert_eq!(
            default_session_filename(1_787_063_525, Some(Path::new("/srv/sets/song.strudel"))),
            format!("song-2026-08-18T14-32-05{}", product::SESSION_FILE_SUFFIX)
        );
        assert_eq!(
            default_session_filename(0, None),
            format!(
                "session-1970-01-01T00-00-00{}",
                product::SESSION_FILE_SUFFIX
            )
        );
        assert_eq!(
            default_session_filename(1_000_000_000, None),
            format!(
                "session-2001-09-09T01-46-40{}",
                product::SESSION_FILE_SUFFIX
            )
        );
        let name = default_session_filename(1_787_063_525, None);
        assert!(!name.contains(':'), "windows cannot hold a colon: {name}");
    }

    #[test]
    fn the_default_score_filename_is_just_a_sortable_timestamp() {
        assert_eq!(
            default_score_filename(1_787_063_525),
            format!("2026-08-18T14-32-05{}", product::SCORE_FILE_SUFFIX)
        );
        assert_eq!(
            default_score_filename(0),
            format!("1970-01-01T00-00-00{}", product::SCORE_FILE_SUFFIX)
        );
        let name = default_score_filename(1_787_063_525);
        assert!(!name.contains(':'), "windows cannot hold a colon: {name}");
    }

    #[test]
    fn iso8601_utc_millis_keeps_the_sub_second_digits_sortable() {
        // The same shape the whole-second form produces, with the millis
        // where a sort and an eye agree on them.
        assert_eq!(
            iso8601_utc_millis(1_787_869_926_123),
            "2026-08-27T22:32:06.123Z"
        );
        // Rollover into the next second keeps the whole-second part right.
        assert_eq!(
            iso8601_utc_millis(1_787_869_926_999 + 1),
            "2026-08-27T22:32:07.000Z"
        );
        // Pre-epoch instants stay readable.
        assert_eq!(iso8601_utc_millis(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn legacy_format_only_headers_remain_replayable_after_a_product_rename() {
        let dir =
            std::env::temp_dir().join(format!("rustel-session-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("legacy.strudel-session");
        std::fs::write(
            &path,
            concat!(
                "{\"format\":\"strudel-session\",\"keeps\":\"installed-saves\"}\n",
                "{\"t\":0,\"kind\":\"save\",\"status\":\"installed\",",
                "\"source\":\"cygiYmQiKQ==\"}\n"
            ),
        )
        .expect("legacy tape");

        let script = SessionScript::load(&path).expect("load legacy tape");
        assert_eq!(script.saves.len(), 1);
        assert_eq!(&*script.saves[0].source, "s(\"bd\")");

        let foreign = dir.join("foreign.session");
        std::fs::write(&foreign, "{}\n").expect("foreign tape");
        let error = SessionScript::load(&foreign).expect_err("foreign header must be refused");
        let suffix = format!(" is not a {} session file", product::NAME);
        assert!(error.ends_with(&suffix), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normal_mode_keeps_the_performance_and_debug_keeps_diagnostics() {
        let dir = std::env::temp_dir().join(format!("rustel-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        for (mode, expected_saves) in [(SessionMode::Normal, 1), (SessionMode::Debug, 2)] {
            let path = dir.join(format!("{}.rustel-session", mode.as_str()));
            let mut recorder = SessionRecorder::create(path.clone(), mode, None).expect("recorder");
            recorder.record_save(0.0, SaveStatus::Installed, "s(\"bd\")", None);
            recorder.record_save(1.5, SaveStatus::Rejected, "s(\"bd\"", Some("syntax"));
            recorder.record_log(1.6, r#"{"live_error":{"kind":"evaluation"}}"#);
            drop(recorder);

            let script = SessionScript::load(&path).expect("load");
            assert_eq!(
                script.saves.len(),
                expected_saves,
                "{} kept the wrong saves",
                mode.as_str()
            );
            assert_eq!(&*script.saves[0].source, "s(\"bd\")");
            // A debug tape opens with the engine diagnostic and keeps the
            // one recorded below it; a normal tape keeps neither.
            assert_eq!(script.logs, if mode == SessionMode::Debug { 2 } else { 0 });
            if mode == SessionMode::Debug {
                assert_eq!(script.saves[1].status, "rejected");
                assert_eq!(script.saves[1].error.as_deref(), Some("syntax"));
                assert!((script.duration() - 1.5).abs() < 1e-9);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tape_truncated_by_a_crash_still_replays_what_it_holds() {
        let dir = std::env::temp_dir().join(format!("rustel-session-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("cut.rustel-session");
        let mut recorder =
            SessionRecorder::create(path.clone(), SessionMode::Normal, None).expect("recorder");
        recorder.record_save(0.0, SaveStatus::Installed, "s(\"bd\")", None);
        recorder.record_save(2.0, SaveStatus::Installed, "s(\"sd\")", None);
        drop(recorder);

        // The process died mid-line: keep everything before the tear.
        let mut text = std::fs::read_to_string(&path).expect("read");
        text.push_str("{\"t\":3.0,\"kind\":\"sa");
        std::fs::write(&path, text).expect("write");

        let script = SessionScript::load(&path).expect("a torn tape must still load");
        assert_eq!(script.saves.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
