//! TCP control for Studio, enabled from the CLI or Options menu.
//!
//! Up to eight clients can authenticate once with `auth <token>`, then send
//! `key <chord>` or `screen`. Replies are `ok`, `err <message>`, or
//! `screen <json>`. The token grants full UI control. TCP is plaintext, so
//! use a trusted network or an encrypted tunnel.
//!
//! Each client has a worker thread. A worker passes key and screen requests to
//! the Studio event loop through a bounded queue. If the queue is full, the
//! worker replies `err busy`.
//!
//! ```text
//! client               worker thread          Studio event loop
//!   auth <token>  -->  authenticate
//!                 <--  ok
//!   key <chord>   -->  try_send(Key)     -->  poll: App handles the key
//!                 <--  ok (the key is queued)
//!   screen        -->  try_send(Screen)  -->  poll: queue_screen
//!                      wait_for_screen   <--  JSON of the next drawn frame
//!                 <--  screen <json>
//! ```

use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
#[cfg(test)]
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use tempfile::NamedTempFile;

#[path = "remote_token.rs"]
mod token_file;

/// Port `--remote-control` binds when no port is given, on IPv4 loopback.
pub const REMOTE_PORT: u16 = 9247;

/// A port binds `127.0.0.1`; an explicit IP address binds that interface.
/// Hostnames are not accepted.
pub fn parse_bind(text: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("expected a port or ip:port".into());
    }
    let address = if text.bytes().all(|byte| byte.is_ascii_digit()) {
        let port: u16 = text.parse().map_err(|_| format!("bad port {text}"))?;
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    } else {
        text.parse()
            .map_err(|_| format!("expected a port or ip:port, got {text}"))?
    };
    if address.port() == 0 {
        return Err("port must be 1..=65535".into());
    }
    Ok(address)
}

/// Check a custom token from the CLI or Studio.
pub fn parse_auth_token(text: &str) -> Result<String, String> {
    if !(4..=256).contains(&text.len()) || !text.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("Token must be 4 to 256 ASCII characters without spaces".into());
    }
    Ok(text.to_owned())
}

/// How often the studio looks for a command while the socket is open.
pub(crate) const POLL: Duration = Duration::from_millis(15);

const SCREEN_WAIT: Duration = Duration::from_secs(30);
const READ_WAIT: Duration = Duration::from_millis(200);
const WRITE_WAIT: Duration = Duration::from_secs(1);
const AUTH_WAIT: Duration = Duration::from_secs(3);
const IDLE_WAIT: Duration = Duration::from_secs(60);
const UNAUTHENTICATED_PAUSE: Duration = Duration::from_millis(200);
const LINE_WAIT: Duration = Duration::from_secs(5);
const LINE_LIMIT: usize = 1024;
const QUEUE_LIMIT: usize = 64;
const BATCH: usize = 8;
const MAX_CLIENTS: usize = 8;
// 60 random bits per activation; failed attempts trigger a shared pause.
const GENERATED_TOKEN_SYMBOLS: usize = 12;
// Crockford-style Base32 omits I, L, O, and U.
const GENERATED_TOKEN_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

type ActiveClients = Arc<Mutex<Vec<(usize, TcpStream)>>>;

pub(crate) enum RemoteEvent {
    Key(KeyEvent),
    Screen(Sender<String>),
}

pub(crate) struct Remote {
    rx: Option<Receiver<RemoteEvent>>,
    address: SocketAddr,
    token: String,
    token_file: Option<NamedTempFile>,
    stop: Arc<AtomicBool>,
    active_clients: ActiveClients,
    thread: Option<JoinHandle<()>>,
    pub(crate) pending_screens: Vec<Sender<String>>,
    pub(crate) activity_until: Option<Instant>,
}

impl Remote {
    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    #[cfg(test)]
    pub(crate) fn token_path(&self) -> Option<&Path> {
        self.token_file.as_ref().map(NamedTempFile::path)
    }

    pub(crate) fn poll(&self) -> Vec<RemoteEvent> {
        let Some(rx) = self.rx.as_ref() else {
            return Vec::new();
        };
        rx.try_iter().take(BATCH).collect()
    }

    pub(crate) fn queue_screen(&mut self, reply: Sender<String>) {
        // Bound retries that pile up while Studio cannot draw a frame.
        if self.pending_screens.len() == QUEUE_LIMIT {
            self.pending_screens.remove(0);
        }
        self.pending_screens.push(reply);
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.pending_screens.clear();
        self.rx.take();
        // Wake blocked reads and writes, even when a peer reads very slowly.
        for (_, stream) in self
            .active_clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        // Remove the token file after all workers stop.
        drop(self.token_file.take());
    }
}

pub(crate) fn listen(address: SocketAddr, explicit_token: Option<&str>) -> io::Result<Remote> {
    let explicit_token = explicit_token
        .map(|token| {
            parse_auth_token(token)
                .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))
        })
        .transpose()?;
    let generated_token = explicit_token.is_none();
    let listener = TcpListener::bind(address)?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let (token, token_file) = match explicit_token {
        Some(token) => (token, None),
        None => {
            let (token, file) = create_token()?;
            (token, Some(file))
        }
    };
    let (tx, rx) = mpsc::sync_channel(QUEUE_LIMIT);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let active_clients = Arc::new(Mutex::new(Vec::new()));
    let thread_clients = Arc::clone(&active_clients);
    let thread_token = token.clone();
    let thread = thread::Builder::new()
        .name("studio-remote".into())
        .spawn(move || {
            serve(
                || listener.accept(),
                tx,
                thread_stop,
                thread_clients,
                thread_token,
                generated_token,
            )
        })?;
    Ok(Remote {
        rx: Some(rx),
        address,
        token,
        token_file,
        stop,
        active_clients,
        thread: Some(thread),
        pending_screens: Vec::new(),
        activity_until: None,
    })
}

fn create_token() -> io::Result<(String, NamedTempFile)> {
    let mut bytes = [0_u8; GENERATED_TOKEN_SYMBOLS];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    let mut token = String::with_capacity(GENERATED_TOKEN_SYMBOLS + 2);
    for (index, byte) in bytes.into_iter().enumerate() {
        if index > 0 && index % 4 == 0 {
            token.push('-');
        }
        // All 32 symbols are equally likely.
        token.push(GENERATED_TOKEN_ALPHABET[usize::from(byte & 0b11111)] as char);
    }
    let file = token_file::create(&token)?;
    Ok((token, file))
}

enum ClientEnd {
    Unauthenticated,
    Done,
}

impl ClientEnd {
    fn closed(authenticated: bool) -> Self {
        if authenticated {
            Self::Done
        } else {
            Self::Unauthenticated
        }
    }
}

fn serve(
    mut accept: impl FnMut() -> io::Result<(TcpStream, SocketAddr)>,
    tx: SyncSender<RemoteEvent>,
    stop: Arc<AtomicBool>,
    active_clients: ActiveClients,
    token: String,
    generated_token: bool,
) {
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    let mut next_client = 0_usize;
    let auth_cooldown = Arc::new(Mutex::new(Instant::now()));
    while !stop.load(Ordering::Relaxed) {
        // Finished workers no longer count toward the client limit.
        let mut index = 0;
        while index < workers.len() {
            if workers[index].is_finished() {
                workers.swap_remove(index).join().ok();
            } else {
                index += 1;
            }
        }
        let stream = match accept() {
            Ok((stream, _)) => stream,
            Err(_) => {
                // No client is waiting, or accept failed. Wait and try again: a
                // failed connection or a temporary resource shortage can recover.
                thread::park_timeout(READ_WAIT);
                continue;
            }
        };
        if workers.len() >= MAX_CLIENTS {
            let mut stream = stream;
            let _ = stream.set_write_timeout(Some(WRITE_WAIT));
            let _ = writeln!(stream, "err busy");
            continue;
        }
        let Ok(interrupt_stream) = stream.try_clone() else {
            continue;
        };
        let id = next_client;
        next_client = next_client.wrapping_add(1);
        active_clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((id, interrupt_stream));
        let worker_tx = tx.clone();
        let worker_stop = Arc::clone(&stop);
        let worker_clients = Arc::clone(&active_clients);
        let worker_token = token.clone();
        let worker_cooldown = Arc::clone(&auth_cooldown);
        match thread::Builder::new()
            .name("studio-remote-client".into())
            .spawn(move || {
                // Failed logins delay new clients. Connected clients keep working.
                loop {
                    if worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let deadline = *worker_cooldown.lock().unwrap_or_else(|p| p.into_inner());
                    if Instant::now() >= deadline {
                        let result = handle_client(
                            stream,
                            &worker_tx,
                            &worker_stop,
                            &worker_token,
                            generated_token,
                            AUTH_WAIT,
                            IDLE_WAIT,
                        );
                        if matches!(result, ClientEnd::Unauthenticated) {
                            let mut deadline =
                                worker_cooldown.lock().unwrap_or_else(|p| p.into_inner());
                            *deadline = Instant::now() + UNAUTHENTICATED_PAUSE;
                        }
                        break;
                    }
                    thread::park_timeout(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(READ_WAIT),
                    );
                }
                worker_clients
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .retain(|(client_id, _)| *client_id != id);
            }) {
            Ok(worker) => workers.push(worker),
            Err(_) => {
                active_clients
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .retain(|(client_id, _)| *client_id != id);
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    for worker in workers {
        worker.thread().unpark();
        let _ = worker.join();
    }
}

fn handle_client(
    mut stream: TcpStream,
    tx: &SyncSender<RemoteEvent>,
    stop: &AtomicBool,
    token: &str,
    generated_token: bool,
    auth_wait: Duration,
    idle_wait: Duration,
) -> ClientEnd {
    if stream.set_nonblocking(false).is_err()
        || stream.set_nodelay(true).is_err()
        || stream.set_read_timeout(Some(READ_WAIT)).is_err()
        || stream.set_write_timeout(Some(WRITE_WAIT)).is_err()
    {
        return ClientEnd::Unauthenticated;
    }
    let Ok(reader_stream) = stream.try_clone() else {
        return ClientEnd::Unauthenticated;
    };
    let mut reader = BufReader::with_capacity(LINE_LIMIT, reader_stream);
    let mut line = Vec::with_capacity(LINE_LIMIT);
    let auth_deadline = Instant::now() + auth_wait;
    let mut authenticated = false;
    let mut idle_deadline = None;
    let mut line_deadline = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            return ClientEnd::closed(authenticated);
        }
        if !authenticated && Instant::now() >= auth_deadline {
            let _ = writeln!(stream, "err authentication required");
            return ClientEnd::Unauthenticated;
        }
        if idle_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let _ = writeln!(stream, "err idle timeout");
            return ClientEnd::Done;
        }
        if line_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let _ = writeln!(stream, "err incomplete command");
            return ClientEnd::closed(authenticated);
        }
        match read_bounded_line(&mut reader, &mut line) {
            Ok(LineRead::Closed) => return ClientEnd::closed(authenticated),
            Ok(LineRead::TooLong) => {
                let _ = writeln!(stream, "err line too long");
                return ClientEnd::closed(authenticated);
            }
            Ok(LineRead::Partial) => {
                line_deadline.get_or_insert_with(|| Instant::now() + LINE_WAIT);
                continue;
            }
            Ok(LineRead::Complete) => {
                line_deadline = None;
            }
            Err(error) if paused(&error) => continue,
            Err(_) => return ClientEnd::closed(authenticated),
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            let _ = writeln!(stream, "err invalid utf-8");
            return ClientEnd::closed(authenticated);
        };
        let text = text.trim_end_matches(['\r', '\n']);
        if !authenticated {
            if Instant::now() >= auth_deadline || !authenticate(text, token, generated_token) {
                let _ = writeln!(stream, "err authentication required");
                return ClientEnd::Unauthenticated;
            }
            authenticated = true;
            if writeln!(stream, "ok").is_err() {
                return ClientEnd::Done;
            }
            idle_deadline = Some(Instant::now() + idle_wait);
            line.clear();
            continue;
        }
        let reply = match command(text) {
            Ok(Command::Key(key)) => match tx.try_send(RemoteEvent::Key(key)) {
                Ok(()) => "ok".to_owned(),
                Err(TrySendError::Full(_)) => "err busy".to_owned(),
                Err(TrySendError::Disconnected(_)) => return ClientEnd::Done,
            },
            Ok(Command::Screen) => {
                let (reply_tx, reply_rx) = mpsc::channel();
                match tx.try_send(RemoteEvent::Screen(reply_tx)) {
                    Ok(()) => match wait_for_screen(&reply_rx, stop) {
                        Some(json) => format!("screen {json}"),
                        None => "err no screen".to_owned(),
                    },
                    Err(TrySendError::Full(_)) => "err busy".to_owned(),
                    Err(TrySendError::Disconnected(_)) => return ClientEnd::Done,
                }
            }
            Err(message) => format!("err {message}"),
        };
        if stop.load(Ordering::Relaxed) || writeln!(stream, "{reply}").is_err() {
            return ClientEnd::Done;
        }
        idle_deadline = Some(Instant::now() + idle_wait);
        line.clear();
    }
}

/// Compare the fixed-size secret without returning on its first differing byte.
/// Generated codes tolerate uppercase letters and omitted group hyphens;
/// caller-supplied tokens retain exact matching.
fn authenticate(line: &str, token: &str, generated_token: bool) -> bool {
    let Some(candidate) = line.strip_prefix("auth ") else {
        return false;
    };
    if generated_token {
        return match candidate.len() {
            GENERATED_TOKEN_SYMBOLS => compare_token(
                candidate.bytes(),
                token.bytes().filter(|byte| *byte != b'-'),
                true,
            ),
            len if len == token.len() => compare_token(candidate.bytes(), token.bytes(), true),
            _ => false,
        };
    }
    candidate.len() == token.len() && compare_token(candidate.bytes(), token.bytes(), false)
}

fn compare_token(
    candidate: impl Iterator<Item = u8>,
    expected: impl Iterator<Item = u8>,
    fold_case: bool,
) -> bool {
    candidate.zip(expected).fold(0, |different, (left, right)| {
        let left = if fold_case {
            left.to_ascii_lowercase()
        } else {
            left
        };
        different | (left ^ right)
    }) == 0
}

enum LineRead {
    Complete,
    Partial,
    Closed,
    TooLong,
}

/// Never append more than LINE_LIMIT bytes, and keep partial input intact when
/// a socket times out. Only a newline terminates a command; EOF never executes it.
fn read_bounded_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<LineRead> {
    // Return after each available fragment so the caller checks stop and
    // deadlines even when a peer continuously supplies an incomplete line.
    let available = reader.fill_buf()?;
    if available.is_empty() {
        return Ok(LineRead::Closed);
    }
    let newline = available.iter().position(|byte| *byte == b'\n');
    let take = newline.map_or(available.len(), |index| index + 1);
    if take > LINE_LIMIT - line.len() {
        return Ok(LineRead::TooLong);
    }
    line.extend_from_slice(&available[..take]);
    reader.consume(take);
    if newline.is_some() {
        Ok(LineRead::Complete)
    } else if line.len() == LINE_LIMIT {
        Ok(LineRead::TooLong)
    } else {
        Ok(LineRead::Partial)
    }
}

fn wait_for_screen(reply: &Receiver<String>, stop: &AtomicBool) -> Option<String> {
    let deadline = Instant::now() + SCREEN_WAIT;
    while !stop.load(Ordering::Relaxed) {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        match reply.recv_timeout(remaining.min(READ_WAIT)) {
            Ok(json) => return Some(json),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

fn paused(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

enum Command {
    Key(KeyEvent),
    Screen,
}

fn command(line: &str) -> Result<Command, &'static str> {
    let line = line.trim();
    if line == "screen" {
        return Ok(Command::Screen);
    }
    if let Some(spec) = line.strip_prefix("key ") {
        return Ok(Command::Key(parse_key(spec)?));
    }
    if line == "key" {
        return Err("missing key");
    }
    Err("unknown command")
}

fn parse_key(spec: &str) -> Result<KeyEvent, &'static str> {
    let spec = spec.trim();
    if spec.is_empty() || spec.len() > 64 {
        return Err("missing key");
    }
    let mut modifiers = KeyModifiers::NONE;
    let mut rest = spec;
    while let Some((name, modifier)) = modifier_prefix(rest) {
        modifiers |= modifier;
        rest = &rest[name.len()..];
    }
    if rest.is_empty() {
        return Err("missing key");
    }
    let mut code = key_code(rest, !modifiers.is_empty())?;
    if code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT) {
        code = KeyCode::BackTab;
        modifiers -= KeyModifiers::SHIFT;
    }
    if modifiers.contains(KeyModifiers::SHIFT)
        && let KeyCode::Char(character) = code
        && character.is_ascii_lowercase()
    {
        code = KeyCode::Char(character.to_ascii_uppercase());
    }
    let event = KeyEvent::new(code, modifiers);
    debug_assert_eq!(event.kind, KeyEventKind::Press);
    Ok(event)
}

const PREFIXES: &[(&str, KeyModifiers)] = &[
    ("control+", KeyModifiers::CONTROL),
    ("control-", KeyModifiers::CONTROL),
    ("option+", KeyModifiers::ALT),
    ("option-", KeyModifiers::ALT),
    ("shift+", KeyModifiers::SHIFT),
    ("shift-", KeyModifiers::SHIFT),
    ("super+", KeyModifiers::SUPER),
    ("super-", KeyModifiers::SUPER),
    ("ctrl+", KeyModifiers::CONTROL),
    ("ctrl-", KeyModifiers::CONTROL),
    ("meta+", KeyModifiers::META),
    ("meta-", KeyModifiers::META),
    ("alt+", KeyModifiers::ALT),
    ("alt-", KeyModifiers::ALT),
    ("cmd+", KeyModifiers::SUPER),
    ("cmd-", KeyModifiers::SUPER),
    ("opt+", KeyModifiers::ALT),
    ("opt-", KeyModifiers::ALT),
];

fn modifier_prefix(spec: &str) -> Option<(&'static str, KeyModifiers)> {
    let lower = spec.to_ascii_lowercase();
    PREFIXES
        .iter()
        .copied()
        .filter(|(name, _)| lower.starts_with(name))
        .max_by_key(|(name, _)| name.len())
}

fn key_code(name: &str, fold_ascii: bool) -> Result<KeyCode, &'static str> {
    let lower = name.to_ascii_lowercase();
    let named = match lower.as_str() {
        "enter" | "return" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "space" => Some(KeyCode::Char(' ')),
        "tab" => Some(KeyCode::Tab),
        "backtab" => Some(KeyCode::BackTab),
        "backspace" | "bs" => Some(KeyCode::Backspace),
        "delete" | "del" => Some(KeyCode::Delete),
        "insert" | "ins" => Some(KeyCode::Insert),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" | "page-up" | "pgup" => Some(KeyCode::PageUp),
        "pagedown" | "page-down" | "pgdn" => Some(KeyCode::PageDown),
        other => function_key(other),
    };
    if let Some(code) = named {
        return Ok(code);
    }
    let mut chars = name.chars();
    let Some(mut character) = chars.next() else {
        return Err("missing key");
    };
    if chars.next().is_some() || character.is_control() {
        return Err("unknown key");
    }
    if fold_ascii {
        character = character.to_ascii_lowercase();
    }
    Ok(KeyCode::Char(character))
}

fn function_key(name: &str) -> Option<KeyCode> {
    let number = name.strip_prefix('f')?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let number: u8 = number.parse().ok()?;
    (1..=24).contains(&number).then_some(KeyCode::F(number))
}

/// The cells on screen. `text` is the glyphs, one row a line, with a space
/// standing in for an empty cell. `glyph` is one string per cell, row by
/// row, and `fg` / `bg` / `ul` are `RRGGBB` in that same order. `ul` is
/// black when the cell has no underline.
pub(crate) fn screen_json(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut text = String::new();
    let mut glyph = Vec::with_capacity(usize::from(area.width) * usize::from(area.height));
    let mut fg = String::new();
    let mut bg = String::new();
    let mut ul = String::new();
    for y in area.top()..area.bottom() {
        if y != area.top() {
            text.push('\n');
        }
        for x in area.left()..area.right() {
            let Some(cell) = buffer.cell((x, y)) else {
                text.push(' ');
                glyph.push(String::new());
                push_hex(&mut fg, (0, 0, 0));
                push_hex(&mut bg, (0, 0, 0));
                push_hex(&mut ul, (0, 0, 0));
                continue;
            };
            let symbol = cell.symbol();
            if symbol.is_empty() {
                text.push(' ');
            } else {
                text.push_str(symbol);
            }
            glyph.push(symbol.to_owned());
            let (foreground, background) = if cell.modifier.contains(Modifier::REVERSED) {
                (crate::graphics::rgb(cell.bg), crate::graphics::rgb(cell.fg))
            } else {
                (crate::graphics::rgb(cell.fg), crate::graphics::rgb(cell.bg))
            };
            push_hex(&mut fg, foreground);
            push_hex(&mut bg, background);
            if cell.modifier.contains(Modifier::UNDERLINED) {
                let underline = crate::graphics::rgb(cell.underline_color);
                // Lint sets an explicit error colour. Menu mnemonics and other
                // plain underlines leave it unset (black), so they take the
                // glyph colour and do not read as errors.
                push_hex(
                    &mut ul,
                    if underline == (0, 0, 0) {
                        foreground
                    } else {
                        underline
                    },
                );
            } else {
                push_hex(&mut ul, (0, 0, 0));
            }
        }
    }
    serde_json::json!({
        "cols": area.width,
        "rows": area.height,
        "text": text,
        "glyph": glyph,
        "fg": fg,
        "bg": bg,
        "ul": ul,
    })
    .to_string()
}

fn push_hex(out: &mut String, (red, green, blue): (u8, u8, u8)) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in [red, green, blue] {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use std::time::Duration;

    fn key(spec: &str) -> KeyEvent {
        parse_key(spec).unwrap_or_else(|error| panic!("{spec}: {error}"))
    }

    #[test]
    fn chords_become_presses() {
        let ctrl_s = key("ctrl-s");
        assert_eq!(ctrl_s.code, KeyCode::Char('s'));
        assert_eq!(ctrl_s.modifiers, KeyModifiers::CONTROL);
        assert_eq!(ctrl_s.kind, KeyEventKind::Press);

        let ctrl_s_upper = key("Ctrl+S");
        assert_eq!(ctrl_s_upper.code, KeyCode::Char('s'));
        assert_eq!(ctrl_s_upper.modifiers, KeyModifiers::CONTROL);

        let capital = key("A");
        assert_eq!(capital.code, KeyCode::Char('A'));
        assert!(capital.modifiers.is_empty());

        let shifted = key("shift-a");
        assert_eq!(shifted.code, KeyCode::Char('A'));
        assert_eq!(shifted.modifiers, KeyModifiers::SHIFT);

        let rewind = key("ctrl-shift-s");
        assert_eq!(rewind.code, KeyCode::Char('S'));
        assert_eq!(
            rewind.modifiers,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        );

        assert_eq!(key("shift-tab").code, KeyCode::BackTab);
        assert!(key("shift-tab").modifiers.is_empty());
        assert_eq!(key("f5").code, KeyCode::F(5));
        assert_eq!(key("page-up").code, KeyCode::PageUp);
        assert_eq!(key("alt-enter").code, KeyCode::Enter);
        assert!(key("alt-enter").modifiers.contains(KeyModifiers::ALT));
        assert_eq!(key("cmd-q").modifiers, KeyModifiers::SUPER);
        assert_eq!(key("-").code, KeyCode::Char('-'));
        assert_eq!(key("ctrl-+").code, KeyCode::Char('+'));
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(parse_key("").is_err());
        assert!(parse_key("nope").is_err());
        assert!(parse_key("f99").is_err());
        assert!(command("nope").is_err());
        assert!(matches!(command("screen"), Ok(Command::Screen)));
    }

    #[test]
    fn screen_json_lists_every_cell_and_swaps_reversed_colours() {
        let area = Rect::new(0, 0, 2, 1);
        let mut buffer = Buffer::empty(area);
        buffer.cell_mut((0, 0)).unwrap().set_symbol("Z").set_style(
            Style::default()
                .fg(Color::Rgb(1, 2, 3))
                .bg(Color::Rgb(4, 5, 6)),
        );
        buffer.cell_mut((1, 0)).unwrap().set_symbol(" ").set_style(
            Style::default()
                .fg(Color::Rgb(1, 2, 3))
                .bg(Color::Rgb(4, 5, 6))
                .add_modifier(Modifier::REVERSED),
        );
        let value: serde_json::Value = serde_json::from_str(&screen_json(&buffer)).unwrap();
        assert_eq!(value["cols"], 2);
        assert_eq!(value["rows"], 1);
        assert_eq!(value["text"], "Z ");
        assert_eq!(value["glyph"][0], "Z");
        assert_eq!(value["glyph"][1], " ");
        assert_eq!(value["fg"], "010203040506");
        assert_eq!(value["bg"], "040506010203");
        assert_eq!(value["ul"], "000000000000");
    }

    #[test]
    fn screen_json_keeps_underline_colour() {
        let area = Rect::new(0, 0, 1, 1);
        let mut buffer = Buffer::empty(area);
        buffer.cell_mut((0, 0)).unwrap().set_symbol("x").set_style(
            Style::default()
                .fg(Color::Rgb(1, 2, 3))
                .underline_color(Color::Rgb(200, 40, 40))
                .add_modifier(Modifier::UNDERLINED),
        );
        let value: serde_json::Value = serde_json::from_str(&screen_json(&buffer)).unwrap();
        assert_eq!(value["ul"], "c82828");
    }

    #[test]
    fn the_completed_frame_is_the_screen_the_current_buffer_is_blank() {
        let backend = ratatui::backend::TestBackend::new(4, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let completed = terminal
            .draw(|frame| {
                frame.buffer_mut().cell_mut((0, 0)).unwrap().set_symbol("P");
            })
            .unwrap();
        let shown: serde_json::Value =
            serde_json::from_str(&screen_json(completed.buffer)).unwrap();
        let cleared: serde_json::Value =
            serde_json::from_str(&screen_json(terminal.current_buffer_mut())).unwrap();
        assert_eq!(shown["text"], "P   ");
        assert_eq!(cleared["text"], "    ");
    }

    #[test]
    fn explicit_ip_addresses_are_accepted() {
        assert_eq!(
            parse_bind("9000").unwrap(),
            "127.0.0.1:9000".parse().unwrap()
        );
        assert_eq!(
            parse_bind(" 9247 ").unwrap(),
            "127.0.0.1:9247".parse().unwrap()
        );
        for address in [
            "0.0.0.0:9000",
            "[::]:9000",
            "192.168.1.10:9000",
            "8.8.8.8:9000",
        ] {
            assert_eq!(parse_bind(address).unwrap(), address.parse().unwrap());
        }
        assert_eq!(
            parse_bind("[::1]:9000").unwrap(),
            "[::1]:9000".parse().unwrap()
        );
        assert!(parse_bind("0").is_err());
        assert!(parse_bind("studio.local:9000").is_err());
        assert!(parse_bind("").is_err());
    }

    #[test]
    fn wildcard_listener_accepts_a_loopback_client() {
        let remote = listen(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)), None).unwrap();
        assert_eq!(remote.address().ip(), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, remote.address().port()));
        let token = std::fs::read_to_string(remote.token_path().unwrap()).unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        writeln!(
            stream,
            "auth {}",
            token.trim().replace('-', "").to_ascii_uppercase()
        )
        .unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert_eq!(reply, "ok\n");
    }

    #[test]
    fn accept_errors_do_not_stop_the_listener() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, _rx) = mpsc::sync_channel(QUEUE_LIMIT);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let mut errors = [
                io::ErrorKind::ConnectionAborted,
                io::ErrorKind::ConnectionReset,
                io::ErrorKind::OutOfMemory,
                io::ErrorKind::Other,
            ]
            .into_iter();
            serve(
                || match errors.next() {
                    Some(error) => Err(error.into()),
                    None => listener.accept(),
                },
                tx,
                worker_stop,
                Arc::new(Mutex::new(Vec::new())),
                "test-token".into(),
                false,
            );
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(b"auth test-token\n").unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        stop.store(true, Ordering::Relaxed);
        worker.thread().unpark();
        worker.join().unwrap();
        assert_eq!(reply, "ok\n");
    }

    #[test]
    fn a_client_presses_a_key_and_reads_the_screen() {
        let remote = listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), None).unwrap();
        let address = remote.address();
        let token = std::fs::read_to_string(remote.token_path().unwrap()).unwrap();
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.set_nodelay(true).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            writeln!(stream, "auth {}", token.trim()).unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "ok\n");
            stream.write_all(b"key ctrl-s\n").unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "ok\n");
            stream.write_all(b"nope\n").unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("err "), "{line}");
            stream.write_all(b"screen\n").unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            line
        });
        let event = remote
            .rx
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        match event {
            RemoteEvent::Key(key) => {
                assert_eq!(key.code, KeyCode::Char('s'));
                assert_eq!(key.modifiers, KeyModifiers::CONTROL);
            }
            RemoteEvent::Screen(_) => panic!("expected a key"),
        }
        let event = remote
            .rx
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        match event {
            RemoteEvent::Screen(reply) => {
                reply
                    .send(r#"{"cols":1,"rows":1,"text":"Z"}"#.to_owned())
                    .unwrap();
            }
            RemoteEvent::Key(_) => panic!("expected a screen"),
        }
        let line = client.join().unwrap();
        assert_eq!(line, "screen {\"cols\":1,\"rows\":1,\"text\":\"Z\"}\n");
    }

    #[test]
    fn a_screen_poller_does_not_starve_another_authentication() {
        let remote = listen(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            Some("test-token"),
        )
        .unwrap();
        let address = remote.address();
        let keep_polling = Arc::new(AtomicBool::new(true));
        let polling = Arc::clone(&keep_polling);
        let poller = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            writeln!(stream, "auth test-token").unwrap();
            assert_eq!(read_reply(&mut reader), "ok\n");
            while polling.load(Ordering::Relaxed) {
                writeln!(stream, "screen").unwrap();
                assert!(read_reply(&mut reader).starts_with("screen "));
            }
        });
        let first = remote
            .rx
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let RemoteEvent::Screen(reply) = first else {
            panic!("expected screen")
        };
        reply.send("{}".into()).unwrap();

        let second = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            writeln!(stream, "auth test-token").unwrap();
            read_reply(&mut reader)
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !second.is_finished() && Instant::now() < deadline {
            if let Ok(RemoteEvent::Screen(reply)) = remote
                .rx
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_millis(50))
            {
                let _ = reply.send("{}".into());
            }
        }
        assert!(
            second.is_finished(),
            "another client could not authenticate while screen polling"
        );
        assert_eq!(second.join().unwrap(), "ok\n");
        keep_polling.store(false, Ordering::Relaxed);
        if let Ok(RemoteEvent::Screen(reply)) = remote
            .rx
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_millis(100))
        {
            let _ = reply.send("{}".into());
        }
        poller.join().unwrap();
    }

    fn read_reply(reader: &mut BufReader<TcpStream>) -> String {
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        reply
    }

    fn connect(remote: &Remote, authenticate_client: bool) -> (TcpStream, BufReader<TcpStream>) {
        let mut stream = TcpStream::connect(remote.address()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        if authenticate_client {
            let token = std::fs::read_to_string(remote.token_path().unwrap()).unwrap();
            writeln!(stream, "auth {}", token.trim()).unwrap();
            let mut reply = String::new();
            reader.read_line(&mut reply).unwrap();
            assert_eq!(reply, "ok\n");
        }
        (stream, reader)
    }

    #[test]
    fn credentials_are_unique_private_and_removed_on_drop() {
        let first = listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), None).unwrap();
        let second = listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), None).unwrap();
        let token = std::fs::read_to_string(first.token_path().unwrap()).unwrap();
        let other = std::fs::read_to_string(second.token_path().unwrap()).unwrap();
        let token = token.trim();
        assert_eq!(token, first.token());
        assert_eq!(token.len(), 14);
        for (index, byte) in token.bytes().enumerate() {
            if [4, 9].contains(&index) {
                assert_eq!(byte, b'-');
            } else {
                assert!(GENERATED_TOKEN_ALPHABET.contains(&byte));
            }
        }
        assert_ne!(token, other.trim());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(first.token_path().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let path = first.token_path().unwrap().to_owned();
        drop(first);
        assert!(!path.exists());
    }

    #[test]
    fn explicit_token_is_reusable_without_a_temporary_file() {
        let token = "gig1";
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        for _ in 0..2 {
            let remote = listen(address, Some(token)).unwrap();
            assert_eq!(remote.token(), token);
            assert!(remote.token_path().is_none());
            let mut stream = TcpStream::connect(remote.address()).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            writeln!(stream, "auth {token}").unwrap();
            let mut reply = String::new();
            BufReader::new(stream).read_line(&mut reply).unwrap();
            assert_eq!(reply, "ok\n");
        }
    }

    #[test]
    fn generated_token_accepts_uppercase_and_no_hyphens_but_explicit_is_exact() {
        let token = "0123-ABCD-EFGH".to_ascii_lowercase();
        assert!(authenticate(&format!("auth {token}"), &token, true));
        assert!(authenticate("auth 0123-ABCD-EFGH", &token, true));
        assert!(authenticate("auth 0123ABCDEFGH", &token, true));
        assert!(!authenticate("auth 0123ABCDEFGJ", &token, true));
        assert!(!authenticate("auth 0123ABCDEFG", &token, true));
        assert!(!authenticate("auth 0123ABCDEFGHJ", &token, true));
        assert!(!authenticate("auth 0123-ABCDEFGH", &token, true));
        assert!(!authenticate("auth 0123ABCDEFGH", &token, false));
        assert!(!authenticate("auth 0123-ABCD-EFGH", &token, false));
    }

    #[test]
    fn explicit_token_must_fit_on_a_single_protocol_line() {
        for token in [
            "",
            "x",
            "xx",
            "xxx",
            "a token with spaces",
            "a-token-with-newline\n",
            "abcd\t",
            "abcd\x7f",
            "café",
        ] {
            assert!(parse_auth_token(token).is_err(), "accepted {token:?}");
        }
        assert!(parse_auth_token(&"x".repeat(257)).is_err());
        assert_eq!(parse_auth_token("gig1").unwrap(), "gig1");
        assert_eq!(parse_auth_token(&"x".repeat(256)).unwrap().len(), 256);
        assert_eq!(
            parse_auth_token("a-long-private-token").unwrap(),
            "a-long-private-token"
        );
        assert_eq!(
            listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), Some("abc"))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn authentication_is_required_before_any_event_and_failure_closes() {
        let remote = listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), None).unwrap();
        for first_line in ["screen\n", "auth invalid\n", "\n"] {
            let (mut stream, mut reader) = connect(&remote, false);
            stream.write_all(first_line.as_bytes()).unwrap();
            let mut reply = String::new();
            reader.read_line(&mut reply).unwrap();
            assert_eq!(reply, "err authentication required\n");
            reply.clear();
            assert_eq!(reader.read_line(&mut reply).unwrap(), 0);
            assert!(remote.poll().is_empty());
        }
    }

    #[test]
    fn failed_authentication_pauses_before_accepting_another_client() {
        let token = "a-long-private-remote-control-token";
        let remote = listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), Some(token)).unwrap();
        let mut bad = TcpStream::connect(remote.address()).unwrap();
        bad.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        bad.write_all(b"auth wrong\n").unwrap();
        let mut reply = String::new();
        BufReader::new(bad).read_line(&mut reply).unwrap();
        assert_eq!(reply, "err authentication required\n");

        let mut good = TcpStream::connect(remote.address()).unwrap();
        good.set_read_timeout(Some(Duration::from_millis(75)))
            .unwrap();
        writeln!(good, "auth {token}").unwrap();
        let mut reader = BufReader::new(good);
        assert!(
            reader
                .read_line(&mut String::new())
                .is_err_and(|error| paused(&error)),
            "the next client was accepted without a pause"
        );
        reader
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        reply.clear();
        reader.read_line(&mut reply).unwrap();
        assert_eq!(reply, "ok\n");
    }

    #[test]
    fn queue_and_poll_batch_are_bounded() {
        let remote = listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), None).unwrap();
        let (mut stream, mut reader) = connect(&remote, true);
        for index in 0..=QUEUE_LIMIT {
            stream.write_all(b"key a\n").unwrap();
            let mut reply = String::new();
            reader.read_line(&mut reply).unwrap();
            assert_eq!(
                reply,
                if index == QUEUE_LIMIT {
                    "err busy\n"
                } else {
                    "ok\n"
                }
            );
        }
        let batch = remote.poll();
        assert_eq!(batch.len(), BATCH);
        let remaining = remote.rx.as_ref().unwrap().try_iter().count();
        assert_eq!(remaining + batch.len(), QUEUE_LIMIT);
    }

    #[test]
    fn oversized_lines_are_rejected_before_unbounded_allocation() {
        let input = vec![b'a'; LINE_LIMIT * 2];
        let mut reader = io::Cursor::new(input);
        let mut line = Vec::with_capacity(LINE_LIMIT);
        assert!(matches!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineRead::TooLong
        ));
        assert!(line.len() <= LINE_LIMIT);
        assert_eq!(line.capacity(), LINE_LIMIT);
    }

    #[test]
    fn partial_lines_survive_read_timeouts() {
        struct FragmentReader(usize);
        impl io::Read for FragmentReader {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let part: &[u8] = match self.0 {
                    0 => b"key ctrl-",
                    1 => {
                        self.0 += 1;
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    2 => b"s\n",
                    _ => b"",
                };
                self.0 += 1;
                output[..part.len()].copy_from_slice(part);
                Ok(part.len())
            }
        }
        let mut reader = BufReader::new(FragmentReader(0));
        let mut line = Vec::with_capacity(LINE_LIMIT);
        assert!(matches!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineRead::Partial
        ));
        assert!(paused(
            &read_bounded_line(&mut reader, &mut line).err().unwrap()
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineRead::Complete
        ));
        assert_eq!(line, b"key ctrl-s\n");
    }

    #[test]
    fn bounded_reader_yields_after_each_fragment_for_deadline_checks() {
        let mut reader = BufReader::with_capacity(1, io::Cursor::new(b"incomplete"));
        let mut line = Vec::with_capacity(LINE_LIMIT);
        for expected in 1..=10 {
            assert!(matches!(
                read_bounded_line(&mut reader, &mut line).unwrap(),
                LineRead::Partial
            ));
            assert_eq!(line.len(), expected);
        }
        assert!(matches!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineRead::Closed
        ));
    }

    #[test]
    fn authentication_has_a_deadline() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::sync_channel(QUEUE_LIMIT);
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_client(
                stream,
                &tx,
                &AtomicBool::new(false),
                "token",
                false,
                Duration::from_millis(50),
                IDLE_WAIT,
            )
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(b"auth ").unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert_eq!(reply, "err authentication required\n");
        assert!(matches!(worker.join().unwrap(), ClientEnd::Unauthenticated));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn authenticated_idle_client_releases_the_worker() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::sync_channel(QUEUE_LIMIT);
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_client(
                stream,
                &tx,
                &AtomicBool::new(false),
                "a-long-private-token",
                false,
                Duration::from_secs(1),
                Duration::from_millis(50),
            )
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(b"auth a-long-private-token\n").unwrap();
        let mut reader = BufReader::new(stream);
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        assert_eq!(reply, "ok\n");
        reply.clear();
        reader.read_line(&mut reply).unwrap();
        assert_eq!(reply, "err idle timeout\n");
        reply.clear();
        assert_eq!(reader.read_line(&mut reply).unwrap(), 0);
        assert!(matches!(worker.join().unwrap(), ClientEnd::Done));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn drop_interrupts_an_authenticated_idle_client() {
        let remote = listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), None).unwrap();
        let (_stream, mut reader) = connect(&remote, true);
        let start = Instant::now();
        drop(remote);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(reader.read_line(&mut String::new()).unwrap(), 0);
    }

    #[test]
    fn drop_interrupts_a_screen_wait_even_when_the_reply_sender_is_alive() {
        let remote = listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), None).unwrap();
        let (mut stream, _reader) = connect(&remote, true);
        stream.write_all(b"screen\n").unwrap();
        let event = remote
            .rx
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(matches!(event, RemoteEvent::Screen(_)));
        let start = Instant::now();
        drop(remote);
        assert!(start.elapsed() < Duration::from_secs(1));
        drop(event);
    }
}
