/*
lib.rs - Serial output for live patterns
Message formatting and framing adapted from Strudel packages/serial/serial.mjs.
Its CRC source credits https://gist.github.com/tijnkooijmans/10981093.
Copyright (C) 2022 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Serial output: a pattern driving a microcontroller.
//!
//! This is the physical-computing path - an Arduino, Teensy or Pico on the
//! other end firing solenoids at real drums, stepping motors, switching relays.
//! Upstream uses Web Serial, which can only run after a user gesture picks a
//! port; natively we open the port by name and there is no dialog.
//!
//! Like MIDI and unlike OSC, the wire has no notion of "later", so the sends
//! are scheduled here: a time-ordered queue drained by one thread. Sends go
//! through a [`SerialPort`] trait so the formatting and timing are testable
//! with no hardware attached, the same arrangement as the MIDI work.
//!
//! Nothing here may end a set: an unplugged adapter, a write error or a full
//! queue is counted and dropped.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Upstream's default in `Pattern.prototype.serial`.
pub const DEFAULT_BAUD: u32 = 115_200;

/// Upstream adds a fixed 100ms before writing (`const latency = 0.1`).
///
/// It exists to absorb the browser's timer jitter. Kept because a device
/// tuned against strudel.cc's timing would otherwise land 100ms early here.
pub const SERIAL_LATENCY: Duration = Duration::from_millis(100);

/// Queue ceiling; pushes past it are refused and counted, never blocked on.
pub const MAX_QUEUED_MESSAGES: usize = 4096;

/// A message this late is dropped rather than written.
pub const MAX_QUEUE_LATENESS: Duration = Duration::from_millis(500);

/// CRC16/CCITT-FALSE: init 0xFFFF, polynomial 0x1021, no reflection, no final
/// xor.
///
/// Upstream hashes `charCodeAt`, i.e. UTF-16 code units. This hashes bytes,
/// which agrees for the ASCII these messages are built from and is what a
/// microcontroller checking the frame will compute.
pub fn crc16(data: &[u8]) -> u16 {
    if data.is_empty() {
        return 0;
    }
    let mut crc: u16 = 0xffff;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// One key/value of a hap, already stringified.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub key: String,
    pub value: String,
}

/// Build the message body for one hap.
///
/// Two shapes, both strudel.cc's:
///
/// - with an `action` field: `action(key:value,key:value)`
/// - without: the pairs concatenated with no separator at all, `k:vk:v`
///
/// The second looks like a bug and is not one - it is what strudel.cc writes, and
/// a sketch parsing it would break if we inserted separators.
pub fn format_message(action: Option<&str>, fields: &[Field], single_char_ids: bool) -> String {
    let shorten = |text: &str| -> String {
        if single_char_ids {
            text.chars().next().map(String::from).unwrap_or_default()
        } else {
            text.to_string()
        }
    };

    match action {
        Some(action) => {
            let mut out = shorten(action);
            out.push('(');
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&shorten(&field.key));
                out.push(':');
                out.push_str(&field.value);
            }
            out.push(')');
            out
        }
        None => fields
            .iter()
            .map(|field| format!("{}:{}", field.key, field.value))
            .collect(),
    }
}

/// Append the checksum frame: `|`, the CRC big-endian, `;`.
pub fn with_crc(message: &str) -> Vec<u8> {
    let crc = crc16(message.as_bytes());
    let mut out = message.as_bytes().to_vec();
    out.push(b'|');
    out.push((crc >> 8) as u8);
    out.push((crc & 0xff) as u8);
    out.push(b';');
    out
}

/// Where bytes actually go.
pub trait SerialPort: Send {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String>;

    /// Write now, with the queue deadline available to capture ports.
    fn write_scheduled(&mut self, _due: Instant, bytes: &[u8]) -> Result<(), String> {
        self.write(bytes)
    }
}

impl SerialPort for Box<dyn serialport::SerialPort> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        use std::io::Write as _;
        std::io::Write::write_all(self, bytes).map_err(|error| error.to_string())?;
        self.flush().map_err(|error| error.to_string())
    }
}

/// One captured write.
pub type CapturedWrite = (Instant, Vec<u8>);

/// Records what was written and when, for tests.
#[derive(Clone, Default)]
pub struct CapturePort {
    pub log: Arc<Mutex<Vec<CapturedWrite>>>,
}

impl CapturePort {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn writes(&self) -> Vec<CapturedWrite> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl SerialPort for CapturePort {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((Instant::now(), bytes.to_vec()));
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SerialReport {
    pub written: u64,
    pub dropped_late: u64,
    pub refused_full: u64,
    pub write_errors: u64,
}

impl SerialReport {
    /// Both reports' counts together, as several senders make one total.
    /// Every field is named, with no `..`, so a counter added to the report
    /// does not compile until it is summed here too.
    pub fn plus(self, other: Self) -> Self {
        Self {
            written: self.written.saturating_add(other.written),
            dropped_late: self.dropped_late.saturating_add(other.dropped_late),
            refused_full: self.refused_full.saturating_add(other.refused_full),
            write_errors: self.write_errors.saturating_add(other.write_errors),
        }
    }

    /// What was counted after `base`, as a sender taken over mid-life counts
    /// from where it was taken. Named field by field for the same reason as
    /// [`Self::plus`].
    pub fn since(self, base: Self) -> Self {
        Self {
            written: self.written.saturating_sub(base.written),
            dropped_late: self.dropped_late.saturating_sub(base.dropped_late),
            refused_full: self.refused_full.saturating_sub(base.refused_full),
            write_errors: self.write_errors.saturating_sub(base.write_errors),
        }
    }
}

struct Queue {
    heap: BinaryHeap<Reverse<Queued>>,
    stopping: bool,
}

#[derive(PartialEq, Eq)]
struct Queued {
    due: Instant,
    seq: u64,
    bytes: Vec<u8>,
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.due
            .cmp(&other.due)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Starts a sender's writer thread from its builder and body.
type SpawnWriter = fn(
    std::thread::Builder,
    Box<dyn FnOnce() + Send>,
) -> std::io::Result<std::thread::JoinHandle<()>>;

/// Owns the port and the thread that drains the queue.
pub struct SerialSender {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    seq: AtomicU64,
    written: Arc<AtomicU64>,
    dropped_late: Arc<AtomicU64>,
    refused_full: Arc<AtomicU64>,
    write_errors: Arc<AtomicU64>,
    port_name: String,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SerialSender {
    /// Every serial port the system reports.
    pub fn ports() -> Result<Vec<String>, String> {
        Ok(serialport::available_ports()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|port| port.port_name)
            .collect())
    }

    /// Open `name` at `baud`.
    ///
    /// A name, not a dialog: the browser must call `navigator.serial.requestPort()`
    /// behind a user gesture, which a terminal has no equivalent for and does
    /// not need.
    ///
    /// The timeout below configures the port's read/write deadlines once it
    /// exists - on Windows, COMMTIMEOUTS - not how long the open call itself
    /// may take. An offline Bluetooth SPP port answers the platform open in
    /// seconds and a wedged USB-CDC driver may never answer, so this call
    /// belongs on a thread that can afford to be held (the runtime opens
    /// each serial port on a thread of its own), never on a thread with a
    /// schedule to keep.
    pub fn open(name: &str, baud: u32) -> Result<Self, String> {
        let port = serialport::new(name, baud)
            .timeout(Duration::from_millis(50))
            .open()
            .map_err(|error| format!("could not open serial port \"{name}\": {error}"))?;
        Self::try_with_port(Box::new(port), name.to_string())
    }

    /// Build a sender over any sink, so tests can drive the real timing path.
    ///
    /// A sender whose writer thread cannot start refuses every write;
    /// [`Self::try_with_port`] reports that failure instead. Mirrors
    /// `MidiSender::with_port`.
    pub fn with_port(port: Box<dyn SerialPort>, port_name: String) -> Self {
        let fallback_name = port_name.clone();
        Self::try_with_port(port, port_name).unwrap_or_else(|_| Self::closed(fallback_name))
    }

    /// Fallible form of [`Self::with_port`]: fails when the writer thread
    /// cannot start, so every sender it returns has a writer draining its
    /// queue.
    pub fn try_with_port(port: Box<dyn SerialPort>, port_name: String) -> Result<Self, String> {
        Self::try_with_port_spawning(port, port_name, |builder, writer| builder.spawn(writer))
    }

    /// [`Self::try_with_port`] with the writer thread started by `spawn`, so
    /// a test can refuse it.
    fn try_with_port_spawning(
        mut port: Box<dyn SerialPort>,
        port_name: String,
        spawn: SpawnWriter,
    ) -> Result<Self, String> {
        let queue = Arc::new((
            Mutex::new(Queue {
                heap: BinaryHeap::new(),
                stopping: false,
            }),
            Condvar::new(),
        ));
        let written = Arc::new(AtomicU64::new(0));
        let dropped_late = Arc::new(AtomicU64::new(0));
        let write_errors = Arc::new(AtomicU64::new(0));

        let thread = {
            let queue = Arc::clone(&queue);
            let written = Arc::clone(&written);
            let dropped_late = Arc::clone(&dropped_late);
            let write_errors = Arc::clone(&write_errors);
            spawn(
                std::thread::Builder::new().name("rustel-serial".into()),
                Box::new(move || {
                    let (lock, condvar) = &*queue;
                    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if guard.stopping {
                            // Discard what is not yet due rather than waiting
                            // for it: a set that is ending must not hold the
                            // terminal for the length of its longest gate.
                            return;
                        }
                        let Some(Reverse(next)) = guard.heap.peek() else {
                            guard = condvar.wait(guard).unwrap_or_else(|e| e.into_inner());
                            continue;
                        };
                        let now = Instant::now();
                        if next.due > now {
                            let wait = next.due - now;
                            let (next_guard, _) = condvar
                                .wait_timeout(guard, wait)
                                .unwrap_or_else(|e| e.into_inner());
                            guard = next_guard;
                            continue;
                        }
                        let Some(Reverse(due)) = guard.heap.pop() else {
                            continue;
                        };
                        // Written with the lock released: a wedged adapter must
                        // not block the scheduler pushing new messages.
                        drop(guard);
                        if now.duration_since(due.due) > MAX_QUEUE_LATENESS {
                            dropped_late.fetch_add(1, Ordering::Relaxed);
                        } else if port.write_scheduled(due.due, &due.bytes).is_err() {
                            write_errors.fetch_add(1, Ordering::Relaxed);
                        } else {
                            written.fetch_add(1, Ordering::Relaxed);
                        }
                        guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    }
                }),
            )
            .map_err(|error| {
                format!("cannot start serial sender worker for \"{port_name}\": {error}")
            })?
        };

        Ok(Self {
            queue,
            seq: AtomicU64::new(0),
            written,
            dropped_late,
            refused_full: Arc::new(AtomicU64::new(0)),
            write_errors,
            port_name,
            thread: Some(thread),
        })
    }

    /// A sender with no writer, stopped from the start so it refuses every
    /// write: what [`Self::with_port`] returns when its writer cannot start.
    fn closed(port_name: String) -> Self {
        Self {
            queue: Arc::new((
                Mutex::new(Queue {
                    heap: BinaryHeap::new(),
                    stopping: true,
                }),
                Condvar::new(),
            )),
            seq: AtomicU64::new(0),
            written: Arc::new(AtomicU64::new(0)),
            dropped_late: Arc::new(AtomicU64::new(0)),
            refused_full: Arc::new(AtomicU64::new(0)),
            write_errors: Arc::new(AtomicU64::new(0)),
            port_name,
            thread: None,
        }
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Queue bytes for `due`. Never blocks. Returns whether the queue accepted
    /// the write so producer-side correlation cannot claim a stopped/full
    /// sender accepted it.
    pub fn send_at(&self, due: Instant, bytes: Vec<u8>) -> bool {
        let (lock, condvar) = &*self.queue;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.stopping {
            return false;
        }
        if guard.heap.len() >= MAX_QUEUED_MESSAGES {
            self.refused_full.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        guard.heap.push(Reverse(Queued {
            due,
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            bytes,
        }));
        drop(guard);
        condvar.notify_one();
        true
    }

    /// Drop every write still queued, without stopping the writer, so the
    /// sender carries on for whoever keeps it with nothing left over from
    /// before. A write already handed to the port still finishes.
    pub fn discard_queued(&self) {
        let (lock, _) = &*self.queue;
        lock.lock().unwrap_or_else(|e| e.into_inner()).heap.clear();
    }

    /// Stop the writer and wait until it has let go of the port.
    ///
    /// Dropping a sender detaches its writer, so that a wedged write can
    /// never hold up the thread doing the drop, and the port closes some
    /// time later. An exclusive port refuses a second open until then, so a
    /// thread about to open the same port again, and able to wait, closes
    /// the old sender with this first. What is still queued is discarded,
    /// as on a drop.
    pub fn close(mut self) {
        self.signal_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Tell the writer to stop, discarding what is not yet due. Whether
    /// anyone then waits for it is the caller's choice: `close` does, a drop
    /// does not.
    fn signal_stop(&self) {
        let (lock, condvar) = &*self.queue;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.stopping = true;
        condvar.notify_all();
    }

    pub fn report(&self) -> SerialReport {
        SerialReport {
            written: self.written.load(Ordering::Relaxed),
            dropped_late: self.dropped_late.load(Ordering::Relaxed),
            refused_full: self.refused_full.load(Ordering::Relaxed),
            write_errors: self.write_errors.load(Ordering::Relaxed),
        }
    }
}

impl Drop for SerialSender {
    fn drop(&mut self) {
        self.signal_stop();
        // Dropping a JoinHandle detaches. A wedged USB write can block forever
        // inside `port.write`; joining here placed that unbounded wait on the
        // live producer (and on process teardown that drops SerialOutputs).
        // MIDI detaches for the same class of driver wedge.
        let _ = self.thread.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(key: &str, value: &str) -> Field {
        Field {
            key: key.into(),
            value: value.into(),
        }
    }

    /// The canonical CCITT-FALSE check value.
    #[test]
    fn crc16_matches_the_published_check_value() {
        assert_eq!(crc16(b"123456789"), 0x29b1);
        assert_eq!(crc16(b""), 0, "strudel.cc returns 0 for an empty message");
    }

    #[test]
    fn an_action_message_is_parenthesised_and_comma_separated() {
        let message = format_message(
            Some("hit"),
            &[field("drum", "1"), field("velocity", "90")],
            false,
        );
        assert_eq!(message, "hit(drum:1,velocity:90)");
    }

    /// Preserve strudel.cc's concatenation without a separator when `action` is
    /// absent. Adding a separator changes the data received by existing sketches.
    #[test]
    fn without_an_action_the_pairs_run_together_exactly_as_strudel_writes_them() {
        let message = format_message(None, &[field("a", "1"), field("b", "2")], false);
        assert_eq!(message, "a:1b:2");
    }

    #[test]
    fn single_char_ids_shorten_the_action_and_the_keys_but_not_the_values() {
        let message = format_message(
            Some("hit"),
            &[field("drum", "12"), field("velocity", "90")],
            true,
        );
        assert_eq!(message, "h(d:12,v:90)");
    }

    #[test]
    fn the_crc_frame_is_pipe_then_big_endian_crc_then_semicolon() {
        let framed = with_crc("abc");
        let crc = crc16(b"abc");
        assert_eq!(&framed[..3], b"abc");
        assert_eq!(framed[3], b'|');
        assert_eq!(framed[4], (crc >> 8) as u8);
        assert_eq!(framed[5], (crc & 0xff) as u8);
        assert_eq!(framed[6], b';');
        assert_eq!(framed.len(), 7);
    }

    #[test]
    fn queued_writes_arrive_in_time_order_near_their_targets() {
        let capture = CapturePort::new();
        let sender = SerialSender::with_port(Box::new(capture.clone()), "capture".into());
        let start = Instant::now();
        sender.send_at(start + Duration::from_millis(120), b"third".to_vec());
        sender.send_at(start, b"first".to_vec());
        sender.send_at(start + Duration::from_millis(60), b"second".to_vec());

        std::thread::sleep(Duration::from_millis(320));
        let writes: Vec<Vec<u8>> = capture.writes().into_iter().map(|(_, b)| b).collect();
        assert_eq!(
            writes,
            vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec()],
            "time order was not restored"
        );
    }

    #[test]
    fn writes_past_their_moment_are_dropped_rather_than_written_late() {
        let capture = CapturePort::new();
        let sender = SerialSender::with_port(Box::new(capture.clone()), "capture".into());
        sender.send_at(
            Instant::now() - MAX_QUEUE_LATENESS - Duration::from_millis(200),
            b"stale".to_vec(),
        );
        sender.send_at(Instant::now(), b"fresh".to_vec());
        std::thread::sleep(Duration::from_millis(150));

        let writes = capture.writes();
        assert_eq!(writes.len(), 1, "{writes:?}");
        assert_eq!(writes[0].1, b"fresh".to_vec());
        assert_eq!(sender.report().dropped_late, 1);
    }

    #[test]
    fn a_full_queue_refuses_rather_than_growing_or_blocking() {
        let capture = CapturePort::new();
        let sender = SerialSender::with_port(Box::new(capture), "capture".into());
        let due = Instant::now() + Duration::from_secs(30);
        let mut accepted = 0;
        for _ in 0..(MAX_QUEUED_MESSAGES + 32) {
            accepted += usize::from(sender.send_at(due, b"x".to_vec()));
        }
        assert_eq!(accepted, MAX_QUEUED_MESSAGES);
        assert!(
            sender.report().refused_full >= 32,
            "queue did not refuse past its ceiling: {:?}",
            sender.report()
        );
    }

    /// A writer thread that cannot start fails the constructor with an error
    /// naming the port, rather than yielding a sender nothing drains.
    #[test]
    fn a_writer_that_cannot_start_fails_the_constructor() {
        let built = SerialSender::try_with_port_spawning(
            Box::new(CapturePort::new()),
            "refused".into(),
            |_, _| Err(std::io::Error::other("no threads left")),
        );
        let Err(message) = built else {
            panic!("a sender was built with no writer to drain its queue");
        };
        assert!(
            message.contains("\"refused\"") && message.contains("no threads left"),
            "{message}"
        );
    }

    /// A sender with no writer, as `with_port` falls back to, refuses every
    /// write and reports nothing done.
    #[test]
    fn a_sender_whose_worker_never_started_refuses_writes_rather_than_accepting_them() {
        let sender = SerialSender::closed("dead".into());
        assert!(
            !sender.send_at(Instant::now(), b"x".to_vec()),
            "a sender with no worker accepted a write nothing could deliver"
        );
        let report = sender.report();
        assert_eq!(
            (
                report.written,
                report.dropped_late,
                report.refused_full,
                report.write_errors
            ),
            (0, 0, 0, 0),
            "the report claimed activity from a sender with no worker: {report:?}"
        );
    }

    /// Reports add and subtract counter by counter, saturating rather than
    /// wrapping.
    #[test]
    fn reports_add_and_subtract_counter_by_counter() {
        let a = SerialReport {
            written: 5,
            dropped_late: 1,
            refused_full: 2,
            write_errors: 0,
        };
        let b = SerialReport {
            written: 3,
            dropped_late: 4,
            refused_full: 0,
            write_errors: u64::MAX,
        };
        let sum = a.plus(b);
        assert_eq!(
            (
                sum.written,
                sum.dropped_late,
                sum.refused_full,
                sum.write_errors
            ),
            (8, 5, 2, u64::MAX)
        );
        let back = sum.since(b);
        assert_eq!(
            (
                back.written,
                back.dropped_late,
                back.refused_full,
                back.write_errors
            ),
            (5, 1, 2, 0)
        );
        let floor = a.since(b);
        assert_eq!(
            (
                floor.written,
                floor.dropped_late,
                floor.refused_full,
                floor.write_errors
            ),
            (2, 0, 2, 0)
        );
        let ceiling = b.plus(b);
        assert_eq!(ceiling.write_errors, u64::MAX);
    }

    /// Discarding the queue keeps the sender: what was queued never goes
    /// out, and what is queued afterwards does.
    #[test]
    fn discarding_the_queue_keeps_the_sender_writing() {
        let capture = CapturePort::new();
        let sender = SerialSender::with_port(Box::new(capture.clone()), "capture".into());
        let start = Instant::now();
        sender.send_at(start + Duration::from_millis(60), b"before".to_vec());
        sender.discard_queued();
        sender.send_at(start + Duration::from_millis(120), b"after".to_vec());
        let deadline = start + Duration::from_secs(5);
        while sender.report().written + sender.report().dropped_late == 0
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let writes: Vec<Vec<u8>> = capture.writes().into_iter().map(|(_, b)| b).collect();
        assert!(
            writes.iter().all(|bytes| bytes == b"after"),
            "a discarded write went out: {writes:?}"
        );
        assert_eq!(sender.report().written + sender.report().dropped_late, 1);
    }

    /// `close` returns only once the writer has dropped the port, so the
    /// caller can open it again straight away.
    #[test]
    fn close_waits_until_the_port_is_let_go() {
        struct Watched(Arc<std::sync::atomic::AtomicBool>);
        impl SerialPort for Watched {
            fn write(&mut self, _bytes: &[u8]) -> Result<(), String> {
                Ok(())
            }
        }
        impl Drop for Watched {
            fn drop(&mut self) {
                // Slow enough that a close that did not wait would return
                // first.
                std::thread::sleep(Duration::from_millis(50));
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sender =
            SerialSender::with_port(Box::new(Watched(Arc::clone(&released))), "watched".into());
        sender.send_at(Instant::now() + Duration::from_secs(30), b"later".to_vec());
        sender.close();
        assert!(
            released.load(Ordering::SeqCst),
            "close returned before the port was let go"
        );
    }

    #[test]
    fn shutdown_does_not_wait_for_queued_writes() {
        let capture = CapturePort::new();
        let sender = SerialSender::with_port(Box::new(capture), "capture".into());
        sender.send_at(Instant::now() + Duration::from_secs(30), b"later".to_vec());
        let start = Instant::now();
        drop(sender);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "drop waited {:?} for a write due in 30s",
            start.elapsed()
        );
    }
}
