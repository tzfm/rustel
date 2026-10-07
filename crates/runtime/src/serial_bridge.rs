/*
serial_bridge.rs - Route scheduled onsets to a serial port
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Turning scheduled onsets into serial writes.
//!
//! A hap goes to serial when it carries `serialport`, which `.serial()` sets.
//! The score names a device, or uses `"default"` for the first available port.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustel_serial::{Field, SerialSender};

use crate::hap_json::{OnsetEventJson, ValueJson};

/// Cap on distinct serial port names retained for one set, matching MIDI's
/// per-generation ceiling so a score cycling unique names cannot open
/// unbounded worker threads.
pub const MAX_SERIAL_OUTPUT_PORTS: usize = 16;

/// Cap on a score-controlled port selector, matching MIDI.
pub const MAX_SERIAL_PORT_NAME_BYTES: usize = 1024;

/// Cap on framed message bytes retained from one hap.
pub const MAX_SERIAL_MESSAGE_BYTES: usize = 64 * 1024;

/// Cap on one field key or value string taken from a control.
const MAX_SERIAL_FIELD_CHARS: usize = 1024;

/// One onset's destination and framed bytes.
pub struct SerialOnset {
    pub onset_id: u64,
    pub generation: u64,
    pub port: String,
    pub baud: u32,
    pub bytes: Vec<u8>,
    /// Session-clock seconds, the same scale as `OnsetEventJson::target_time`.
    pub target_time: f64,
}

fn object(value: &ValueJson) -> Option<&serde_json::Map<String, serde_json::Value>> {
    match value {
        ValueJson::Raw(serde_json::Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// Convert scalar controls to text for the `key:value` serial protocol.
fn stringify(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::String(text) => {
            if text.chars().count() > MAX_SERIAL_FIELD_CHARS {
                return None;
            }
            Some(text.clone())
        }
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        // Structured values have no field encoding in this scalar protocol.
        _ => None,
    }
}

/// Whether this onset names a serial port, before any score-sized cloning.
pub fn serial_route(onset: &OnsetEventJson) -> Option<String> {
    let map = object(&onset.value)?;
    match map.get("serialport")? {
        serde_json::Value::String(text) if text.len() <= MAX_SERIAL_PORT_NAME_BYTES => {
            Some(text.clone())
        }
        serde_json::Value::Number(number) => {
            let text = number.to_string();
            (text.len() <= MAX_SERIAL_PORT_NAME_BYTES).then_some(text)
        }
        _ => None,
    }
}

/// Extract the serial intent of one onset, or `None` if it names no port or
/// exceeds the size budgets.
pub fn serial_onset(onset: &OnsetEventJson) -> Option<SerialOnset> {
    let map = object(&onset.value)?;
    let port = match map.get("serialport")? {
        serde_json::Value::String(text) if text.len() <= MAX_SERIAL_PORT_NAME_BYTES => text.clone(),
        serde_json::Value::Number(number) => {
            let text = number.to_string();
            if text.len() > MAX_SERIAL_PORT_NAME_BYTES {
                return None;
            }
            text
        }
        _ => return None,
    };
    let baud = map
        .get("serialbaud")
        .and_then(|value| value.as_f64())
        .filter(|baud| baud.is_finite() && *baud > 0.0 && *baud <= 4_000_000.0)
        .map(|baud| baud as u32)
        .unwrap_or(rustel_serial::DEFAULT_BAUD);
    let send_crc = map
        .get("serialcrc")
        .and_then(|value| value.as_f64())
        .is_some_and(|flag| flag != 0.0);
    let single_char_ids = map
        .get("serialshort")
        .and_then(|value| value.as_f64())
        .is_some_and(|flag| flag != 0.0);

    let action = map
        .get("action")
        .and_then(|value| value.as_str())
        .filter(|text| text.chars().count() <= MAX_SERIAL_FIELD_CHARS);
    let fields: Vec<Field> = map
        .iter()
        // Routing and framing options are ours, not the sketch's.
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "serialport" | "serialbaud" | "serialcrc" | "serialshort" | "action"
            )
        })
        .filter(|(key, _)| key.chars().count() <= MAX_SERIAL_FIELD_CHARS)
        .filter_map(|(key, value)| {
            Some(Field {
                key: key.clone(),
                value: stringify(value)?,
            })
        })
        .collect();

    let message = rustel_serial::format_message(action, &fields, single_char_ids);
    let bytes = if send_crc {
        rustel_serial::with_crc(&message)
    } else {
        message.into_bytes()
    };
    if bytes.len() > MAX_SERIAL_MESSAGE_BYTES {
        return None;
    }

    Some(SerialOnset {
        onset_id: onset.onset_id,
        generation: onset.generation,
        port,
        baud,
        bytes,
        target_time: onset.target_time,
    })
}

/// Cap on the distinct baud-mismatch notes one port reports in a set, so a
/// score cycling unique `serialbaud` values cannot grow the bookkeeping
/// without bound, and one such port cannot use up the notes another port's
/// first mismatch needs. Past the cap the writes keep flowing; the note is
/// what stops.
pub const MAX_SERIAL_BAUD_NOTES_PER_PORT: usize = 4;

/// What became of one onset handed to [`SerialOutputs::submit`].
#[derive(Debug)]
pub struct SerialSubmit {
    /// The write crossed the producer's side: into an open port's sender, or
    /// into the bounded pre-open queue of a port still opening, where it
    /// flushes in strike order when the open installs. The onset counts as
    /// accepted, as a `true` from MIDI's `submit_batch_for_generation_at`
    /// does.
    pub accepted: bool,
    /// A baud note, reported at most once per port and requested baud for
    /// the life of the set: the port is already open or opening at another
    /// baud, or is still opening for the last set and will be opened again
    /// at this one. The write still goes out, at the baud the port has for
    /// this set: a note is news, not a refusal.
    pub note: Option<String>,
}

/// One routing of an onset to its port: the step inside
/// [`SerialOutputs::submit`] that decides where the write goes.
struct SerialRoute<'a> {
    /// The handle to write through. `None` when the port is remembered as
    /// unopenable, and also while its open is still in flight: then either
    /// the write was held ([`Self::buffered`]) or the pre-open cap refused
    /// it, which is counted and told through [`SerialOutputs::poll`], never
    /// held.
    sender: Option<&'a SerialSender>,
    /// The baud note, as [`SerialSubmit::note`] has it.
    note: Option<String>,
    /// The onset's bytes crossed into the port's bounded pre-open queue
    /// while its open thread still holds the platform open, so nothing is
    /// left to send and the write is already accounted for: it flushes in
    /// strike order the moment the open installs, or is dropped with the
    /// port's failure.
    buffered: bool,
}

/// How loud a piece of [`SerialOutputs::poll`] news is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialNewsKind {
    /// An open failed: the port writes nothing for the rest of the set.
    Failure,
    /// A port is still opening. Nothing has failed; its writes wait for it.
    StillOpening,
    /// Writes were refused, dropped late or failed; the set plays on.
    Trouble,
}

impl SerialNewsKind {
    /// The `live_error` kind the CLI reports this under. A failure keeps the
    /// "serial" kind a refused open always had; the others are news, not
    /// failures, and get kinds of their own so they do not read as errors.
    pub fn live_kind(self) -> &'static str {
        match self {
            Self::Failure => "serial",
            Self::StillOpening => "serial-opening",
            Self::Trouble => "serial-trouble",
        }
    }
}

/// One thing [`SerialOutputs::poll`] has to tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialNews {
    pub kind: SerialNewsKind,
    pub message: String,
}

impl SerialNews {
    fn new(kind: SerialNewsKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// How a port is opened by selector and baud: the platform's stack, or a
/// stand-in for a test.
pub type SerialOpenFn = dyn Fn(&str, u32) -> Result<SerialSender, String> + Send + Sync + 'static;

struct SerialOpenResult {
    port: String,
    result: Result<SerialSender, String>,
}

/// An open whose thread has not reported yet.
struct InFlightOpen {
    baud: u32,
    since: Instant,
    /// The set that started it, as counted by [`SerialOutputs::begin_set`].
    set: u64,
}

/// Bound on platform opens in flight at once, across every set these
/// outputs play. Each open has a thread of its own, and one the driver never
/// answers can be neither joined nor called back, so this is also the most
/// threads a run of wedged adapters can strand - and such a driver keeps its
/// place for the life of the process. A set names at most
/// [`MAX_SERIAL_OUTPUT_PORTS`] ports and adopts any open still running for
/// one of them, so only opens an earlier set left wedged can reach it.
const MAX_SERIAL_OPENS_IN_FLIGHT: usize = MAX_SERIAL_OUTPUT_PORTS;

/// How long a port may take to open before the set is told it is still
/// opening. An offline Bluetooth SPP port takes seconds to answer, so this
/// is a notice, not a deadline: the open keeps running and its writes stay
/// held.
const SERIAL_OPEN_STALL_NOTICE: Duration = Duration::from_secs(2);

/// Bound on writes held for one port whose open is still in flight: the
/// pre-open ceiling MIDI's queue has too.
const MAX_PENDING_SERIAL_WRITES_PER_PORT: usize = crate::MAX_PENDING_OPEN_WRITES_PER_PORT;

/// Bound on writes held across every still-opening port, matching the
/// platform sender's own queue ceiling: the pre-open queue can never ask the
/// producer to stage more than a live port could have accepted anyway.
const MAX_PENDING_SERIAL_WRITES_TOTAL: usize = rustel_serial::MAX_QUEUED_MESSAGES;

/// Resolve a score's selector and open it: the only place a platform serial
/// port is opened, running on that port's open thread.
///
/// Empty and "default" selectors use the first port the system reports, so
/// enumeration lives here beside the open for the same reason: it walks the
/// platform's device graph on the score's behalf and has no deadline the
/// producer could enforce either.
fn open_serial_port(port: &str, baud: u32) -> Result<SerialSender, String> {
    let resolved = resolve_serial_selector(port, &SerialSender::ports()?)?;
    SerialSender::open(&resolved, baud)
}

/// Resolve only ports enumerated by the operating system.
fn resolve_serial_selector(port: &str, ports: &[String]) -> Result<String, String> {
    if port.is_empty() || port == "default" {
        ports
            .first()
            .cloned()
            .ok_or_else(|| "no serial ports available".into())
    } else {
        ports
            .iter()
            .find(|name| name.as_str() == port)
            .cloned()
            .ok_or_else(|| format!("serial port \"{port}\" is not a serial port this system lists"))
    }
}

/// One write held while its port's platform open is still in flight.
struct PendingSerialWrite {
    due: Instant,
    bytes: Vec<u8>,
}

/// A delivery trouble a set is told of once, as the hosts tell MIDI's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SerialTrouble {
    QueueFull,
    DroppedLate,
    WriteErrors,
}

impl SerialTrouble {
    const ALL: [Self; 3] = [Self::QueueFull, Self::DroppedLate, Self::WriteErrors];

    /// How many times it happened, by the report's count.
    fn count(self, report: &rustel_serial::SerialReport) -> u64 {
        match self {
            Self::QueueFull => report.refused_full,
            Self::DroppedLate => report.dropped_late,
            Self::WriteErrors => report.write_errors,
        }
    }

    fn text(self) -> &'static str {
        match self {
            Self::QueueFull => "serial output queue filled; a complete onset was refused",
            Self::DroppedLate => "serial writes arrived too late and were dropped",
            Self::WriteErrors => "a serial port rejected writes or disconnected",
        }
    }
}

/// What one set knows about its ports, and the next set starts without.
///
/// A set begins from `SetState::default()`, so a field added here starts
/// fresh with every set by construction. What outlives a set - the opens
/// still in flight, the senders it leaves the next one - lives on
/// [`SerialOutputs`] itself.
#[derive(Default)]
struct SetState {
    /// Each port this set names, with the baud the set first asked for it at
    /// and its sender once one is installed.
    open: Vec<(String, u32, Option<SerialSender>)>,
    /// The (index into `open`, requested baud) mismatches already noted, so
    /// a mismatch is reported once per set rather than on every onset.
    /// `open` only grows within a set, so an index names its port for the
    /// whole set.
    baud_noted: HashSet<(usize, u32)>,
    /// What the senders this set took from the last one had already counted,
    /// so [`SerialOutputs::report`] counts from zero with the set.
    carried: rustel_serial::SerialReport,
    /// Writes held while their port's open is in flight, in strike order.
    pending: HashMap<String, VecDeque<PendingSerialWrite>>,
    pending_total: usize,
    /// Writes the pre-open caps refused, counted into
    /// [`SerialOutputs::report`] beside the senders' own refusals and told
    /// once through [`SerialOutputs::poll`].
    pending_refused: u64,
    /// The ports of this set already told they are still opening, by index
    /// into `open`.
    stall_noted: HashSet<usize>,
    /// The delivery troubles this set was told of already.
    troubles_told: HashSet<SerialTrouble>,
    /// News not yet taken by [`SerialOutputs::poll`]. Open failures can land
    /// here between polls, one per port at most - a failure is remembered,
    /// never re-requested - so the port-count cap bounds it; the notices
    /// `poll` makes itself are drained in the same call.
    notices: VecDeque<SerialNews>,
}

/// Open ports, kept for the life of the set, and the opens that outlive one.
///
/// The platform open itself runs on a thread of its own
/// ([`Self::start_open`]): [`SerialOutputs::submit`] only starts it and
/// holds the onset's bytes, and a completed open - or a failure - installs
/// on a later tick. A failure is remembered so a score naming an adapter
/// that is not plugged in reports once and then plays on, rather than
/// retrying per onset.
///
/// One port in one set:
///
/// ```text
/// first onset --+-- kept sender fits ------------------------> OPEN
///               |
///               +-- otherwise --> OPENING -- Ok at the set's baud --> OPEN
///                                  ^  | |
///                                  |  | +-- Err, this set's open --> FAILED
///                                  |  |
///                                  +--+  Ok at another baud, or Err from an
///                                        earlier set's open: open again
/// ```
///
/// A kept sender fits when it opened at the same baud and has rejected no
/// write. OPENING holds writes in the bounded pre-open queue. OPEN flushes
/// them in strike order. FAILED drops them and lasts for the set. An open
/// that cannot start is FAILED at once.
///
/// A host that plays set after set keeps one of these and calls
/// [`Self::begin_set`] between them. An open still in flight cannot be
/// called back, so the next set adopts it rather than asking the driver for
/// the same port twice: an exclusive COM port refuses the second open while
/// the first is pending, and a wedged adapter answers it with one more
/// stranded thread. An adopted open is the set's own only if it succeeds at
/// the baud the set asked for; otherwise the set opens the port again, once
/// nothing holds it. A port the last set had open is kept the same way, its
/// queue emptied: the next set takes it as it is at the same baud, and at
/// another baud, or once it has rejected writes, it is closed before the
/// port is asked for again, rather than dropped to let go of the port some
/// time after a new open may already have been refused.
///
/// Ports are told apart by the score's selector, not by the device it
/// resolves to, because "default" (or "") resolves only on its open thread.
/// So "default" and the device's own name are two ports here, and naming
/// one while the other is still opening asks an exclusive device for a
/// second open, which it refuses.
pub struct SerialOutputs {
    opener: Arc<SerialOpenFn>,
    /// Completed opens, each pushed once by the thread that ran it.
    results: Arc<Mutex<VecDeque<SerialOpenResult>>>,
    /// Opens whose thread has not reported yet, by selector; these outlive
    /// the set that asked. An entry in the set's `open` with no sender is
    /// opening while its port is named here, and a remembered failure
    /// otherwise.
    in_flight: HashMap<String, InFlightOpen>,
    /// Senders the last set leaves this one, with the baud they opened at:
    /// the ports it had open, and opens an earlier set asked for that
    /// completed after it ended. The first onset of this set that names the
    /// port at that baud takes one as it is, unless it has rejected writes;
    /// otherwise it is closed on the new open's thread first. The next set
    /// drops what this one never took, which by then has sat idle for a
    /// whole set.
    adoptable: HashMap<String, (u32, SerialSender)>,
    /// Which set this is, counting [`Self::begin_set`] calls, so an open can
    /// tell whether this set started it.
    set: u64,
    /// Everything else, which starts fresh with each set.
    this_set: SetState,
}

impl Default for SerialOutputs {
    fn default() -> Self {
        Self::new()
    }
}

impl SerialOutputs {
    pub fn new() -> Self {
        Self::with_opener(Arc::new(open_serial_port))
    }

    /// Outputs that open ports through `opener` instead of the platform:
    /// a capture in a test, the real thing everywhere else.
    pub(crate) fn with_opener(opener: Arc<SerialOpenFn>) -> Self {
        Self {
            opener,
            results: Arc::new(Mutex::new(VecDeque::new())),
            in_flight: HashMap::new(),
            adoptable: HashMap::new(),
            set: 0,
            this_set: SetState::default(),
        }
    }

    /// Open later ports through `opener`. Only the opener changes: what is
    /// open, remembered or still in flight stays, so an open already running
    /// is still adopted and no port is asked for twice.
    pub fn set_opener(&mut self, opener: Arc<SerialOpenFn>) {
        self.opener = opener;
    }

    /// Start a new set on the same outputs.
    ///
    /// What the last set remembered or held goes with it: a failure is tried
    /// afresh, so an adapter plugged in since gets its frames, and no write
    /// the last set struck goes out in this one. Its ports are not dropped
    /// but kept, queues emptied, for this set to take at the same baud or
    /// close before opening again at another, and its opens still running
    /// stay in flight for this set to adopt. One that succeeds at the baud
    /// this set asks for is its sender; one that fails, or opens at another
    /// baud, was the last set's attempt, so this set opens the port again
    /// once nothing holds it.
    pub fn begin_set(&mut self) {
        self.set = self.set.wrapping_add(1);
        let last = std::mem::take(&mut self.this_set);
        // What the last set left untaken has sat idle all through it, so
        // its writer lets go of the port as soon as it is dropped.
        self.adoptable.clear();
        for (port, baud, sender) in last.open {
            if let Some(sender) = sender {
                sender.discard_queued();
                self.adoptable.insert(port, (baud, sender));
            }
        }
    }

    /// Take the news since the last call, each item once.
    ///
    /// An open failure cannot refuse the onset that asked for the port -
    /// that onset crossed into the pre-open queue, and the refusal arrives a
    /// tick later - so the live loop calls this every tick, beside its MIDI
    /// take, and reports each failure as a refused port. Beside the
    /// failures: a port still opening after [`SERIAL_OPEN_STALL_NOTICE`],
    /// once per port, and each delivery trouble in [`Self::report`], once -
    /// the troubles the hosts tell for MIDI, so a wedged open is never
    /// silent. Each item says how loud it is, so a host need not read a
    /// slow port as a failed one.
    pub fn poll(&mut self) -> Vec<SerialNews> {
        self.apply_open_results();
        self.note_stalled_opens(Instant::now());
        self.note_troubles();
        self.this_set.notices.drain(..).collect()
    }

    /// Install completed opens, flushing what was buffered for them, and
    /// remember completed failures with their buffered writes dropped.
    fn apply_open_results(&mut self) {
        let completed = std::mem::take(
            &mut *self
                .results
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for completed in completed {
            let Some(flight) = self.in_flight.remove(&completed.port) else {
                // Unreachable - an open is in flight from its start until
                // its one result is taken here - but a bookkeeping mismatch
                // must not panic the producer.
                continue;
            };
            let Some(index) = self
                .this_set
                .open
                .iter()
                .position(|(name, _, _)| *name == completed.port)
            else {
                // An earlier set's open, finished before this set named its
                // port. A sender is kept for this set to adopt rather than
                // closed and asked for again; a failure is forgotten, so the
                // port is tried afresh if this set names it.
                if let Ok(sender) = completed.result {
                    self.adoptable.insert(completed.port, (flight.baud, sender));
                }
                continue;
            };
            let wanted = self.this_set.open[index].1;
            match completed.result {
                Ok(sender) if flight.baud == wanted => {
                    let writes = self.take_pending(&completed.port);
                    // Flushed in the order the score struck them. The
                    // pre-open cap sits below the sender's own queue ceiling
                    // so this hand-off can never itself be the refusal it
                    // exists to prevent; anything the open delay made stale
                    // is the sender's to drop late, exactly as if it had
                    // been queued on time.
                    let sender = self.this_set.open[index].2.insert(sender);
                    for write in writes {
                        sender.send_at(write.due, write.bytes);
                    }
                }
                Ok(sender) => {
                    // An earlier set's open, at the baud that set asked for.
                    // This set asked for another, so it opens the port again
                    // at its own; the new open's thread closes this handle
                    // first, and the held writes wait for the new one.
                    self.reopen(&completed.port, wanted, Some(sender));
                }
                Err(message) if flight.set == self.set => {
                    // Nowhere to send what was buffered while the open was
                    // in flight; the failure message is the news, and the
                    // entry keeps no sender for the life of the set so the
                    // port is never re-opened per onset.
                    self.take_pending(&completed.port);
                    self.this_set
                        .notices
                        .push_back(SerialNews::new(SerialNewsKind::Failure, message));
                }
                Err(_) => {
                    // An earlier set's open, adopted and then failed. That
                    // says nothing about the port this set asked for - the
                    // adapter may have come up since the restart - and a
                    // failed open holds nothing, so this set gets one try
                    // of its own, with its writes still held.
                    self.reopen(&completed.port, wanted, None);
                }
            }
        }
    }

    /// Open `port` again at `baud` for this set, closing `replacing` first,
    /// keeping its held writes; if that cannot even start, it is this set's
    /// failure like any other.
    fn reopen(&mut self, port: &str, baud: u32, replacing: Option<SerialSender>) {
        if let Err(message) = self.start_open(port, baud, replacing) {
            self.take_pending(port);
            self.this_set
                .notices
                .push_back(SerialNews::new(SerialNewsKind::Failure, message));
        }
    }

    /// Take the writes held for `port`, in strike order.
    fn take_pending(&mut self, port: &str) -> VecDeque<PendingSerialWrite> {
        let set = &mut self.this_set;
        let writes = set.pending.remove(port).unwrap_or_default();
        set.pending_total = set.pending_total.saturating_sub(writes.len());
        writes
    }

    /// Tell each port of this set that has been opening for longer than
    /// [`SERIAL_OPEN_STALL_NOTICE`], once. A wedged driver never completes,
    /// so without this its port would be silent with no word about why.
    fn note_stalled_opens(&mut self, now: Instant) {
        let set = &mut self.this_set;
        for (index, (port, _, sender)) in set.open.iter().enumerate() {
            if sender.is_some() || set.stall_noted.contains(&index) {
                continue;
            }
            let Some(flight) = self.in_flight.get(port) else {
                continue;
            };
            let waited = now.saturating_duration_since(flight.since);
            if waited < SERIAL_OPEN_STALL_NOTICE {
                continue;
            }
            set.stall_noted.insert(index);
            set.notices.push_back(SerialNews::new(
                SerialNewsKind::StillOpening,
                format!(
                    "serial port \"{port}\" is still opening after {}s; up to \
                     {MAX_PENDING_SERIAL_WRITES_PER_PORT} of its writes wait for it, \
                     and any stale by the time it opens are dropped",
                    waited.as_secs()
                ),
            ));
        }
    }

    /// Tell each delivery trouble once, as both hosts tell MIDI's. The
    /// counters start from zero with the set, so any count at all is news.
    fn note_troubles(&mut self) {
        let report = self.report();
        let set = &mut self.this_set;
        for trouble in SerialTrouble::ALL {
            if trouble.count(&report) > 0 && set.troubles_told.insert(trouble) {
                set.notices
                    .push_back(SerialNews::new(SerialNewsKind::Trouble, trouble.text()));
            }
        }
    }

    /// Hold one write for a port whose platform open is still in flight.
    ///
    /// Bounded per port and process-wide, as MIDI's pre-open queue is: a
    /// wedged open must not turn the producer's staging into unbounded
    /// memory. Returns whether the write was taken; a refusal is counted,
    /// never blocked on. Past the cap the newest write is the one refused,
    /// as MIDI refuses its newest batch: the cap is a thousand writes deep,
    /// which a set fills only against an open that never returns, and then
    /// none of them is ever written.
    fn queue_pending_write(&mut self, port: &str, due: Instant, bytes: &[u8]) -> bool {
        let set = &mut self.this_set;
        let port_queued = set.pending.get(port).map_or(0, VecDeque::len);
        if port_queued >= MAX_PENDING_SERIAL_WRITES_PER_PORT
            || set.pending_total >= MAX_PENDING_SERIAL_WRITES_TOTAL
        {
            set.pending_refused = set.pending_refused.saturating_add(1);
            return false;
        }
        set.pending
            .entry(port.to_string())
            .or_default()
            .push_back(PendingSerialWrite {
                due,
                bytes: bytes.to_vec(),
            });
        set.pending_total += 1;
        true
    }

    /// Start the platform open of `port` at `baud` on a thread of its own,
    /// closing `replacing` on that thread first when this opens a port again.
    ///
    /// `serialport`'s timeout configures the port's COMMTIMEOUTS - the
    /// deadline of reads and writes already issued - not how long the open
    /// call itself may take. An offline Bluetooth SPP port answers the
    /// platform open in 3-10 seconds and a wedged USB-CDC driver may never
    /// answer, so an open on the live producer would block every output the
    /// engine has until the driver returns. The producer only
    /// starts the thread and later drains its one result; the thread holds
    /// no lock while asking the platform for a port, and is never joined,
    /// because the driver may hold the open with no deadline at all. A
    /// thread per port rather than one worker for all, as MIDI outputs have
    /// (`MidiOpenManager`): serial ports are few, and a wedged adapter then
    /// holds up its own port and nothing else - not another port, not the
    /// "default" lookup. The serial crate already quarantines wedged writes
    /// the same way, so the producer makes no platform serial call at all.
    fn start_open(
        &mut self,
        port: &str,
        baud: u32,
        replacing: Option<SerialSender>,
    ) -> Result<(), String> {
        if self.in_flight.len() >= MAX_SERIAL_OPENS_IN_FLIGHT {
            return Err(format!(
                "{MAX_SERIAL_OPENS_IN_FLIGHT} serial port opens are still waiting on \
                 their drivers, so \"{port}\" was not opened; a driver that never \
                 answers keeps its place until rustel restarts"
            ));
        }
        let opener = Arc::clone(&self.opener);
        let results = Arc::clone(&self.results);
        let request = port.to_string();
        std::thread::Builder::new()
            .name("rustel-serial-open".into())
            .spawn(move || {
                // Dropping a sender lets its port go on the sender's own
                // thread, some time later; an exclusive port refuses a new
                // open until then, so this waits for it here, where waiting
                // holds up nothing else.
                if let Some(replacing) = replacing {
                    replacing.close();
                }
                // The platform open runs with no lock held and no deadline
                // of ours: this thread is the only thing a wedged driver
                // gets to hold up. A panic is a failure like any other, so
                // the open's place is always given back.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    opener(&request, baud)
                }))
                .unwrap_or_else(|_| {
                    Err(format!("the opener for serial port \"{request}\" panicked"))
                });
                results
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push_back(SerialOpenResult {
                        port: request,
                        result,
                    });
            })
            .map_err(|error| {
                format!("cannot start the opener for serial port \"{port}\": {error}")
            })?;
        // Only this thread takes results, so the entry is in before the
        // open's result can be looked at, however fast the open is.
        self.in_flight.insert(
            port.to_string(),
            InFlightOpen {
                baud,
                since: Instant::now(),
                set: self.set,
            },
        );
        Ok(())
    }

    /// Give `port` its entry for this set at `baud` and return its index,
    /// with a note when it adopts an open at another baud: an open an
    /// earlier set left behind is adopted, anything else starts here.
    fn name_port(&mut self, port: &str, baud: u32) -> Result<(usize, Option<String>), String> {
        if self.this_set.open.len() >= MAX_SERIAL_OUTPUT_PORTS {
            return Err(format!(
                "a score may name at most {MAX_SERIAL_OUTPUT_PORTS} serial ports at once; \
                 \"{port}\" was not opened"
            ));
        }
        let index = self.this_set.open.len();
        // The entry exists before any open does so a completed open always
        // finds its port. If the open cannot even start, the entry stays as
        // the remembered failure so the score is not retried on every
        // onset.
        self.this_set.open.push((port.to_string(), baud, None));
        if let Some((opened_baud, sender)) = self.adoptable.remove(port) {
            // Taken as it is only if it has never rejected a write: a
            // handle that has may belong to an adapter since unplugged, and
            // one plugged back in answers only a fresh open.
            if opened_baud == baud && sender.report().write_errors == 0 {
                self.this_set.carried = self.this_set.carried.plus(sender.report());
                self.this_set.open[index].2 = Some(sender);
                return Ok((index, None));
            }
            // Open at the baud an earlier set asked for, or refusing writes.
            // A restart applies the baud the set asks for and gives a
            // failing port a fresh try, so this one is closed on the new
            // open's thread and the port opened again at this set's baud.
            self.start_open(port, baud, Some(sender))?;
            return Ok((index, None));
        }
        if let Some(flight) = self.in_flight.get(port) {
            // Still running for an earlier set: adopted, never asked for
            // twice. At another baud it is opened again at this one as soon
            // as it has finished, and the set is told why its writes wait.
            let note = (flight.baud != baud).then(|| {
                format!(
                    "serial port \"{port}\" is still opening at {} baud for the last set; \
                     it opens again at {baud} once that finishes, and writes wait for it",
                    flight.baud
                )
            });
            return Ok((index, note));
        }
        self.start_open(port, baud, None)?;
        Ok((index, None))
    }

    /// The note for an onset asking the port at `index` for a baud other
    /// than the one this set opens it at, if that is news: once per port and
    /// baud, and at most [`MAX_SERIAL_BAUD_NOTES_PER_PORT`] per port.
    fn baud_note(&mut self, index: usize, baud: u32) -> Option<String> {
        let set = &mut self.this_set;
        let (port, opened_baud, sender) = &set.open[index];
        if *opened_baud == baud
            || set.baud_noted.contains(&(index, baud))
            || set
                .baud_noted
                .iter()
                .filter(|(noted, _)| *noted == index)
                .count()
                >= MAX_SERIAL_BAUD_NOTES_PER_PORT
        {
            return None;
        }
        let state = if sender.is_some() { "open" } else { "opening" };
        let note = format!(
            "serial port \"{port}\" is already {state} at {opened_baud} baud; \
             serialbaud {baud} was ignored, and writes go out at \
             {opened_baud} until the set restarts"
        );
        set.baud_noted.insert((index, baud));
        Some(note)
    }

    /// Hand one onset's `bytes` for `due` to `port`, opening it at `baud` on
    /// first use - off this thread - and say whether the write was taken.
    ///
    /// The bridge finishes the send itself, as MIDI's
    /// `submit_batch_for_generation_at` does: into the port's sender when it
    /// is open, into its bounded pre-open queue while it opens. A first onset
    /// for a port never waits for the platform; its bytes wait in that queue
    /// and flush in order on a later tick when the open installs. A port
    /// that turns out unopenable is told of by [`Self::poll`], one tick
    /// late, and then remembered silently; its onsets are not accepted.
    ///
    /// The first onset to name a port sets its baud for the set. A later
    /// onset that names it at a different baud - while it is open or still
    /// opening - reuses that open and gets a [`SerialSubmit::note`] saying so
    /// once: changing baud mid-set would drop the adapter's framing, while
    /// the write itself still goes out at the port's baud. "serialbaud
    /// {baud} was ignored" means the baud was ignored, not the write.
    pub fn submit(
        &mut self,
        port: &str,
        baud: u32,
        due: Instant,
        bytes: Vec<u8>,
    ) -> Result<SerialSubmit, String> {
        let SerialRoute {
            sender,
            note,
            buffered,
        } = self.sender(port, baud, due, &bytes)?;
        let accepted = buffered || sender.is_some_and(|sender| sender.send_at(due, bytes));
        Ok(SerialSubmit { accepted, note })
    }

    /// Route one onset to `port`: the step of [`Self::submit`] that opens
    /// the port on first use, holds the write while it opens, and otherwise
    /// hands back the sender to write through.
    fn sender(
        &mut self,
        port: &str,
        baud: u32,
        due: Instant,
        bytes: &[u8],
    ) -> Result<SerialRoute<'_>, String> {
        self.apply_open_results();
        if port.len() > MAX_SERIAL_PORT_NAME_BYTES {
            return Err(format!(
                "a serial port selector may contain at most {MAX_SERIAL_PORT_NAME_BYTES} UTF-8 bytes"
            ));
        }
        let (index, adopted) = match self
            .this_set
            .open
            .iter()
            .position(|(name, _, _)| name == port)
        {
            Some(index) => (index, None),
            None => self.name_port(port, baud)?,
        };
        let installed = self.this_set.open[index].2.is_some();
        let opening = !installed && self.in_flight.contains_key(port);
        // A remembered failure writes nothing, so its baud is no news.
        let note = if adopted.is_some() {
            adopted
        } else if installed || opening {
            self.baud_note(index, baud)
        } else {
            None
        };
        if opening {
            // The port's open is still running; an onset's write waits with
            // the bridge, not with the driver.
            let buffered = self.queue_pending_write(port, due, bytes);
            return Ok(SerialRoute {
                sender: None,
                note,
                buffered,
            });
        }
        Ok(SerialRoute {
            sender: self.this_set.open[index].2.as_ref(),
            note,
            buffered: false,
        })
    }

    pub fn report(&self) -> rustel_serial::SerialReport {
        // Pre-open refusals are the same news as a sender's own full queue:
        // a complete onset the serial path could not take. A sender taken
        // from the last set brings its counts along; this set reports only
        // what came after.
        let refused_before_open = rustel_serial::SerialReport {
            refused_full: self.this_set.pending_refused,
            ..rustel_serial::SerialReport::default()
        };
        self.this_set
            .open
            .iter()
            .filter_map(|(_, _, sender)| sender.as_ref())
            .fold(refused_before_open, |total, sender| {
                total.plus(sender.report())
            })
            .since(self.this_set.carried)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_serial_selector_must_name_an_enumerated_port() {
        let ports = ["/dev/ttyUSB0".into(), "COM3".into()];
        assert_eq!(
            resolve_serial_selector("/dev/pts/3", &ports),
            Err("serial port \"/dev/pts/3\" is not a serial port this system lists".into())
        );
        assert_eq!(
            resolve_serial_selector("/dev/ttyUSB0", &ports),
            Ok("/dev/ttyUSB0".into())
        );
        assert_eq!(resolve_serial_selector("COM3", &ports), Ok("COM3".into()));
        assert!(resolve_serial_selector("com3", &ports).is_err());
        assert!(resolve_serial_selector("/dev/ttyUSB0/", &ports).is_err());
    }

    #[test]
    fn default_serial_selectors_require_an_enumerated_port() {
        let ports = ["/dev/ttyUSB0".into()];
        for selector in ["", "default"] {
            assert_eq!(
                resolve_serial_selector(selector, &ports),
                Ok("/dev/ttyUSB0".into())
            );
            assert_eq!(
                resolve_serial_selector(selector, &[]),
                Err("no serial ports available".into())
            );
        }
        assert!(resolve_serial_selector("/dev/ttyUSB0", &[]).is_err());
    }

    #[test]
    fn security_docs_describe_live_midi_and_serial_output() {
        let security = include_str!("../../../SECURITY.md");
        let devices = security
            .split_once("## MIDI and serial devices")
            .expect("device security section")
            .1
            .split("\n## ")
            .next()
            .expect("device security text");
        assert!(devices.contains("CLI live playback and Studio open and write"));
        assert!(devices.contains("MIDI or serial ports a score names"));
        assert!(!devices.contains("drains them as unsupported"));
    }

    fn onset(value: serde_json::Value) -> OnsetEventJson {
        OnsetEventJson {
            onset_id: 1,
            generation: 1,
            whole_begin: "0/1".into(),
            duration_secs: 0.5,
            target_time: 4.0,
            live_controls: [0; 2],
            ui_visuals: 0,
            value: ValueJson::Raw(value),
            value_show: String::new(),
            log_line: None,
        }
    }

    #[test]
    fn a_hap_without_a_port_is_not_a_serial_hap() {
        assert!(serial_onset(&onset(serde_json::json!({ "s": "bd" }))).is_none());
    }

    #[test]
    fn an_action_hap_frames_as_strudel_does() {
        let got = serial_onset(&onset(serde_json::json!({
            "serialport": "/dev/ttyUSB0",
            "action": "hit",
            "drum": 1,
        })))
        .unwrap();
        assert_eq!(got.onset_id, 1);
        assert_eq!(got.generation, 1);
        assert_eq!(got.port, "/dev/ttyUSB0");
        assert_eq!(got.baud, rustel_serial::DEFAULT_BAUD);
        assert_eq!(String::from_utf8(got.bytes).unwrap(), "hit(drum:1)");
    }

    #[test]
    fn the_framing_options_do_not_leak_into_the_message() {
        let got = serial_onset(&onset(serde_json::json!({
            "serialport": "COM3",
            "serialbaud": 9600,
            "serialcrc": 1,
            "serialshort": 0,
            "action": "go",
            "n": 3,
        })))
        .unwrap();
        assert_eq!(got.baud, 9600);
        let text = String::from_utf8_lossy(&got.bytes).to_string();
        assert!(text.starts_with("go(n:3)"), "{text}");
        assert!(!text.contains("serial"), "framing options leaked: {text}");
        // CRC requested, so the trailer is present.
        assert_eq!(*got.bytes.last().unwrap(), b';');
        assert_eq!(got.bytes[got.bytes.len() - 4], b'|');
    }

    #[test]
    fn without_crc_the_bytes_are_just_the_message() {
        let got = serial_onset(&onset(serde_json::json!({
            "serialport": "x",
            "action": "a",
        })))
        .unwrap();
        assert_eq!(String::from_utf8(got.bytes).unwrap(), "a()");
    }

    /// However strange the score, the bytes must stay a frame a sketch can
    /// parse rather than something that desynchronises its reader.
    #[test]
    fn hostile_values_still_produce_a_parseable_frame() {
        let got = serial_onset(&onset(serde_json::json!({
            "serialport": "x",
            "serialcrc": 1,
            "action": "a",
            "nested": { "k": 1 },
            "list": [1, 2],
            "big": 1e308,
            "text": "value",
        })))
        .unwrap();
        let text = String::from_utf8_lossy(&got.bytes).to_string();
        assert!(text.starts_with("a("), "{text}");
        // Objects and arrays are dropped rather than interpolated as noise.
        assert!(!text.contains("object"), "{text}");
        assert!(!text.contains('['), "{text}");
        assert_eq!(*got.bytes.last().unwrap(), b';');
    }

    #[test]
    fn an_oversized_port_name_is_refused() {
        let huge = "p".repeat(MAX_SERIAL_PORT_NAME_BYTES + 1);
        assert!(
            serial_onset(&onset(serde_json::json!({
                "serialport": huge,
                "action": "a",
            })))
            .is_none()
        );
    }

    #[test]
    fn a_port_that_cannot_be_opened_is_remembered_rather_than_retried() {
        let mut outputs = SerialOutputs::with_opener(std::sync::Arc::new(|port, _baud| {
            Err(format!("could not open serial port \"{port}\": not there"))
        }));
        let due = std::time::Instant::now();
        // The open runs on the port's own thread, so the onset that asked for
        // the port only buffers; the refusal is the thread's to discover and
        // `poll`'s to deliver, one tick late.
        let route = outputs
            .sender("/dev/definitely-not-a-serial-port", 115_200, due, b"x()")
            .expect("the producer was made to wait for the platform open");
        assert!(route.buffered);
        let errors = poll_until_reported(&mut outputs);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].kind, SerialNewsKind::Failure);
        assert!(
            errors[0]
                .message
                .contains("/dev/definitely-not-a-serial-port"),
            "{}",
            errors[0].message
        );

        // Remembered, not retried: later onsets of the port route nowhere and
        // say nothing more.
        let route = outputs
            .sender("/dev/definitely-not-a-serial-port", 115_200, due, b"x()")
            .expect("a remembered failed port is not an error");
        assert!(
            route.sender.is_none(),
            "a failed port was retried instead of being remembered"
        );
        assert!(!route.buffered);
        assert!(outputs.poll().is_empty(), "the failure repeated");
        assert!(outputs.in_flight.is_empty(), "the port was opened again");
        assert!(
            outputs.this_set.pending.is_empty(),
            "buffered writes outlived the failure"
        );
    }

    /// Whether a stand-in port may let go when its writer drops it: a test can
    /// shut this to prove who waits for the letting go.
    type DropGate = std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>;

    /// A stand-in port that counts itself as held until its writer lets it go,
    /// and lets go slowly - and not at all while its gate is shut - so an open
    /// that did not wait for the letting go would find it still held.
    struct HeldPort {
        capture: rustel_serial::CapturePort,
        held: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        gate: DropGate,
    }

    impl rustel_serial::SerialPort for HeldPort {
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            rustel_serial::SerialPort::write(&mut self.capture, bytes)
        }
    }

    impl Drop for HeldPort {
        fn drop(&mut self) {
            let (lock, opened) = &*self.gate;
            let shut = lock.lock().unwrap_or_else(|error| error.into_inner());
            // Bounded, so a test that fails with the gate shut strands nothing
            // for long.
            drop(opened.wait_timeout_while(shut, std::time::Duration::from_secs(10), |shut| *shut));
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.held.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A stand-in for a driver that takes its time: every open counts itself,
    /// notes the baud it was asked for and how many of its ports were still
    /// held when it began, says which port it is opening, and then holds until
    /// the test releases it with the outcome it should have. Dropping this ends
    /// every open still held, so no test leaves a thread waiting on it.
    struct GatedOpener {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        bauds: std::sync::Arc<std::sync::Mutex<Vec<u32>>>,
        held_on_entry: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
        gate: DropGate,
        entered: std::sync::mpsc::Receiver<String>,
        release: std::sync::mpsc::Sender<Result<(), String>>,
        capture: rustel_serial::CapturePort,
    }

    impl GatedOpener {
        fn new() -> (std::sync::Arc<SerialOpenFn>, Self) {
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let bauds = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let held = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let held_on_entry = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let gate: DropGate =
                std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
            let capture = rustel_serial::CapturePort::new();
            let (entered_tx, entered) = std::sync::mpsc::channel();
            let (release, release_rx) = std::sync::mpsc::channel::<Result<(), String>>();
            let release_rx = std::sync::Mutex::new(release_rx);
            let counted = std::sync::Arc::clone(&calls);
            let asked = std::sync::Arc::clone(&bauds);
            let noted = std::sync::Arc::clone(&held_on_entry);
            let ports_gate = std::sync::Arc::clone(&gate);
            let opened = capture.clone();
            let opener: std::sync::Arc<SerialOpenFn> = std::sync::Arc::new(move |port, baud| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                asked
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(baud);
                noted
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(held.load(std::sync::atomic::Ordering::SeqCst));
                entered_tx
                    .send(port.to_string())
                    .map_err(|_| "open observer disappeared".to_string())?;
                release_rx
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .map_err(|_| "test opener was not released".to_string())??;
                held.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(SerialSender::with_port(
                    Box::new(HeldPort {
                        capture: opened.clone(),
                        held: std::sync::Arc::clone(&held),
                        gate: std::sync::Arc::clone(&ports_gate),
                    }),
                    port.to_string(),
                ))
            });
            (
                opener,
                Self {
                    calls,
                    bauds,
                    held_on_entry,
                    gate,
                    entered,
                    release,
                    capture,
                },
            )
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }

        /// The baud each open was asked for, in order.
        fn bauds(&self) -> Vec<u32> {
            self.bauds
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }

        /// How many of the stand-in's ports were still held as each open began.
        fn held_on_entry(&self) -> Vec<usize> {
            self.held_on_entry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }

        /// Shut or open the gate every stand-in port waits at to let go.
        fn shut_drops(&self, shut: bool) {
            let (lock, opened) = &*self.gate;
            *lock.lock().unwrap_or_else(|error| error.into_inner()) = shut;
            opened.notify_all();
        }

        /// Wait until an open is inside the stand-in driver, and name its port.
        fn wait_entered(&self) -> String {
            self.entered
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("no open ever reached the driver")
        }

        fn release(&self, outcome: Result<(), String>) {
            self.release
                .send(outcome)
                .expect("the held open gave up waiting");
        }

        /// The bytes written so far, once every write the sender took has been
        /// written or dropped late. A runner stalled past the late-drop window
        /// drops a write late rather than writing it, which is the sender's
        /// timing, not the bridge losing it; the bytes that did go out are still
        /// in the order they were handed over.
        fn settled_writes(&self, outputs: &SerialOutputs, expected: usize) -> Vec<Vec<u8>> {
            let settled = |outputs: &SerialOutputs| {
                let report = outputs.report();
                report.written + report.dropped_late
            };
            wait_until(|| settled(outputs) >= expected as u64);
            assert_eq!(
                settled(outputs),
                expected as u64,
                "the writes never reached the port: {:?}",
                outputs.report()
            );
            self.capture
                .writes()
                .into_iter()
                .map(|(_, bytes)| bytes)
                .collect()
        }
    }

    /// A producer-side `sender` or `poll` returns at once while an open is
    /// held inside the driver. The writes wait in the pre-open queue and flush
    /// in strike order when the open completes.
    #[test]
    fn a_wedged_open_never_blocks_the_producer_and_buffers_flush_in_order() {
        let (mut outputs, driver) = gated_outputs();
        // One due for both writes, on purpose: the sender's heap orders unequal
        // dues by itself, so only a tie leaves the order to the flush.
        let due = std::time::Instant::now() + std::time::Duration::from_millis(300);
        // The driver holds each open for up to ten seconds, so a producer call
        // that waited on it could not finish inside this bound, however slow
        // the runner.
        let bound = std::time::Duration::from_secs(5);
        let asked = std::time::Instant::now();
        let route = outputs
            .sender("slow-open", 115_200, due, b"first()")
            .expect("admission while the open is wedged");
        assert!(route.buffered, "the producer was made to wait for the open");
        assert!(asked.elapsed() < bound, "waited {:?}", asked.elapsed());
        assert_eq!(driver.wait_entered(), "slow-open");
        let asked = std::time::Instant::now();
        let route = outputs
            .sender("slow-open", 115_200, due, b"second()")
            .expect("second admission while the open is wedged");
        assert!(route.buffered);
        let news = outputs.poll();
        assert!(
            asked.elapsed() < bound,
            "the producer waited {:?} on an open inside the driver",
            asked.elapsed()
        );
        assert!(
            news.iter()
                .all(|notice| notice.kind != SerialNewsKind::Failure),
            "a held open is no failure: {news:?}"
        );
        driver.release(Ok(()));
        poll_until_opened(&mut outputs, "slow-open");
        let writes = driver.settled_writes(&outputs, 2);
        let struck = [b"first()".to_vec(), b"second()".to_vec()];
        assert_eq!(
            writes,
            struck[..writes.len()],
            "the buffered writes did not flush in strike order"
        );
        let news = outputs.poll();
        assert_eq!(
            news.len(),
            usize::from(outputs.report().dropped_late > 0),
            "a successful open was reported as a failure: {news:?}"
        );
        assert_eq!(driver.calls(), 1);
    }

    /// A wedged open must not turn the producer's staging into unbounded
    /// memory: the pre-open queue is bounded per port, and a write past the cap
    /// is refused, counted exactly once, and told once.
    #[test]
    fn a_wedged_open_buffers_only_a_bounded_number_of_writes() {
        let (mut outputs, _driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_secs(30);
        for index in 0..MAX_PENDING_SERIAL_WRITES_PER_PORT {
            let route = outputs
                .sender("wedged", 115_200, due, b"x()")
                .expect("a bounded queue is not an error");
            assert!(route.buffered, "write {index} was refused early");
        }
        let route = outputs
            .sender("wedged", 115_200, due, b"x()")
            .expect("a refused write is not an error either");
        assert!(!route.buffered, "the per-port cap did not refuse");
        assert!(route.sender.is_none());
        assert_eq!(outputs.report().refused_full, 1, "{:?}", outputs.report());
        // Counted by kind: a runner slow enough to cross the stalled open notice
        // adds that one too, which is not this test's business.
        let refusals = |news: Vec<SerialNews>| {
            news.iter()
                .filter(|notice| notice.kind == SerialNewsKind::Trouble)
                .filter(|notice| notice.message.contains("queue filled"))
                .count()
        };
        assert_eq!(refusals(outputs.poll()), 1, "the refusal was not told");
        outputs
            .sender("wedged", 115_200, due, b"x()")
            .expect("a refused write is not an error either");
        assert_eq!(outputs.report().refused_full, 2);
        assert_eq!(refusals(outputs.poll()), 0, "the refusal was told twice");
    }

    /// The pre-open queue is also capped across ports, at the sender's own
    /// ceiling: enough wedged ports filled to their own cap refuse the next
    /// port's very first write.
    #[test]
    fn the_pre_open_queue_is_capped_across_ports() {
        let full_ports = MAX_PENDING_SERIAL_WRITES_TOTAL / MAX_PENDING_SERIAL_WRITES_PER_PORT;
        assert_eq!(
            MAX_PENDING_SERIAL_WRITES_TOTAL % MAX_PENDING_SERIAL_WRITES_PER_PORT,
            0,
            "the ports below fill the total exactly"
        );
        assert!(full_ports < MAX_SERIAL_OUTPUT_PORTS.min(MAX_SERIAL_OPENS_IN_FLIGHT));
        let (mut outputs, _driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_secs(30);
        for port in 0..full_ports {
            let name = format!("wedged-{port}");
            for _ in 0..MAX_PENDING_SERIAL_WRITES_PER_PORT {
                let route = outputs
                    .sender(&name, 115_200, due, b"x()")
                    .expect("a bounded queue is not an error");
                assert!(route.buffered, "{name} was refused below the total");
            }
        }
        assert_eq!(
            outputs.this_set.pending_total,
            MAX_PENDING_SERIAL_WRITES_TOTAL
        );
        let route = outputs
            .sender("one-more", 115_200, due, b"x()")
            .expect("a refused write is not an error");
        assert!(!route.buffered, "the process-wide cap did not refuse");
        assert!(route.sender.is_none());
        assert_eq!(outputs.report().refused_full, 1, "{:?}", outputs.report());
        assert!(
            outputs.in_flight.contains_key("one-more"),
            "the refused write's port still opens"
        );
    }

    /// Each port opens on its own thread: a port whose driver never answers
    /// holds up itself and nothing else. The wedged one here is the "default"
    /// selector, whose lookup runs on that same thread.
    #[test]
    fn a_wedged_open_holds_up_only_its_own_port() {
        let (gated, driver) = GatedOpener::new();
        let capture = rustel_serial::CapturePort::new();
        let opened = capture.clone();
        let mut outputs = SerialOutputs::with_opener(std::sync::Arc::new(move |port, baud| {
            if port == "default" {
                return gated(port, baud);
            }
            Ok(SerialSender::with_port(
                Box::new(opened.clone()),
                port.to_string(),
            ))
        }));
        let due = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let route = outputs
            .sender("default", 115_200, due, b"x()")
            .expect("admission while the lookup is wedged");
        assert!(route.buffered);
        assert_eq!(driver.wait_entered(), "default");
        let route = outputs
            .sender("COM-PROMPT", 115_200, due, b"y()")
            .expect("admission of another port");
        assert!(route.buffered);
        poll_until_opened(&mut outputs, "COM-PROMPT");
        assert!(
            outputs.in_flight.contains_key("default"),
            "the wedged open finished after all"
        );
        let route = outputs
            .sender("COM-PROMPT", 115_200, due, b"z()")
            .expect("the prompt port routes");
        assert!(
            route.sender.is_some(),
            "the prompt port waited on the wedged one"
        );
    }

    /// Opens in flight are capped, which bounds the threads wedged drivers can
    /// strand: a set that names a port past the cap is told so, and the
    /// refusal is remembered rather than repeated per onset. A driver that
    /// answers at last gives its place back.
    #[test]
    fn opens_in_flight_are_capped_and_a_finished_one_gives_its_place_back() {
        let (mut outputs, driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_secs(30);
        // One set may name as many ports as there are places, so it fills them.
        const { assert!(MAX_SERIAL_OPENS_IN_FLIGHT <= MAX_SERIAL_OUTPUT_PORTS) };
        for port in 0..MAX_SERIAL_OPENS_IN_FLIGHT {
            outputs
                .sender(&format!("wedged-{port}"), 115_200, due, b"x()")
                .expect("below the cap");
            driver.wait_entered();
        }
        outputs.begin_set();
        let calls = driver.calls();
        let refused = match outputs.sender("one-too-many", 115_200, due, b"x()") {
            Err(message) => message,
            Ok(_) => panic!("a port past the cap was opened"),
        };
        assert!(
            refused.contains("still waiting on their drivers"),
            "{refused}"
        );
        assert!(refused.contains("until rustel restarts"), "{refused}");
        let route = outputs
            .sender("one-too-many", 115_200, due, b"x()")
            .expect("a remembered refusal is not an error");
        assert!(route.sender.is_none());
        assert!(!route.buffered, "a refused port held a write");
        assert_eq!(driver.calls(), calls, "a thread was started past the cap");
        assert_eq!(outputs.in_flight.len(), MAX_SERIAL_OPENS_IN_FLIGHT);

        driver.release(Err("the driver answered at last".into()));
        poll_until(&mut outputs, |outputs| {
            outputs.in_flight.len() < MAX_SERIAL_OPENS_IN_FLIGHT
        });
        outputs.begin_set();
        let route = outputs
            .sender("one-too-many", 115_200, due, b"x()")
            .expect("a place is free again");
        assert!(route.buffered, "the freed place was not used");
        assert_eq!(driver.wait_entered(), "one-too-many");
        assert_eq!(driver.calls(), calls + 1);
    }

    /// An opener that panics still gives its place back, as a failure: the
    /// cap would otherwise lose that place for good.
    #[test]
    fn a_panicking_open_gives_its_place_back() {
        let mut outputs = SerialOutputs::with_opener(std::sync::Arc::new(
            |_port: &str, _baud: u32| -> Result<SerialSender, String> {
                panic!("the stand-in driver fell over")
            },
        ));
        outputs
            .sender("fragile", 115_200, std::time::Instant::now(), b"x()")
            .expect("admission");
        let news = poll_until_reported(&mut outputs);
        assert_eq!(news.len(), 1, "{news:?}");
        assert_eq!(news[0].kind, SerialNewsKind::Failure);
        assert!(news[0].message.contains("panicked"), "{}", news[0].message);
        assert!(
            outputs.in_flight.is_empty(),
            "the place was never given back"
        );
    }

    /// A port stuck opening is told once a set, as news rather than a failure,
    /// so a wedged driver is never silent - and a set that adopts the same
    /// stuck open is told afresh.
    #[test]
    fn a_port_still_opening_is_told_once_per_set() {
        let (mut outputs, driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_secs(30);
        start_opening(&mut outputs, &driver, "stuck", 115_200, due, b"x()");
        // The open's age is set by hand rather than slept out, both ways, so a
        // slow runner cannot move it across the notice.
        let age = |outputs: &mut SerialOutputs, age: std::time::Duration| {
            outputs.in_flight.get_mut("stuck").expect("in flight").since =
                std::time::Instant::now()
                    .checked_sub(age)
                    .expect("a clock this close to its epoch");
        };
        age(&mut outputs, std::time::Duration::ZERO);
        assert!(outputs.poll().is_empty(), "told before the notice is due");
        age(
            &mut outputs,
            SERIAL_OPEN_STALL_NOTICE + std::time::Duration::from_secs(1),
        );
        let news = outputs.poll();
        assert_eq!(news.len(), 1, "{news:?}");
        assert_eq!(news[0].kind, SerialNewsKind::StillOpening);
        assert!(
            news[0].message.contains("\"stuck\" is still opening after"),
            "{}",
            news[0].message
        );
        assert!(outputs.poll().is_empty(), "the stalled open was told twice");

        outputs.begin_set();
        outputs
            .sender("stuck", 115_200, due, b"x()")
            .expect("the next set names the port again");
        let news = outputs.poll();
        assert_eq!(news.len(), 1, "the next set was not told: {news:?}");
        assert_eq!(news[0].kind, SerialNewsKind::StillOpening);
        assert_eq!(driver.calls(), 1, "the next set opened the port again");
    }

    /// A set that restarts while a port is still opening adopts that open: it
    /// cannot be called back, an exclusive COM port refuses a second open while
    /// it is pending, and a wedged adapter would strand one more thread per
    /// restart. What the old set held goes with it; what the new set strikes
    /// flushes to the adopted open.
    #[test]
    fn a_new_set_adopts_an_open_still_in_flight_instead_of_opening_again() {
        let (mut outputs, driver) = gated_outputs();
        // Both sets' writes due soon and together, so a held write of the old
        // set that survived would reach the port beside the new one.
        let due = std::time::Instant::now() + std::time::Duration::from_millis(300);
        start_opening(&mut outputs, &driver, "COM-SLOW", 9_600, due, b"old()");

        outputs.begin_set();
        let route = outputs
            .sender("COM-SLOW", 9_600, due, b"new()")
            .expect("the new set's admission");
        assert!(
            route.buffered,
            "the new set did not wait for the adopted open"
        );
        assert!(route.note.is_none(), "{:?}", route.note);
        assert_eq!(driver.calls(), 1, "the new set opened the port again");

        driver.release(Ok(()));
        poll_until_opened(&mut outputs, "COM-SLOW");
        let writes = driver.settled_writes(&outputs, 1);
        assert!(
            writes.iter().all(|bytes| bytes == b"new()"),
            "the old set's held write outlived its set: {writes:?}"
        );
        assert_eq!(driver.calls(), 1);
    }

    /// A restart applies the baud the new set asks for, even while the last
    /// set's open is still running at the old one: the open is adopted, not
    /// asked for twice, and once it finishes the port is closed and opened
    /// again at the new baud. The set is told why its writes wait.
    #[test]
    fn an_adopted_open_at_another_baud_is_opened_again_once_it_finishes() {
        let (mut outputs, driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_millis(300);
        start_opening(&mut outputs, &driver, "COM-SLOW", 9_600, due, b"old()");

        outputs.begin_set();
        let route = outputs
            .sender("COM-SLOW", 115_200, due, b"new()")
            .expect("the new set's admission");
        assert!(route.buffered);
        let note = route.note.expect("the set is told why its writes wait");
        assert!(
            note.contains("still opening at 9600 baud for the last set"),
            "{note}"
        );
        assert!(note.contains("opens again at 115200"), "{note}");
        assert_eq!(driver.calls(), 1, "asked for the port while it was pending");

        driver.release(Ok(()));
        poll_until(&mut outputs, |outputs| {
            outputs
                .in_flight
                .get("COM-SLOW")
                .is_some_and(|flight| flight.baud == 115_200)
        });
        assert_eq!(driver.wait_entered(), "COM-SLOW");
        assert_eq!(driver.calls(), 2);
        assert_eq!(
            driver.held_on_entry(),
            [0, 0],
            "the port was asked for again before the old handle let it go"
        );
        driver.release(Ok(()));
        poll_until_opened(&mut outputs, "COM-SLOW");
        assert_eq!(
            driver.bauds(),
            [9_600, 115_200],
            "the port was not opened again at the set's baud"
        );
        let writes = driver.settled_writes(&outputs, 1);
        assert!(
            writes.iter().all(|bytes| bytes == b"new()"),
            "the held write did not wait for the new open: {writes:?}"
        );
    }

    /// When an adopted open fails, the failure belongs to the last set. This
    /// set opens the port again once and keeps its writes held.
    #[test]
    fn an_adopted_open_that_fails_is_tried_again_for_the_new_set() {
        let (mut outputs, driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_millis(300);
        start_opening(&mut outputs, &driver, "COM-BT", 115_200, due, b"old()");
        outputs.begin_set();
        let route = outputs
            .sender("COM-BT", 115_200, due, b"new()")
            .expect("the new set's admission");
        assert!(route.buffered);

        driver.release(Err(
            "could not open serial port \"COM-BT\": semaphore timeout".into(),
        ));
        poll_until_reopened_by_this_set(&mut outputs, "COM-BT");
        let news = outputs.poll();
        assert!(
            news.iter()
                .all(|notice| notice.kind != SerialNewsKind::Failure),
            "the last set's failure was kept: {news:?}"
        );
        assert_eq!(driver.wait_entered(), "COM-BT");
        assert_eq!(driver.calls(), 2);
        driver.release(Ok(()));
        poll_until_opened(&mut outputs, "COM-BT");
        let writes = driver.settled_writes(&outputs, 1);
        assert!(
            writes.iter().all(|bytes| bytes == b"new()"),
            "the held write did not carry over to the new try: {writes:?}"
        );
    }

    /// Only an open this set started is this set's failure: when its own try
    /// fails too, that is told once and remembered like any other.
    #[test]
    fn a_failed_try_of_the_set_s_own_is_remembered() {
        let (mut outputs, driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_secs(30);
        start_opening(&mut outputs, &driver, "COM-BT", 115_200, due, b"x()");
        outputs.begin_set();
        outputs
            .sender("COM-BT", 115_200, due, b"x()")
            .expect("the new set's admission");
        driver.release(Err("the last set's try failed".into()));
        poll_until_reopened_by_this_set(&mut outputs, "COM-BT");
        assert_eq!(driver.wait_entered(), "COM-BT");
        driver.release(Err("could not open serial port \"COM-BT\": gone".into()));
        let news = poll_until_reported(&mut outputs);
        assert_eq!(news.len(), 1, "{news:?}");
        assert_eq!(news[0].kind, SerialNewsKind::Failure);
        assert!(news[0].message.contains("gone"), "{}", news[0].message);
        let route = outputs
            .sender("COM-BT", 115_200, due, b"x()")
            .expect("a remembered failure is not an error");
        assert!(route.sender.is_none());
        assert!(!route.buffered);
        assert_eq!(driver.calls(), 2, "the set's own failure was retried");
    }

    /// An open that completes between sets is the next set's to adopt, not a
    /// reason to open the port again; one that failed is forgotten, so the
    /// port is tried afresh when a set names it.
    #[test]
    fn an_open_finishing_between_sets_is_adopted_or_forgotten() {
        let (mut outputs, driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_secs(30);
        start_opening(&mut outputs, &driver, "COM-SLOW", 9_600, due, b"old()");
        outputs.begin_set();
        driver.release(Ok(()));
        poll_until(&mut outputs, |outputs| {
            outputs.adoptable.contains_key("COM-SLOW")
        });
        let route = outputs
            .sender("COM-SLOW", 9_600, due, b"new()")
            .expect("the new set's admission");
        assert!(route.sender.is_some(), "the finished open was not adopted");
        assert!(!route.buffered);
        assert_eq!(driver.calls(), 1, "the new set opened the port again");

        start_opening(&mut outputs, &driver, "COM-GONE", 9_600, due, b"x()");
        outputs.begin_set();
        driver.release(Err("could not open serial port \"COM-GONE\": gone".into()));
        poll_until(&mut outputs, |outputs| outputs.in_flight.is_empty());
        assert!(
            outputs.poll().is_empty(),
            "a set that never named the port was told of its failure"
        );
        let route = outputs
            .sender("COM-GONE", 9_600, due, b"x()")
            .expect("the port is asked for afresh");
        assert!(route.buffered, "an earlier set's failure was kept");
        assert_eq!(driver.wait_entered(), "COM-GONE");
        assert_eq!(driver.calls(), 3);
    }

    /// An open that finished between sets at another baud than the new set asks
    /// for is not adopted: the restart applies the new baud, closing the old
    /// handle before the port is asked for again.
    #[test]
    fn an_open_finished_at_another_baud_is_opened_again_at_the_set_s() {
        let (mut outputs, driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_millis(300);
        start_opening(&mut outputs, &driver, "COM-SLOW", 9_600, due, b"old()");
        outputs.begin_set();
        driver.release(Ok(()));
        poll_until(&mut outputs, |outputs| {
            outputs.adoptable.contains_key("COM-SLOW")
        });
        let route = outputs
            .sender("COM-SLOW", 115_200, due, b"new()")
            .expect("the new set's admission");
        assert!(route.buffered, "the old baud's open was adopted");
        assert!(route.note.is_none(), "{:?}", route.note);
        assert_eq!(driver.wait_entered(), "COM-SLOW");
        assert_eq!(
            driver.held_on_entry(),
            [0, 0],
            "the port was asked for again before the old handle let it go"
        );
        driver.release(Ok(()));
        poll_until_opened(&mut outputs, "COM-SLOW");
        assert_eq!(
            driver.bauds(),
            [9_600, 115_200],
            "the port was not opened again at the set's baud"
        );
        assert_eq!(driver.calls(), 2);
    }

    /// A restart keeps a port the last set had open and takes it as it is at
    /// the same baud: no second open, nothing the last set queued goes out in
    /// the new one, and the new set counts its writes and troubles from zero.
    #[test]
    fn a_restart_takes_a_port_the_last_set_had_open_as_it_is() {
        let (mut outputs, driver) = gated_outputs();
        let started = std::time::Instant::now();
        let far = started + std::time::Duration::from_secs(30);
        open_in_first_set(&mut outputs, &driver, "COM-OPEN", 9_600, far, b"");
        let sender = outputs
            .sender("COM-OPEN", 9_600, far, b"")
            .expect("the open port routes")
            .sender
            .expect("the port is open");
        // The last set leaves a write queued for later, and one it was already
        // too late for, which the sender counts as dropped late.
        let later = started + std::time::Duration::from_millis(700);
        assert!(sender.send_at(later, b"old-later()".to_vec()));
        assert!(
            sender.send_at(
                started
                    .checked_sub(std::time::Duration::from_secs(2))
                    .expect("a clock this close to its epoch"),
                b"old-stale()".to_vec()
            )
        );
        poll_until(&mut outputs, |outputs| outputs.report().dropped_late == 1);

        outputs.begin_set();
        let route = outputs
            .sender("COM-OPEN", 9_600, far, b"")
            .expect("the new set's admission");
        assert!(!route.buffered, "the open port was not taken as it is");
        assert!(route.note.is_none(), "{:?}", route.note);
        assert!(
            route.sender.is_some(),
            "the open port was not taken as it is"
        );
        // What the sender counted for the last set is not this set's news.
        let counted = outputs.report();
        assert_eq!(
            (counted.written, counted.dropped_late),
            (0, 0),
            "the last set's counts were carried into the new one: {counted:?}"
        );
        assert!(
            outputs.poll().is_empty(),
            "the last set's trouble was told to the new one"
        );
        let sender = outputs
            .sender("COM-OPEN", 9_600, far, b"")
            .expect("the taken port routes")
            .sender
            .expect("the port is open");
        assert!(sender.send_at(
            later + std::time::Duration::from_millis(100),
            b"new()".to_vec()
        ));
        assert_eq!(driver.calls(), 1, "the port was opened a second time");
        let writes = driver.settled_writes(&outputs, 1);
        assert!(
            !writes.iter().any(|bytes| bytes == b"old-later()"),
            "a write the last set queued went out in the new one: {writes:?}"
        );
        let news = outputs.poll();
        assert_eq!(
            news.len(),
            usize::from(outputs.report().dropped_late > 0),
            "only the new write can be news: {news:?}"
        );
    }

    /// At another baud, the port the last set had open is closed before it is
    /// asked for again: an exclusive port refuses a new open while the old
    /// handle still holds it.
    #[test]
    fn a_restart_at_another_baud_closes_the_open_port_before_opening_it_again() {
        let (mut outputs, driver) = gated_outputs();
        let far = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let due = std::time::Instant::now() + std::time::Duration::from_millis(300);
        open_in_first_set(&mut outputs, &driver, "COM-OPEN", 9_600, far, b"old()");

        outputs.begin_set();
        let route = outputs
            .sender("COM-OPEN", 115_200, due, b"new()")
            .expect("the new set's admission");
        assert!(route.buffered, "the old baud's port was taken as it is");
        assert_eq!(driver.wait_entered(), "COM-OPEN");
        assert_eq!(
            driver.held_on_entry(),
            [0, 0],
            "the port was asked for again before the old handle let it go"
        );
        assert_eq!(driver.bauds(), [9_600, 115_200]);
        driver.release(Ok(()));
        poll_until_opened(&mut outputs, "COM-OPEN");
        let writes = driver.settled_writes(&outputs, 1);
        assert!(writes.iter().all(|bytes| bytes == b"new()"), "{writes:?}");
    }

    /// A port that has rejected writes may belong to an adapter unplugged since,
    /// and one plugged back in answers only a fresh open: a restart closes it
    /// and opens the port again, even at the same baud.
    #[test]
    fn a_restart_opens_afresh_a_port_that_rejected_writes() {
        struct Unplugged;
        impl rustel_serial::SerialPort for Unplugged {
            fn write(&mut self, _bytes: &[u8]) -> Result<(), String> {
                Err("the device is gone".into())
            }
        }
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&calls);
        let mut outputs = SerialOutputs::with_opener(std::sync::Arc::new(move |port, _baud| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(SerialSender::with_port(
                Box::new(Unplugged),
                port.to_string(),
            ))
        }));
        let now = std::time::Instant::now();
        outputs
            .sender("COM-GONE", 9_600, now, b"")
            .expect("the first set's admission");
        poll_until_opened(&mut outputs, "COM-GONE");
        let sender = outputs
            .sender("COM-GONE", 9_600, now, b"")
            .expect("the open port routes")
            .sender
            .expect("the port is open");
        assert!(sender.send_at(std::time::Instant::now(), b"x()".to_vec()));
        poll_until(&mut outputs, |outputs| outputs.report().write_errors > 0);

        outputs.begin_set();
        let route = outputs
            .sender("COM-GONE", 9_600, now, b"")
            .expect("the new set's admission");
        assert!(route.buffered, "a port that rejected writes was kept");
        poll_until_opened(&mut outputs, "COM-GONE");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// The old handle is closed on the new open's thread, not the producer's.
    /// With every old port held, `sender` and `poll` still return at once, for
    /// a port the last set had open and for one whose adopted open finished
    /// at another baud. No port is opened again until its old handle is
    /// released.
    #[test]
    fn an_old_handle_is_closed_off_the_producer() {
        let (mut outputs, driver) = gated_outputs();
        let due = std::time::Instant::now() + std::time::Duration::from_secs(30);
        open_in_first_set(&mut outputs, &driver, "COM-OPEN", 9_600, due, b"x()");
        start_opening(&mut outputs, &driver, "COM-OPENING", 9_600, due, b"x()");

        driver.shut_drops(true);
        outputs.begin_set();
        // A port held for ten seconds is unmistakable next to this bound.
        let bound = std::time::Duration::from_secs(5);
        let asked = std::time::Instant::now();
        let route = outputs
            .sender("COM-OPEN", 115_200, due, b"y()")
            .expect("the new set's admission");
        assert!(route.buffered);
        assert!(
            asked.elapsed() < bound,
            "the producer waited {:?} for an old handle",
            asked.elapsed()
        );
        outputs
            .sender("COM-OPENING", 115_200, due, b"y()")
            .expect("the new set's admission");
        driver.release(Ok(()));
        let reopened = wait_until(|| {
            let asked = std::time::Instant::now();
            outputs.poll();
            assert!(
                asked.elapsed() < bound,
                "the producer waited {:?} for an old handle",
                asked.elapsed()
            );
            outputs
                .in_flight
                .get("COM-OPENING")
                .is_some_and(|flight| flight.baud == 115_200)
        });
        assert!(reopened, "the finished open was never opened again");
        // Both new opens have started and wait for their old handles.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            driver.calls(),
            2,
            "a port was asked for again while its old handle still held it"
        );
        driver.shut_drops(false);
        let mut reopened = [driver.wait_entered(), driver.wait_entered()];
        reopened.sort();
        assert_eq!(reopened, ["COM-OPEN", "COM-OPENING"]);
    }

    /// Wait up to five seconds for `done`, looking every millisecond, and say
    /// whether it came true: the one deadline every wait in these tests uses.
    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Outputs opening their ports through a fresh [`GatedOpener`].
    fn gated_outputs() -> (SerialOutputs, GatedOpener) {
        let (opener, driver) = GatedOpener::new();
        (SerialOutputs::with_opener(opener), driver)
    }

    /// Name `port` with one write, and wait until its open is inside the
    /// stand-in driver.
    fn start_opening(
        outputs: &mut SerialOutputs,
        driver: &GatedOpener,
        port: &str,
        baud: u32,
        due: Instant,
        bytes: &[u8],
    ) {
        outputs
            .sender(port, baud, due, bytes)
            .expect("admission while the port opens");
        assert_eq!(driver.wait_entered(), port);
    }

    /// The same, then let the open succeed and install, as the first set of a
    /// restart test has it.
    fn open_in_first_set(
        outputs: &mut SerialOutputs,
        driver: &GatedOpener,
        port: &str,
        baud: u32,
        due: Instant,
        bytes: &[u8],
    ) {
        start_opening(outputs, driver, port, baud, due, bytes);
        driver.release(Ok(()));
        poll_until_opened(outputs, port);
    }

    /// `poll` reports an open failure only once it has actually completed, so
    /// this helper waits out the open thread's round trip.
    fn poll_until_reported(outputs: &mut SerialOutputs) -> Vec<SerialNews> {
        let mut news = Vec::new();
        let reported = wait_until(|| {
            news = outputs.poll();
            !news.is_empty()
        });
        assert!(reported, "the open thread never reported the failure");
        news
    }

    fn poll_until(outputs: &mut SerialOutputs, done: impl Fn(&SerialOutputs) -> bool) {
        let reported = wait_until(|| {
            outputs.apply_open_results();
            done(outputs)
        });
        assert!(reported, "the open thread never reported");
    }

    /// Wait until this set has started an open of its own for `port`: a retry
    /// of an adopted open that failed, or a reopen at the set's baud.
    fn poll_until_reopened_by_this_set(outputs: &mut SerialOutputs, port: &str) {
        poll_until(outputs, |outputs| {
            outputs
                .in_flight
                .get(port)
                .is_some_and(|flight| flight.set == outputs.set)
        });
    }

    fn poll_until_opened(outputs: &mut SerialOutputs, port: &str) {
        poll_until(outputs, |outputs| {
            outputs
                .this_set
                .open
                .iter()
                .any(|(name, _, sender)| name == port && sender.is_some())
        });
    }

    #[test]
    fn port_name_and_port_count_caps_are_enforced() {
        let mut outputs = SerialOutputs::new();
        let due = std::time::Instant::now();
        match outputs.sender(
            &"p".repeat(MAX_SERIAL_PORT_NAME_BYTES + 1),
            115_200,
            due,
            b"x()",
        ) {
            Err(message) => assert!(message.contains("at most"), "{message}"),
            Ok(_) => panic!("oversized port name must be refused"),
        }
        for index in 0..MAX_SERIAL_OUTPUT_PORTS {
            outputs
                .this_set
                .open
                .push((format!("/dev/port-{index}"), 115_200, None));
        }
        match outputs.sender("/dev/overflow", 115_200, due, b"x()") {
            Err(message) => assert!(message.contains("at most"), "{message}"),
            Ok(_) => panic!("port count cap must refuse"),
        }
    }

    /// A live edit of `serialbaud` on an open port is not an error: the port
    /// reuses the open handle, reports the mismatch once, and keeps writing at
    /// the open baud.
    #[test]
    fn a_baud_mismatch_keeps_writing_and_notes_once() {
        let mut outputs = SerialOutputs::new();
        let capture = rustel_serial::CapturePort::new();
        outputs.this_set.open.push((
            "COM-TEST".to_string(),
            115_200,
            Some(SerialSender::with_port(
                Box::new(capture.clone()),
                "COM-TEST".into(),
            )),
        ));

        let route = outputs
            .sender("COM-TEST", 9_600, std::time::Instant::now(), b"")
            .expect("a mismatch is a note, not a refusal");
        let note = route.note.expect("the mismatch is reported");
        assert!(note.contains("already open at 115200 baud"), "{note}");
        assert!(note.contains("serialbaud 9600 was ignored"), "{note}");
        let sender = route.sender.expect("the open handle still routes writes");
        // The route is the port already open, so the write goes out at its baud.
        assert_eq!(sender.port_name(), "COM-TEST");
        assert!(sender.send_at(std::time::Instant::now(), b"hit(drum:1)".to_vec()));

        // The write reaches the port's queue even though the baud could not
        // change. How the worker then delivers it is rustel-serial's to test: a
        // runner stalled past the late-drop window drops it late rather than
        // writing it, which is not the bridge refusing it.
        let settled = |report: rustel_serial::SerialReport| report.written + report.dropped_late;
        wait_until(|| settled(sender.report()) > 0);
        let report = sender.report();
        assert_eq!(
            settled(report),
            1,
            "the mismatched onset's write never reached the port"
        );
        if report.written == 1 {
            assert_eq!(capture.writes()[0].1, b"hit(drum:1)");
        }

        // Once per requested baud, not once per onset.
        let route = outputs
            .sender("COM-TEST", 9_600, std::time::Instant::now(), b"")
            .unwrap();
        assert!(route.note.is_none(), "the note repeated");
        assert!(route.sender.is_some());
        // The open baud itself is never a mismatch.
        let route = outputs
            .sender("COM-TEST", 115_200, std::time::Instant::now(), b"")
            .unwrap();
        assert!(route.note.is_none());
        assert!(route.sender.is_some());
        // A second distinct wrong baud gets its own single note.
        let route = outputs
            .sender("COM-TEST", 4_800, std::time::Instant::now(), b"")
            .unwrap();
        assert!(route.note.expect("a new baud is news").contains("4800"));
        assert!(route.sender.is_some());
    }

    /// A score cycling unique baud values cannot grow the note bookkeeping
    /// without bound; past the cap the writes keep flowing and the notes stop.
    /// The cap is per port: one port cycling bauds cannot silence another
    /// port's first mismatch.
    #[test]
    fn baud_mismatch_notes_are_capped_but_writes_are_not() {
        let mut outputs = SerialOutputs::new();
        let capture = rustel_serial::CapturePort::new();
        outputs.this_set.open.push((
            "COM-CAP".to_string(),
            115_200,
            Some(SerialSender::with_port(
                Box::new(capture.clone()),
                "COM-CAP".into(),
            )),
        ));
        outputs.this_set.open.push((
            "COM-OTHER".to_string(),
            115_200,
            Some(SerialSender::with_port(
                Box::new(rustel_serial::CapturePort::new()),
                "COM-OTHER".into(),
            )),
        ));
        for offset in 0..MAX_SERIAL_BAUD_NOTES_PER_PORT {
            let route = outputs
                .sender(
                    "COM-CAP",
                    (offset + 1) as u32,
                    std::time::Instant::now(),
                    b"",
                )
                .expect("a mismatch is never a refusal");
            assert!(route.note.is_some(), "note {offset} was swallowed early");
            assert!(route.sender.is_some());
        }
        let route = outputs
            .sender("COM-CAP", 999_999, std::time::Instant::now(), b"")
            .unwrap();
        assert!(route.note.is_none(), "the cap did not stop the notes");
        let sender = route.sender.expect("the cap must not stop the writes");
        assert!(sender.send_at(std::time::Instant::now(), b"go()".to_vec()));
        // The other port's first mismatch is still news.
        let route = outputs
            .sender("COM-OTHER", 9_600, std::time::Instant::now(), b"")
            .unwrap();
        assert!(route.note.is_some(), "one port's notes used up another's");
    }
}
