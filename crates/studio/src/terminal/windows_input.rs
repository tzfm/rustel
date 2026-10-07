//! One owner for native console records and the VT packets carried by zero-key records.
//!
//! Console keys keep their native modifiers and key-up records. Only synthetic VT
//! text is passed through Termina's byte parser; Crossterm still owns rendering.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode,
    ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};
use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Console as console;
use windows_sys::Win32::System::Threading::WaitForSingleObject;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_PACKET;

#[path = "windows_keys.rs"]
mod windows_keys;
use windows_keys::NativeKeys;

const RECORDS_PER_READ: usize = 128;
const MAX_READS_PER_POLL: usize = 8;
const MAX_VT_BYTES: usize = 16 * 1024 * 1024;
const MAX_REPLY_BYTES: usize = 64 * 1024;
const ESCAPE_SETTLE: Duration = Duration::from_millis(20);

enum Queued {
    Event(Event),
    Key(KeyEvent, u16),
    // Keep the origin until read(): startup events can be queued before
    // pixel mode is negotiated, and their units must follow that decision.
    NativeMouse(MouseEvent),
}

struct VtInputMode {
    input: u32,
    win32_input: bool,
}

pub(super) struct WindowsInput {
    input: File,
    output: File,
    saved_mode: Option<u32>,
    saved_output_mode: Option<u32>,
    vt_input_mode: Option<VtInputMode>,
    console_packet: Vec<u8>,
    console_escape_since: Option<Instant>,
    keys: NativeKeys,
    held: [bool; 256],
    escaped_alt_key: Option<u16>,
    mouse_buttons: u32,
    pixel_mouse: Option<(u16, u16)>,
    queue: VecDeque<Queued>,
    parser: termina::Parser,
    vt_bytes: Vec<u8>,
    vt_surrogate: Option<u16>,
    escape_since: Option<Instant>,
    paste_search_from: usize,
    replies: Vec<u8>,
}

impl WindowsInput {
    /// Open this process's attached console without changing its modes.
    pub(super) fn new() -> io::Result<Self> {
        let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
        let output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
        Ok(Self::with_handles(input, output))
    }

    fn with_handles(input: File, output: File) -> Self {
        Self {
            input,
            output,
            saved_mode: None,
            saved_output_mode: None,
            vt_input_mode: None,
            console_packet: Vec::new(),
            console_escape_since: None,
            keys: NativeKeys::default(),
            held: [false; 256],
            escaped_alt_key: None,
            mouse_buttons: 0,
            pixel_mouse: None,
            queue: VecDeque::new(),
            parser: termina::Parser::default(),
            vt_bytes: Vec::new(),
            vt_surrogate: None,
            escape_since: None,
            paste_search_from: 0,
            replies: Vec::new(),
        }
    }

    /// Start with native records while configuring the lossless key encoding.
    /// VT_INPUT alone would discard Ctrl/Shift+Enter and repeat counts.
    /// May be called after mouse capture; the first saved modes stay the baseline.
    pub(super) fn prepare_console(&mut self) -> io::Result<()> {
        let mut mode = 0;
        if unsafe { console::GetConsoleMode(self.input.as_raw_handle(), &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            console::SetConsoleMode(
                self.input.as_raw_handle(),
                mode & !console::ENABLE_VIRTUAL_TERMINAL_INPUT,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        self.saved_mode.get_or_insert(mode);
        let mut output_mode = 0;
        if unsafe { console::GetConsoleMode(self.output.as_raw_handle(), &mut output_mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            console::SetConsoleMode(
                self.output.as_raw_handle(),
                output_mode | console::ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        self.saved_output_mode.get_or_insert(output_mode);
        Ok(())
    }

    /// Restore before the enclosing session restores Crossterm's shell modes.
    pub(super) fn restore_input_mode(&mut self) -> io::Result<()> {
        let mut error = self.restore_native_input().err();
        if let Some(mode) = self.saved_mode.take()
            && unsafe { console::SetConsoleMode(self.input.as_raw_handle(), mode) } == 0
        {
            error.get_or_insert_with(io::Error::last_os_error);
        }
        error.map_or(Ok(()), Err)
    }

    /// Restore output after the enclosing session emits its final VT cleanup.
    /// Drop also calls this for startup failures before a session is installed.
    pub(super) fn restore_mode(&mut self) -> io::Result<()> {
        let mut error = self.restore_input_mode().err();
        if let Some(mode) = self.saved_output_mode {
            if unsafe { console::SetConsoleMode(self.output.as_raw_handle(), mode) } == 0 {
                error.get_or_insert_with(io::Error::last_os_error);
            } else {
                self.saved_output_mode = None;
            }
        }
        error.map_or(Ok(()), Err)
    }

    pub(super) fn set_pixel_mouse(&mut self, cell: Option<(u16, u16)>) {
        self.pixel_mouse = cell.filter(|(width, height)| *width > 0 && *height > 0);
    }

    pub(super) fn vt_input_active(&self) -> bool {
        self.vt_input_mode.is_some()
    }

    /// Carry full Win32 keys and original mouse reports through VT input.
    /// Pixel sessions keep this mode throughout their lifetime: switching
    /// mid-gesture leaves ConPTY's own native mouse-button state stale.
    pub(super) fn enable_vt_input(&mut self) -> io::Result<()> {
        if self.vt_input_mode.is_some() {
            return Err(io::Error::other("console VT input already active"));
        }
        let mut mode = 0;
        if unsafe { console::GetConsoleMode(self.input.as_raw_handle(), &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if mode & console::ENABLE_VIRTUAL_TERMINAL_INPUT != 0 {
            return Err(io::Error::other(
                "VT input setup requires native input mode",
            ));
        }
        // DECRQM for this mode is answered by ConPTY itself even in native
        // input mode. Refuse a transition if its original state is unknown.
        self.output.write_all(b"\x1b[?9001$p")?;
        self.output.flush()?;
        let replies = self.collect_query(Duration::from_millis(80))?;
        let win32_input = if [b"\x1b[?9001;1$y".as_slice(), b"\x1b[?9001;3$y"]
            .iter()
            .any(|reply| replies.windows(reply.len()).any(|part| part == *reply))
        {
            true
        } else if replies
            .windows(b"\x1b[?9001;2$y".len())
            .any(|part| part == b"\x1b[?9001;2$y")
        {
            false
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "console Win32 input mode was not reported",
            ));
        };
        self.vt_input_mode = Some(VtInputMode {
            input: mode,
            win32_input,
        });
        let result = (|| {
            // Change local mode while VT_INPUT is off: otherwise bundled
            // ConPTY forwards this command and changes its parent terminal.
            self.output.write_all(b"\x1b[?9001h")?;
            self.output.flush()?;
            if unsafe {
                console::SetConsoleMode(
                    self.input.as_raw_handle(),
                    mode | console::ENABLE_VIRTUAL_TERMINAL_INPUT,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.restore_native_input();
        }
        result
    }

    /// Restore native input before restoring local mode 9001. Take ownership
    /// even on failure so a later Drop cannot reapply stale modes after the
    /// enclosing session hands the console back to Crossterm's raw-mode guard.
    pub(super) fn restore_native_input(&mut self) -> io::Result<()> {
        let Some(mode) = self.vt_input_mode.take() else {
            return Ok(());
        };
        if unsafe { console::SetConsoleMode(self.input.as_raw_handle(), mode.input) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.output.write_all(if mode.win32_input {
            b"\x1b[?9001h"
        } else {
            b"\x1b[?9001l"
        })?;
        self.output.flush()
    }

    /// Poll one bounded group of records, respecting the caller's deadline.
    pub(super) fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        let started = Instant::now();
        for _ in 0..MAX_READS_PER_POLL {
            self.settle_escape();
            if !self.queue.is_empty() {
                return Ok(true);
            }
            if self.pending()? {
                self.read_records()?;
                if !self.queue.is_empty() {
                    return Ok(true);
                }
            } else {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Ok(false);
                }
                // A partial outer wrapper may contain the next byte of the
                // inner escape sequence; its arrival must finish framing
                // before the inner Escape timeout can be considered.
                let since = if self.console_packet.is_empty() {
                    self.escape_since
                } else {
                    self.console_escape_since
                };
                let wait = since.map_or(remaining, |since| {
                    remaining.min(ESCAPE_SETTLE.saturating_sub(since.elapsed()))
                });
                self.wait(wait)?;
            }
            if started.elapsed() >= timeout {
                self.settle_escape();
                return Ok(!self.queue.is_empty());
            }
        }
        Ok(!self.queue.is_empty())
    }

    pub(super) fn read(&mut self) -> io::Result<Event> {
        loop {
            if let Some(event) = self.pop_event() {
                return Ok(event);
            }
            self.poll(Duration::from_millis(50))?;
        }
    }

    /// Collect terminal replies after the caller writes its queries. Ordinary
    /// keys, mouse reports, focus, resize and paste stay in the normal queue.
    pub(super) fn collect_query(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        self.replies.clear();
        let started = Instant::now();
        while started.elapsed() < timeout {
            if self.pending()? {
                self.read_records()?;
            } else {
                self.wait(timeout.saturating_sub(started.elapsed()))?;
            }
        }
        self.settle_escape();
        Ok(std::mem::take(&mut self.replies))
    }

    fn pending(&self) -> io::Result<bool> {
        let mut count = 0;
        if unsafe { console::GetNumberOfConsoleInputEvents(self.input.as_raw_handle(), &mut count) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(count != 0)
    }

    fn wait(&self, timeout: Duration) -> io::Result<()> {
        let millis = timeout.as_nanos().div_ceil(1_000_000);
        match unsafe {
            WaitForSingleObject(
                self.input.as_raw_handle(),
                millis.min(u128::from(u32::MAX - 1)) as u32,
            )
        } {
            WAIT_OBJECT_0 | WAIT_TIMEOUT => Ok(()),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("unexpected console input wait result")),
        }
    }

    fn read_records(&mut self) -> io::Result<()> {
        let mut records: [console::INPUT_RECORD; RECORDS_PER_READ] = unsafe { std::mem::zeroed() };
        let mut count = 0;
        if unsafe {
            console::ReadConsoleInputW(
                self.input.as_raw_handle(),
                records.as_mut_ptr(),
                records.len() as u32,
                &mut count,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        for record in &records[..count as usize] {
            self.route_record(record)?;
        }
        self.process_vt()
    }

    fn route_record(&mut self, record: &console::INPUT_RECORD) -> io::Result<()> {
        if u32::from(record.EventType) == console::KEY_EVENT {
            let key = unsafe { record.Event.KeyEvent };
            if key.wVirtualKeyCode == 0 {
                if key.bKeyDown != 0 {
                    for _ in 0..key.wRepeatCount.max(1) {
                        self.push_console_unit(unsafe { key.uChar.UnicodeChar })?;
                    }
                }
                return Ok(());
            }
            self.flush_console_packet();
            return self.route_key(key);
        }
        self.process_vt()?;
        self.decode_native(record)
    }

    /// VT input wraps KEY_EVENT_RECORDs in CSI Vk;Sc;Uc;Kd;Cs;Rc_. Keep
    /// this outer framing separate from the reconstructed VK0 VT stream,
    /// and keep decoding it after restore_native_input for packets still in the queue.
    fn push_console_unit(&mut self, unit: u16) -> io::Result<()> {
        if self.console_packet.is_empty() {
            if unit == 0x1b {
                self.console_packet.push(0x1b);
                self.console_escape_since = Some(Instant::now());
            } else {
                self.push_vt_unit(unit);
                self.process_vt()?;
            }
            return Ok(());
        }
        let expected = if self.console_packet.len() == 1 {
            unit == u16::from(b'[')
        } else {
            unit <= 0x7f && ((unit as u8).is_ascii_digit() || unit == u16::from(b';'))
        };
        if expected && self.console_packet.len() < 80 {
            self.console_packet.push(unit as u8);
            self.console_escape_since = None;
            return Ok(());
        }
        if unit == u16::from(b'_') && self.console_packet.starts_with(b"\x1b[") {
            let key = parse_win32_key(&self.console_packet[2..]);
            self.console_packet.clear();
            self.console_escape_since = None;
            if let Some(key) = key {
                self.route_key(key)?;
            }
        } else {
            self.flush_console_packet();
            self.push_console_unit(unit)?;
        }
        self.process_vt()
    }

    fn flush_console_packet(&mut self) {
        self.console_escape_since = None;
        for index in 0..self.console_packet.len() {
            self.push_vt_unit(u16::from(self.console_packet[index]));
        }
        self.console_packet.clear();
    }

    fn route_key(&mut self, record: console::KEY_EVENT_RECORD) -> io::Result<()> {
        if record.wVirtualKeyCode == 0 {
            // Raw terminal replies have synthetic release duplicates on
            // some ConPTY versions. Only their down records carry VT text.
            if record.bKeyDown != 0 {
                for _ in 0..record.wRepeatCount.max(1) {
                    self.push_vt_unit(unsafe { record.uChar.UnicodeChar });
                }
            }
            return self.process_vt();
        }
        self.process_vt()?;
        if self.vt_bytes.starts_with(b"\x1b[200~") {
            // ConPTY prints paste payload as native keys, including Ctrl+Enter
            // for LF and Alt releases for composed text. Keep literal Unicode
            // inside the paste; none of its synthetic key-up records escape.
            if record.bKeyDown == 0 {
                if let Some(held) = self.held.get_mut(usize::from(record.wVirtualKeyCode)) {
                    *held = false;
                }
                // Clear the saved Alt chord when its release occurs during paste.
                self.escaped_alt_key = None;
            }
            if let Some((ch, repeats)) = self.keys.decode_text(record) {
                for _ in 0..repeats {
                    let mut bytes = [0; 4];
                    self.vt_bytes
                        .extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
                }
            }
            return self.process_vt();
        }
        if self.vt_bytes == b"\x1b" {
            let unit = unsafe { record.uChar.UnicodeChar };
            let printable = char::from_u32(u32::from(unit)).is_some_and(|ch| !ch.is_control());
            if record.bKeyDown != 0
                && printable
                && self
                    .escape_since
                    .is_some_and(|since| since.elapsed() < ESCAPE_SETTLE)
            {
                // ConPTY can send Alt+letter as a VT Escape and a native letter.
                // Combine them before Escape can close the active panel.
                self.vt_bytes.clear();
                self.escape_since = None;
                self.escaped_alt_key = Some(record.wVirtualKeyCode);
            } else {
                self.flush_escape();
            }
        }
        let escaped_alt = self.escaped_alt_key == Some(record.wVirtualKeyCode);
        self.decode_native_key(record, escaped_alt);
        if record.bKeyDown == 0 && escaped_alt {
            self.escaped_alt_key = None;
        }
        Ok(())
    }

    fn decode_native(&mut self, record: &console::INPUT_RECORD) -> io::Result<()> {
        match u32::from(record.EventType) {
            console::KEY_EVENT => {
                let record = unsafe { record.Event.KeyEvent };
                self.decode_native_key(record, false);
            }
            console::FOCUS_EVENT => {
                let focused = unsafe { record.Event.FocusEvent.bSetFocus } != 0;
                self.enqueue(if focused {
                    Event::FocusGained
                } else {
                    Event::FocusLost
                });
            }
            console::WINDOW_BUFFER_SIZE_EVENT => {
                let size = unsafe { record.Event.WindowBufferSizeEvent.dwSize };
                if size.X > 0 && size.Y > 0 {
                    self.enqueue(Event::Resize(size.X as u16, size.Y as u16));
                }
            }
            console::MOUSE_EVENT => self.decode_mouse(unsafe { record.Event.MouseEvent })?,
            _ => {}
        }
        Ok(())
    }

    fn decode_native_key(&mut self, record: console::KEY_EVENT_RECORD, escaped_alt: bool) {
        if let Some((mut key, repeats)) = self.keys.decode(record) {
            if escaped_alt {
                key.modifiers.insert(KeyModifiers::ALT);
            }
            let vk = usize::from(record.wVirtualKeyCode);
            if record.wVirtualKeyCode != VK_PACKET && vk < self.held.len() {
                if record.bKeyDown != 0 {
                    if self.held[vk] {
                        key.kind = KeyEventKind::Repeat;
                    }
                    self.held[vk] = true;
                } else {
                    self.held[vk] = false;
                }
            }
            self.queue.push_back(Queued::Key(key, repeats));
        }
    }

    fn decode_mouse(&mut self, record: console::MOUSE_EVENT_RECORD) -> io::Result<()> {
        let buttons = record.dwButtonState & 0xffff;
        let previous = std::mem::replace(&mut self.mouse_buttons, buttons);
        let mut kinds = Vec::new();
        if record.dwEventFlags & console::MOUSE_WHEELED != 0 {
            let delta = (record.dwButtonState >> 16) as i16;
            if delta != 0 {
                kinds.push(if delta > 0 {
                    MouseEventKind::ScrollUp
                } else {
                    MouseEventKind::ScrollDown
                });
            }
        } else if record.dwEventFlags & console::MOUSE_HWHEELED != 0 {
            let delta = (record.dwButtonState >> 16) as i16;
            if delta != 0 {
                kinds.push(if delta > 0 {
                    MouseEventKind::ScrollRight
                } else {
                    MouseEventKind::ScrollLeft
                });
            }
        } else if record.dwEventFlags & console::MOUSE_MOVED != 0 {
            kinds.push(if buttons & console::RIGHTMOST_BUTTON_PRESSED != 0 {
                MouseEventKind::Drag(MouseButton::Right)
            } else if buttons & console::FROM_LEFT_2ND_BUTTON_PRESSED != 0 {
                MouseEventKind::Drag(MouseButton::Middle)
            } else if buttons & console::FROM_LEFT_1ST_BUTTON_PRESSED != 0 {
                MouseEventKind::Drag(MouseButton::Left)
            } else {
                MouseEventKind::Moved
            });
        } else {
            for (mask, button) in [
                (console::FROM_LEFT_1ST_BUTTON_PRESSED, MouseButton::Left),
                (console::RIGHTMOST_BUTTON_PRESSED, MouseButton::Right),
                (console::FROM_LEFT_2ND_BUTTON_PRESSED, MouseButton::Middle),
            ] {
                if (buttons ^ previous) & mask != 0 {
                    kinds.push(if buttons & mask != 0 {
                        MouseEventKind::Down(button)
                    } else {
                        MouseEventKind::Up(button)
                    });
                }
            }
        }
        if kinds.is_empty() {
            return Ok(());
        }
        let mut origin = (0, 0);
        if self.pixel_mouse.is_none() {
            let mut info: console::CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
            if unsafe {
                console::GetConsoleScreenBufferInfo(self.output.as_raw_handle(), &mut info)
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            origin = (info.srWindow.Left, info.srWindow.Top);
        }
        let column = (i32::from(record.dwMousePosition.X) - i32::from(origin.0))
            .clamp(0, i32::from(u16::MAX)) as u16;
        let row = (i32::from(record.dwMousePosition.Y) - i32::from(origin.1))
            .clamp(0, i32::from(u16::MAX)) as u16;
        for kind in kinds {
            let mouse = MouseEvent {
                kind,
                column,
                row,
                modifiers: native_modifiers(record.dwControlKeyState),
            };
            self.queue.push_back(if self.pixel_mouse.is_some() {
                // ConPTY already decoded these as zero-based pixel positions.
                Queued::Event(Event::Mouse(mouse))
            } else {
                Queued::NativeMouse(mouse)
            });
        }
        Ok(())
    }

    fn push_vt_unit(&mut self, unit: u16) {
        let ch = match unit {
            0xd800..=0xdbff => {
                self.vt_surrogate = Some(unit);
                None
            }
            0xdc00..=0xdfff => self
                .vt_surrogate
                .take()
                .and_then(|high| char::decode_utf16([high, unit]).next()?.ok()),
            _ => {
                self.vt_surrogate = None;
                char::from_u32(u32::from(unit))
            }
        };
        if let Some(ch) = ch {
            let mut bytes = [0; 4];
            self.vt_bytes
                .extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
        }
    }

    /// Frame protocol replies before Termina sees them: its public parser
    /// deliberately supports fewer query responses than the terminal probes.
    fn process_vt(&mut self) -> io::Result<()> {
        if self.vt_bytes.len() > MAX_VT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "terminal input packet exceeds 16 MiB",
            ));
        }
        let mut consumed = 0;
        while consumed < self.vt_bytes.len() {
            let bytes = &self.vt_bytes[consumed..];
            let length = if bytes[0] != 0x1b {
                bytes
                    .iter()
                    .position(|byte| *byte == 0x1b)
                    .unwrap_or(bytes.len())
            } else if bytes.len() < 2 {
                break;
            } else if bytes[1] == 0x1b {
                // Consecutive physical Escape presses are distinct events.
                1
            } else if bytes.starts_with(b"\x1b[200~") {
                if self.paste_search_from == 0 {
                    self.keys.reset_text();
                    self.vt_surrogate = None;
                }
                let end = b"\x1b[201~";
                let start = self.paste_search_from.max(6).min(bytes.len());
                let Some(offset) = bytes[start..]
                    .windows(end.len())
                    .position(|part| part == end)
                else {
                    self.paste_search_from = bytes.len().saturating_sub(end.len() - 1).max(6);
                    break;
                };
                self.paste_search_from = 0;
                self.keys.reset_text();
                self.vt_surrogate = None;
                start + offset + end.len()
            } else if bytes[1] == b'[' {
                let Some(end) = bytes[2..]
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte))
                else {
                    break;
                };
                end + 3
            } else if matches!(bytes[1], b']' | b'P' | b'_' | b'^') {
                let end = bytes
                    .windows(2)
                    .position(|part| part == b"\x1b\\")
                    .map(|at| at + 2);
                let bell = (bytes[1] == b']')
                    .then(|| bytes.iter().position(|byte| *byte == 7))
                    .flatten()
                    .map(|at| at + 1);
                let Some(end) = end.into_iter().chain(bell).min() else {
                    break;
                };
                end
            } else if bytes[1] == b'O' {
                if bytes.len() < 3 {
                    break;
                }
                3
            } else {
                // Alt plus a Unicode character, including a repeated ESC.
                let width = std::str::from_utf8(&bytes[1..])
                    .ok()
                    .and_then(|text| text.chars().next())
                    .map_or(1, char::len_utf8);
                1 + width
            };
            let packet = &bytes[..length];
            if is_query_reply(packet) {
                if self.replies.len() + packet.len() <= MAX_REPLY_BYTES {
                    self.replies.extend_from_slice(packet);
                }
            } else if !packet.starts_with(b"\x1b]")
                && !packet.starts_with(b"\x1bP")
                && !packet.starts_with(b"\x1b_")
                && !packet.starts_with(b"\x1b^")
            {
                // Termina's SGR parser subtracts one without checking zero.
                // Reject malformed coordinates before calling it.
                if !packet.starts_with(b"\x1b[<") || valid_sgr_coordinates(packet) {
                    self.parser.parse(packet, false);
                    self.drain_parser();
                    // Framing is complete here. Unknown terminal controls
                    // must not linger as a partial prefix before later keys.
                    self.parser = termina::Parser::default();
                }
            }
            consumed += length;
        }
        drop(self.vt_bytes.drain(..consumed));
        if self.vt_bytes == b"\x1b" {
            self.escape_since.get_or_insert_with(Instant::now);
        } else {
            self.escape_since = None;
        }
        Ok(())
    }

    fn settle_escape(&mut self) {
        if self
            .console_escape_since
            .is_some_and(|since| since.elapsed() >= ESCAPE_SETTLE)
        {
            self.flush_console_packet();
            let _ = self.process_vt();
            if self.vt_bytes == b"\x1b" {
                self.flush_escape();
            }
        }
        if self.console_packet.is_empty()
            && self
                .escape_since
                .is_some_and(|since| since.elapsed() >= ESCAPE_SETTLE)
        {
            self.flush_escape();
        }
    }

    fn flush_escape(&mut self) {
        self.vt_bytes.clear();
        self.escape_since = None;
        self.enqueue(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    }

    fn drain_parser(&mut self) {
        while let Some(event) = self.parser.pop() {
            if let Some(event) = convert_event(event) {
                self.enqueue(event);
            }
        }
    }

    fn enqueue(&mut self, event: Event) {
        if let Event::Mouse(mouse) = &event {
            // A query can split one gesture between SGR and native records.
            // Both decoders must share the button state used to detect edges.
            match mouse.kind {
                MouseEventKind::Down(button) | MouseEventKind::Drag(button) => {
                    self.mouse_buttons |= button_mask(button);
                }
                MouseEventKind::Up(button) => self.mouse_buttons &= !button_mask(button),
                MouseEventKind::Moved => self.mouse_buttons = 0,
                _ => {}
            }
        }
        if matches!(event, Event::FocusLost) {
            self.held.fill(false);
            self.escaped_alt_key = None;
            self.mouse_buttons = 0;
            self.keys = NativeKeys::default();
            self.vt_surrogate = None;
        }
        self.queue.push_back(Queued::Event(event));
    }

    fn pop_event(&mut self) -> Option<Event> {
        Some(match self.queue.pop_front()? {
            Queued::Event(event) => event,
            Queued::Key(key, repeats) => {
                if repeats > 1 {
                    let mut next = key;
                    if next.kind != KeyEventKind::Release {
                        next.kind = KeyEventKind::Repeat;
                    }
                    self.queue.push_front(Queued::Key(next, repeats - 1));
                }
                Event::Key(key)
            }
            Queued::NativeMouse(mut mouse) => {
                if let Some((width, height)) = self.pixel_mouse {
                    mouse.column = cell_center(mouse.column, width);
                    mouse.row = cell_center(mouse.row, height);
                }
                Event::Mouse(mouse)
            }
        })
    }
}

impl Drop for WindowsInput {
    fn drop(&mut self) {
        let _ = self.restore_mode();
    }
}

fn cell_center(cell: u16, size: u16) -> u16 {
    (u32::from(cell) * u32::from(size) + u32::from(size - 1) / 2).min(u32::from(u16::MAX)) as u16
}

fn button_mask(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => console::FROM_LEFT_1ST_BUTTON_PRESSED,
        MouseButton::Right => console::RIGHTMOST_BUTTON_PRESSED,
        MouseButton::Middle => console::FROM_LEFT_2ND_BUTTON_PRESSED,
    }
}

fn parse_win32_key(parameters: &[u8]) -> Option<console::KEY_EVENT_RECORD> {
    let text = std::str::from_utf8(parameters).ok()?;
    let mut fields = text.split(';');
    let mut next = || fields.next()?.parse::<u32>().ok();
    let virtual_key = u16::try_from(next()?).ok()?;
    let scan = u16::try_from(next()?).ok()?;
    let unicode = u16::try_from(next()?).ok()?;
    let down = next()?;
    let controls = next()?;
    let repeats = u16::try_from(next()?).ok()?;
    if down > 1 || fields.next().is_some() {
        return None;
    }
    Some(console::KEY_EVENT_RECORD {
        bKeyDown: down as i32,
        wRepeatCount: repeats.max(1),
        wVirtualKeyCode: virtual_key,
        wVirtualScanCode: scan,
        uChar: console::KEY_EVENT_RECORD_0 {
            UnicodeChar: unicode,
        },
        dwControlKeyState: controls,
    })
}

fn native_modifiers(state: u32) -> KeyModifiers {
    let mut modifiers = KeyModifiers::NONE;
    for (mask, modifier) in [
        (console::SHIFT_PRESSED, KeyModifiers::SHIFT),
        (
            console::LEFT_CTRL_PRESSED | console::RIGHT_CTRL_PRESSED,
            KeyModifiers::CONTROL,
        ),
        (
            console::LEFT_ALT_PRESSED | console::RIGHT_ALT_PRESSED,
            KeyModifiers::ALT,
        ),
    ] {
        if state & mask != 0 {
            modifiers |= modifier;
        }
    }
    modifiers
}

fn is_query_reply(packet: &[u8]) -> bool {
    packet.starts_with(b"\x1b_G")
        || (packet.starts_with(b"\x1b[?")
            && (packet.ends_with(b"$y") || packet.ends_with(b"c") || packet.ends_with(b"u")))
        || (packet.starts_with(b"\x1b[>") && packet.ends_with(b"c"))
        || (packet.ends_with(b"t")
            && [b"\x1b[4;".as_slice(), b"\x1b[6;", b"\x1b[8;"]
                .iter()
                .any(|prefix| packet.starts_with(prefix)))
}

fn valid_sgr_coordinates(packet: &[u8]) -> bool {
    if !matches!(packet.last(), Some(b'M' | b'm')) {
        return false;
    }
    std::str::from_utf8(&packet[3..packet.len() - 1])
        .ok()
        .is_some_and(|text| {
            let mut parts = text.split(';');
            parts.next().is_some_and(|part| part.parse::<u8>().is_ok())
                && (0..2).all(|_| {
                    parts
                        .next()
                        .and_then(|part| part.parse::<u16>().ok())
                        .is_some_and(|coordinate| coordinate > 0)
                })
        })
}

fn convert_modifiers(modifiers: termina::event::Modifiers) -> KeyModifiers {
    use termina::event::Modifiers as Source;
    let mut result = KeyModifiers::NONE;
    for (from, to) in [
        (Source::SHIFT, KeyModifiers::SHIFT),
        (Source::ALT, KeyModifiers::ALT),
        (Source::CONTROL, KeyModifiers::CONTROL),
        (Source::SUPER, KeyModifiers::SUPER),
        (Source::HYPER, KeyModifiers::HYPER),
        (Source::META, KeyModifiers::META),
    ] {
        if modifiers.contains(from) {
            result |= to;
        }
    }
    result
}

fn convert_event(event: termina::Event) -> Option<Event> {
    use termina::event as source;
    Some(match event {
        source::Event::Key(key) => {
            let mut state = KeyEventState::NONE;
            for (from, to) in [
                (source::KeyEventState::KEYPAD, KeyEventState::KEYPAD),
                (source::KeyEventState::CAPS_LOCK, KeyEventState::CAPS_LOCK),
                (source::KeyEventState::NUM_LOCK, KeyEventState::NUM_LOCK),
            ] {
                if key.state.contains(from) {
                    state |= to;
                }
            }
            Event::Key(KeyEvent {
                code: convert_key(key.code),
                modifiers: convert_modifiers(key.modifiers),
                kind: match key.kind {
                    source::KeyEventKind::Press => KeyEventKind::Press,
                    source::KeyEventKind::Release => KeyEventKind::Release,
                    source::KeyEventKind::Repeat => KeyEventKind::Repeat,
                },
                state,
            })
        }
        source::Event::Mouse(mouse) => {
            let button = |button| match button {
                source::MouseButton::Left => MouseButton::Left,
                source::MouseButton::Right => MouseButton::Right,
                source::MouseButton::Middle => MouseButton::Middle,
            };
            Event::Mouse(MouseEvent {
                column: mouse.column,
                row: mouse.row,
                modifiers: convert_modifiers(mouse.modifiers),
                kind: match mouse.kind {
                    source::MouseEventKind::Down(b) => MouseEventKind::Down(button(b)),
                    source::MouseEventKind::Up(b) => MouseEventKind::Up(button(b)),
                    source::MouseEventKind::Drag(b) => MouseEventKind::Drag(button(b)),
                    source::MouseEventKind::Moved => MouseEventKind::Moved,
                    source::MouseEventKind::ScrollUp => MouseEventKind::ScrollUp,
                    source::MouseEventKind::ScrollDown => MouseEventKind::ScrollDown,
                    source::MouseEventKind::ScrollLeft => MouseEventKind::ScrollLeft,
                    source::MouseEventKind::ScrollRight => MouseEventKind::ScrollRight,
                },
            })
        }
        source::Event::WindowResized(size) => Event::Resize(size.cols, size.rows),
        source::Event::FocusIn => Event::FocusGained,
        source::Event::FocusOut => Event::FocusLost,
        source::Event::Paste(text) => Event::Paste(text),
        source::Event::Csi(_) | source::Event::Osc(_) | source::Event::Dcs(_) => return None,
    })
}

fn convert_key(key: termina::event::KeyCode) -> KeyCode {
    use termina::event as source;
    macro_rules! same_variants {
        ($value:expr, $from:path, $to:path, $($variant:ident),+ $(,)?) => {{
            use $from as From;
            use $to as To;
            match $value { $(From::$variant => To::$variant,)+ }
        }};
    }
    match key {
        source::KeyCode::Char(ch) => KeyCode::Char(ch),
        source::KeyCode::Function(n) => KeyCode::F(n),
        source::KeyCode::Escape => KeyCode::Esc,
        source::KeyCode::Modifier(key) => KeyCode::Modifier(same_variants!(
            key,
            source::ModifierKeyCode,
            ModifierKeyCode,
            LeftShift,
            LeftControl,
            LeftAlt,
            LeftSuper,
            LeftHyper,
            LeftMeta,
            RightShift,
            RightControl,
            RightAlt,
            RightSuper,
            RightHyper,
            RightMeta,
            IsoLevel3Shift,
            IsoLevel5Shift
        )),
        source::KeyCode::Media(key) => KeyCode::Media(same_variants!(
            key,
            source::MediaKeyCode,
            MediaKeyCode,
            Play,
            Pause,
            PlayPause,
            Reverse,
            Stop,
            FastForward,
            Rewind,
            TrackNext,
            TrackPrevious,
            Record,
            LowerVolume,
            RaiseVolume,
            MuteVolume
        )),
        source::KeyCode::Enter => KeyCode::Enter,
        source::KeyCode::Backspace => KeyCode::Backspace,
        source::KeyCode::Tab => KeyCode::Tab,
        source::KeyCode::Left => KeyCode::Left,
        source::KeyCode::Right => KeyCode::Right,
        source::KeyCode::Up => KeyCode::Up,
        source::KeyCode::Down => KeyCode::Down,
        source::KeyCode::Home => KeyCode::Home,
        source::KeyCode::End => KeyCode::End,
        source::KeyCode::BackTab => KeyCode::BackTab,
        source::KeyCode::PageUp => KeyCode::PageUp,
        source::KeyCode::PageDown => KeyCode::PageDown,
        source::KeyCode::Insert => KeyCode::Insert,
        source::KeyCode::Delete => KeyCode::Delete,
        source::KeyCode::KeypadBegin => KeyCode::KeypadBegin,
        source::KeyCode::CapsLock => KeyCode::CapsLock,
        source::KeyCode::ScrollLock => KeyCode::ScrollLock,
        source::KeyCode::NumLock => KeyCode::NumLock,
        source::KeyCode::PrintScreen => KeyCode::PrintScreen,
        source::KeyCode::Pause => KeyCode::Pause,
        source::KeyCode::Menu => KeyCode::Menu,
        source::KeyCode::Null => KeyCode::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_RETURN;

    fn decoder() -> WindowsInput {
        // Decoder tests never query or change console state; NUL handles let
        // them run in a normal cargo test process without an attached console.
        WindowsInput::with_handles(File::open("NUL").unwrap(), File::open("NUL").unwrap())
    }

    fn feed(input: &mut WindowsInput, text: &str) {
        for unit in text.encode_utf16() {
            input.push_vt_unit(unit);
        }
        input.process_vt().unwrap();
    }

    fn native(
        input: &mut WindowsInput,
        vk: u16,
        unicode: u16,
        down: bool,
        controls: u32,
        repeats: u16,
    ) {
        input
            .route_record(&console::INPUT_RECORD {
                EventType: console::KEY_EVENT as u16,
                Event: console::INPUT_RECORD_0 {
                    KeyEvent: console::KEY_EVENT_RECORD {
                        bKeyDown: i32::from(down),
                        wRepeatCount: repeats,
                        wVirtualKeyCode: vk,
                        wVirtualScanCode: 0,
                        uChar: console::KEY_EVENT_RECORD_0 {
                            UnicodeChar: unicode,
                        },
                        dwControlKeyState: controls,
                    },
                },
            })
            .unwrap();
    }

    fn wrapped(input: &mut WindowsInput, text: &str) {
        for unit in text.encode_utf16() {
            for outer in format!("\x1b[0;0;{unit};1;0;1_").encode_utf16() {
                native(input, 0, outer, true, 0, 1);
            }
        }
    }

    #[test]
    fn queued_query_wrappers_keep_native_keys_and_nested_pixel_reports() {
        let mut input = decoder();
        // No active query state: these can still be waiting after restore_native_input.
        for unit in "\x1b[13;28;13;1;8;1_\x1b[13;28;13;0;8;1_".encode_utf16() {
            native(&mut input, 0, unit, true, 0, 1);
        }
        wrapped(&mut input, "\x1b[6;23;11t\x1b[<0;950;41M\x1b[<32;951;41M");
        for kind in [KeyEventKind::Press, KeyEventKind::Release] {
            assert_eq!(
                input.pop_event(),
                Some(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Enter,
                    KeyModifiers::CONTROL,
                    kind
                )))
            );
        }
        assert_eq!(input.replies, b"\x1b[6;23;11t");
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Down(MouseButton::Left), 949, 40))
        );
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Drag(MouseButton::Left), 950, 40))
        );
        wrapped(&mut input, "\x1b[200~é🙂\n\x1b[6;20;10t\x1b[201~");
        assert_eq!(
            input.pop_event(),
            Some(Event::Paste("é🙂\n\x1b[6;20;10t".into()))
        );
        for unit in "\x1b[65536;28;13;1;8;1_\x1b[13;28;13;2;8;1_\x1b[13;28;13;1;4294967296;1_"
            .encode_utf16()
        {
            native(&mut input, 0, unit, true, 0, 1);
        }
        assert!(input.pop_event().is_none());
    }

    #[test]
    fn partial_outer_wrapper_prevents_inner_escape_timeout_splitting_a_report() {
        let mut input = decoder();
        wrapped(&mut input, "\x1b");
        for unit in "\x1b[0;0;91;1;".encode_utf16() {
            native(&mut input, 0, unit, true, 0, 1);
        }
        input.escape_since = Some(Instant::now() - ESCAPE_SETTLE);
        input.settle_escape();
        assert!(input.pop_event().is_none());
        for unit in "0;1_".encode_utf16() {
            native(&mut input, 0, unit, true, 0, 1);
        }
        wrapped(&mut input, "<0;111;41M");
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Down(MouseButton::Left), 110, 40))
        );
        assert!(input.pop_event().is_none());
    }

    #[test]
    fn startup_capabilities_collect_split_apc_without_consuming_keys_mouse_or_paste() {
        let mut input = decoder();
        wrapped(&mut input, "\x1b[?62;4c\x1b_Gi=31;");
        assert_eq!(input.replies, b"\x1b[?62;4c");
        assert!(input.pop_event().is_none());
        wrapped(&mut input, "OK\x1b\\\x1b[?2026;2$y\x1b[6;23;11t");
        native(
            &mut input,
            VK_RETURN,
            13,
            true,
            console::LEFT_CTRL_PRESSED,
            1,
        );
        native(
            &mut input,
            VK_RETURN,
            13,
            false,
            console::LEFT_CTRL_PRESSED,
            1,
        );
        wrapped(
            &mut input,
            "\x1b[<0;111;41M\x1b[200~\x1b_Gi=31;OK\x1b\\\x1b[201~",
        );
        assert_eq!(
            input.replies,
            b"\x1b[?62;4c\x1b_Gi=31;OK\x1b\\\x1b[?2026;2$y\x1b[6;23;11t"
        );
        for kind in [KeyEventKind::Press, KeyEventKind::Release] {
            assert_eq!(
                input.pop_event(),
                Some(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Enter,
                    KeyModifiers::CONTROL,
                    kind
                )))
            );
        }
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Down(MouseButton::Left), 110, 40))
        );
        assert_eq!(
            input.pop_event(),
            Some(Event::Paste("\x1b_Gi=31;OK\x1b\\".into()))
        );
        assert!(input.pop_event().is_none());
    }

    #[test]
    fn native_keys_keep_modifiers_repeats_and_order_around_raw_reports() {
        let mut input = decoder();
        native(
            &mut input,
            VK_RETURN,
            13,
            true,
            console::LEFT_CTRL_PRESSED,
            1,
        );
        native(
            &mut input,
            VK_RETURN,
            13,
            false,
            console::LEFT_CTRL_PRESSED,
            1,
        );
        feed(&mut input, "\x1b[6;20;10t\x1b[<0;111;41M");
        native(&mut input, VK_RETURN, 13, true, console::SHIFT_PRESSED, 1);
        native(&mut input, 65, 97, true, 0, 3);
        feed(&mut input, "\x1b[<32;112;41M\x1b[<0;112;41m");
        assert_eq!(input.replies, b"\x1b[6;20;10t");
        for kind in [KeyEventKind::Press, KeyEventKind::Release] {
            assert_eq!(
                input.pop_event(),
                Some(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Enter,
                    KeyModifiers::CONTROL,
                    kind
                )))
            );
        }
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Down(MouseButton::Left), 110, 40))
        );
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::SHIFT
            )))
        );
        for kind in [
            KeyEventKind::Press,
            KeyEventKind::Repeat,
            KeyEventKind::Repeat,
        ] {
            assert_eq!(
                input.pop_event(),
                Some(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Char('a'),
                    KeyModifiers::NONE,
                    kind
                )))
            );
        }
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Drag(MouseButton::Left), 111, 40))
        );
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Up(MouseButton::Left), 111, 40))
        );
        assert!(input.pop_event().is_none());
    }

    #[test]
    fn native_paste_keeps_alt_unicode_and_lf_without_releases_or_stuck_keys() {
        let mut input = decoder();
        native(&mut input, 86, 118, true, console::LEFT_CTRL_PRESSED, 1);
        input.pop_event().unwrap();
        feed(&mut input, "\x1b[200~");
        native(&mut input, 86, 118, false, console::LEFT_CTRL_PRESSED, 1);
        for down in [true, false] {
            native(&mut input, 80, 112, down, 0, 1);
        }
        // System ConPTY commits supplementary Unicode on Alt key-up.
        for unit in [55357, 56898] {
            native(&mut input, 18, 0, true, console::LEFT_ALT_PRESSED, 1);
            for vk in [102, 99] {
                for down in [true, false] {
                    native(&mut input, vk, 0, down, console::LEFT_ALT_PRESSED, 1);
                }
            }
            native(&mut input, 18, unit, false, 0, 1);
        }
        native(&mut input, 17, 0, true, console::LEFT_CTRL_PRESSED, 1);
        for down in [true, false] {
            native(
                &mut input,
                VK_RETURN,
                10,
                down,
                console::LEFT_CTRL_PRESSED,
                1,
            );
        }
        native(&mut input, 17, 0, false, 0, 1);
        // Newer ConPTY carries emoji as raw UTF16 down/up pairs instead.
        for unit in [55357, 56898] {
            for down in [true, false] {
                native(&mut input, 0, unit, down, 0, 1);
            }
        }
        feed(&mut input, "\x1b[6;20;10t");
        assert!(input.pop_event().is_none());
        feed(&mut input, "\x1b[201~");
        native(&mut input, 86, 118, true, console::LEFT_CTRL_PRESSED, 1);
        assert_eq!(
            input.pop_event(),
            Some(Event::Paste("p🙂\n🙂\x1b[6;20;10t".into()))
        );
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL
            )))
        );
        assert!(input.pop_event().is_none());
        assert!(input.replies.is_empty());
    }

    #[test]
    fn pending_escape_precedes_native_key_and_focus_resets_partial_unicode() {
        let mut input = decoder();
        native(&mut input, 0, 27, true, 0, 1);
        native(&mut input, VK_RETURN, 13, true, console::SHIFT_PRESSED, 1);
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        );
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::SHIFT
            )))
        );
        native(&mut input, 0, 55357, true, 0, 1);
        input.enqueue(Event::FocusLost);
        native(&mut input, 0, 56898, true, 0, 1);
        assert_eq!(input.pop_event(), Some(Event::FocusLost));
        assert!(input.pop_event().is_none());
    }

    #[test]
    fn split_escape_and_native_letter_stay_one_alt_chord() {
        let mut input = decoder();
        native(&mut input, 0, 27, true, 0, 1);
        native(&mut input, 82, u16::from(b'r'), true, 0, 1);
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('r'),
                KeyModifiers::ALT
            )))
        );
        // The release keeps the Alt modifier without a second key press.
        native(&mut input, 82, u16::from(b'r'), false, 0, 1);
        let mut released = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT);
        released.kind = KeyEventKind::Release;
        assert_eq!(input.pop_event(), Some(Event::Key(released)));
        native(&mut input, 79, u16::from(b'o'), true, 0, 1);
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::NONE
            )))
        );
        assert!(input.pop_event().is_none(), "Esc must not close the panel");
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn split_sgr_records_keep_each_pixel_and_release() {
        let mut input = decoder();
        for part in ["\x1b", "[<0;", "111;", "221"] {
            feed(&mut input, part);
            assert!(input.pop_event().is_none());
        }
        feed(&mut input, "M\x1b[<32;112;221M\x1b[<0;113;221m");
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Down(MouseButton::Left), 110, 220))
        );
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Drag(MouseButton::Left), 111, 220))
        );
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Up(MouseButton::Left), 112, 220))
        );
        assert!(input.pop_event().is_none());
    }

    #[test]
    fn probe_replies_leave_mouse_native_keys_and_focus_in_order() {
        let mut input = decoder();
        feed(&mut input, "\x1b[<0;111;221M");
        input
            .decode_native(&console::INPUT_RECORD {
                EventType: console::KEY_EVENT as u16,
                Event: console::INPUT_RECORD_0 {
                    KeyEvent: console::KEY_EVENT_RECORD {
                        bKeyDown: 1,
                        wRepeatCount: 1,
                        wVirtualKeyCode: VK_RETURN,
                        wVirtualScanCode: 28,
                        uChar: console::KEY_EVENT_RECORD_0 { UnicodeChar: 13 },
                        dwControlKeyState: console::LEFT_CTRL_PRESSED,
                    },
                },
            })
            .unwrap();
        feed(&mut input, "\x1b[6;20;");
        feed(&mut input, "10t\x1b[?1016;0$y\x1b[I");
        assert_eq!(input.replies, b"\x1b[6;20;10t\x1b[?1016;0$y");
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Down(MouseButton::Left), 110, 220))
        );
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::CONTROL
            )))
        );
        assert_eq!(input.pop_event(), Some(Event::FocusGained));
        assert!(input.pop_event().is_none());
    }

    #[test]
    fn paste_preserves_unicode_and_query_like_text_across_chunks() {
        let mut input = decoder();
        feed(&mut input, "\x1b[200~paste 🚀\n\x1b[6;20;10t\x1b[?1016;2$y");
        feed(&mut input, "\x1b[20");
        assert!(input.pop_event().is_none());
        feed(&mut input, "1~\x1b[O");
        assert_eq!(
            input.pop_event(),
            Some(Event::Paste("paste 🚀\n\x1b[6;20;10t\x1b[?1016;2$y".into()))
        );
        assert_eq!(input.pop_event(), Some(Event::FocusLost));
        assert!(input.replies.is_empty());
    }

    #[test]
    fn raw_escape_settles_and_consecutive_escapes_are_distinct() {
        let mut input = decoder();
        feed(&mut input, "\x1b\x1b");
        let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(input.pop_event(), Some(escape.clone()));
        assert!(input.pop_event().is_none());
        input.escape_since = Some(Instant::now() - ESCAPE_SETTLE);
        input.settle_escape();
        assert_eq!(input.pop_event(), Some(escape));
        assert!(input.pop_event().is_none());
        assert!(input.vt_bytes.is_empty());
        for _ in 0..2 {
            native(&mut input, 0, 27, true, 0, 1);
        }
        input.console_escape_since = Some(Instant::now() - ESCAPE_SETTLE);
        input.settle_escape();
        for _ in 0..2 {
            assert_eq!(
                input.pop_event(),
                Some(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
            );
        }
        assert!(input.pop_event().is_none());
        assert!(input.console_packet.is_empty());
    }

    #[test]
    fn malformed_coordinates_do_not_overflow_or_swallow_later_input() {
        let mut input = decoder();
        feed(
            &mut input,
            "\x1b[<0;0;1M\x1b[<0;1;0m\x1b[<0;65536;1M\x1b[<0;1;1Xx",
        );
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE
            )))
        );
        assert!(input.pop_event().is_none());
    }

    #[test]
    fn vt_control_and_alt_modifiers_match_crossterm_flags() {
        let mut input = decoder();
        feed(&mut input, "\x1b[1;5C\x1b[1;3C\x1b[<20;2;3M");
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Right,
                KeyModifiers::CONTROL
            )))
        );
        assert_eq!(
            input.pop_event(),
            Some(Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT)))
        );
        assert!(
            matches!(input.pop_event(), Some(Event::Mouse(MouseEvent { modifiers, .. })) if modifiers == KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn queued_native_mouse_uses_the_scale_selected_after_probe() {
        let mut input = decoder();
        input.queue.push_back(Queued::NativeMouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }));
        feed(&mut input, "\x1b[<32;26;71M");
        input.set_pixel_mouse(Some((10, 20)));
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Down(MouseButton::Left), 24, 69))
        );
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Drag(MouseButton::Left), 25, 70))
        );
        input
            .decode_mouse(console::MOUSE_EVENT_RECORD {
                dwMousePosition: console::COORD { X: 949, Y: 40 },
                dwButtonState: console::FROM_LEFT_1ST_BUTTON_PRESSED,
                dwControlKeyState: 0,
                dwEventFlags: console::MOUSE_MOVED,
            })
            .unwrap();
        assert_eq!(
            input.pop_event(),
            Some(mouse(MouseEventKind::Drag(MouseButton::Left), 949, 40))
        );
    }

    #[test]
    fn gestures_cross_query_and_native_input_without_losing_button_edges() {
        for (button, sgr) in [
            (MouseButton::Left, 0),
            (MouseButton::Middle, 1),
            (MouseButton::Right, 2),
        ] {
            let mut input = decoder();
            input.set_pixel_mouse(Some((11, 23)));
            let native_mouse = |buttons| console::MOUSE_EVENT_RECORD {
                dwMousePosition: console::COORD { X: 111, Y: 40 },
                dwButtonState: buttons,
                dwControlKeyState: 0,
                dwEventFlags: 0,
            };
            // Press during a query, release after it without an intervening move.
            wrapped(&mut input, &format!("\x1b[<{sgr};111;41M"));
            input.decode_mouse(native_mouse(0)).unwrap();
            assert_eq!(
                input.pop_event(),
                Some(mouse(MouseEventKind::Down(button), 110, 40))
            );
            assert_eq!(
                input.pop_event(),
                Some(mouse(MouseEventKind::Up(button), 111, 40))
            );
            // Release during a query, then press again in native input mode.
            input
                .decode_mouse(native_mouse(button_mask(button)))
                .unwrap();
            wrapped(&mut input, &format!("\x1b[<{sgr};112;41m"));
            input
                .decode_mouse(native_mouse(button_mask(button)))
                .unwrap();
            for kind in [
                MouseEventKind::Down(button),
                MouseEventKind::Up(button),
                MouseEventKind::Down(button),
            ] {
                assert_eq!(input.pop_event(), Some(mouse(kind, 111, 40)));
            }
            assert!(input.pop_event().is_none());
        }
    }
}
