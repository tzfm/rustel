/*
midi_input.rs - Attach platform MIDI inputs to the score's input bus
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Opening the inputs a score named, and feeding what they send into the
//! lock-free tables in `rustel_core::midi_in`.
//!
//! The twin of [`crate::midi_bridge::MidiOutputs`], with the same contract: a
//! device that is not there is reported ONCE and then ignored, and nothing here
//! may end a set.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rustel_core::midi_in::{InputBus, InputPort, MAX_INPUT_PORTS};
use rustel_midi::input::{MidiEvent, MidiListener};

/// Route one decoded message into the port's tables.
///
/// Dispatch by decoded message type so note numbers and velocities cannot
/// overwrite controller values, even though both messages use two data bytes.
pub fn dispatch(port: &InputPort, now_nanos: u64, event: MidiEvent) {
    match event {
        MidiEvent::ControlChange {
            channel,
            controller,
            value,
        } => port.observe_control_change(channel, controller, value),
        // Velocity zero is a note-off on the wire, which most keyboards use for
        // release. Recording it as a note would play a silent note on every key
        // the player lets go of.
        MidiEvent::NoteOn { velocity: 0, .. } => {}
        MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        } => port.observe_note_on(now_nanos, channel, note, velocity),
        _ => {}
    }
}

const MAX_RETAINED_INPUTS: usize = MAX_INPUT_PORTS * 2;
const MAX_PENDING_INPUT_REPORTS: usize = MAX_RETAINED_INPUTS;
const MAX_INPUT_REPORT_BYTES: usize = 2_048;
const INPUT_RETRY_BASE: Duration = Duration::from_millis(250);
const INPUT_MANAGER_IDLE_POLL: Duration = Duration::from_millis(250);

/// Type-erased listener ownership. Destruction is deliberately worker-owned:
/// on Linux, dropping a midir connection can join its ALSA reader thread.
pub trait ManagedListener: Send {}

impl<T: Send> ManagedListener for T {}

/// How [`MidiInputs`] opens a port: the platform's MIDI stack, or a test's
/// stand-in.
pub trait InputOpener: Send + Sync + 'static {
    fn open(&self, port: Arc<InputPort>) -> Result<Box<dyn ManagedListener>, String>;
}

/// Report the ports a manager was asked to open, without opening any.
/// For a test that only cares that the studio asked at all.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub struct RecordingOpener {
    pub asked: std::sync::Mutex<Vec<String>>,
}

#[cfg(any(test, feature = "test-support"))]
impl InputOpener for RecordingOpener {
    fn open(&self, port: Arc<InputPort>) -> Result<Box<dyn ManagedListener>, String> {
        self.asked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(port.selector.clone());
        Ok(Box::new(()))
    }
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn quiet_inputs(opener: Arc<dyn InputOpener>) -> MidiInputs {
    MidiInputs::with_opener(
        opener,
        ManagerConfig {
            retry_base: Duration::from_millis(5),
            idle_poll: Duration::from_millis(5),
        },
    )
}

struct PlatformInputOpener;

impl InputOpener for PlatformInputOpener {
    fn open(&self, port: Arc<InputPort>) -> Result<Box<dyn ManagedListener>, String> {
        // The callback captures its own strong reference. A driver may finish
        // a callback while the worker retires the connection.
        let selector = port.selector.clone();
        let sink = Arc::clone(&port);
        MidiListener::open_filtered(
            Some(&selector),
            // Sysex, clock and active sensing never reach a score.
            midir::Ignore::All,
            move |_micros, event, _bytes| {
                dispatch(&sink, rustel_core::midi_in::now_nanos(), event);
            },
        )
        .map(|listener| Box::new(listener) as Box<dyn ManagedListener>)
    }
}

/// The worker's ownership record for one exact `Arc<InputPort>`.
struct OpenInput {
    listener: Option<Box<dyn ManagedListener>>,
    retry_at: Option<Instant>,
    port: Arc<InputPort>,
    attempts: u64,
    failure_reported: bool,
}

struct ManagerState {
    revision: u64,
    desired: Vec<Arc<InputPort>>,
    desired_limit_reported: bool,
    reports: VecDeque<String>,
    reports_dropped: u64,
    reports_dropped_pending: u64,
}

impl Default for ManagerState {
    fn default() -> Self {
        Self {
            revision: 0,
            desired: Vec::new(),
            desired_limit_reported: false,
            reports: VecDeque::with_capacity(MAX_PENDING_INPUT_REPORTS),
            reports_dropped: 0,
            reports_dropped_pending: 0,
        }
    }
}

struct ManagerShared {
    state: Mutex<ManagerState>,
    wake: Condvar,
    stopping: AtomicBool,
    connected: AtomicUsize,
    applied_revision: AtomicU64,
    worker_alive: AtomicBool,
}

impl Default for ManagerShared {
    fn default() -> Self {
        Self {
            state: Mutex::new(ManagerState::default()),
            wake: Condvar::new(),
            stopping: AtomicBool::new(false),
            connected: AtomicUsize::new(0),
            applied_revision: AtomicU64::new(0),
            worker_alive: AtomicBool::new(false),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ManagerConfig {
    retry_base: Duration,
    idle_poll: Duration,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            retry_base: INPUT_RETRY_BASE,
            idle_poll: INPUT_MANAGER_IDLE_POLL,
        }
    }
}

/// Nonblocking observation of the input manager.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MidiInputStatus {
    pub desired: usize,
    pub connected: usize,
    pub published_revision: u64,
    pub applied_revision: u64,
    pub pending_reports: usize,
    pub reports_dropped: u64,
    pub worker_alive: bool,
}

/// A bounded asynchronous manager for score-selected MIDI inputs.
///
/// The producer owns only this mailbox handle. The prestarted worker owns all
/// platform connections and is the only thread that may open or drop them.
pub struct MidiInputs {
    shared: Arc<ManagerShared>,
    /// Dropping a `JoinHandle` detaches it. We intentionally never join here:
    /// a platform open or listener destructor may be wedged in a driver.
    worker: Option<std::thread::JoinHandle<()>>,
}

impl MidiInputs {
    pub fn new() -> Self {
        Self::with_opener(Arc::new(PlatformInputOpener), ManagerConfig::default())
    }

    pub(crate) fn with_opener(opener: Arc<dyn InputOpener>, config: ManagerConfig) -> Self {
        let shared = Arc::new(ManagerShared::default());
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("rustel-midi-in".into())
            .spawn(move || run_input_manager(worker_shared, opener, config));
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                push_report(
                    &mut state,
                    format!("could not start MIDI input manager: {error}"),
                );
                None
            }
        };
        Self { shared, worker }
    }

    /// Publish the exact audible/candidate input union to the worker.
    ///
    /// This producer-side method performs no platform enumeration, open,
    /// connection drop, or thread join. Repeated calls coalesce into one latest
    /// desired snapshot; the worker rechecks that revision after every slow
    /// open and discards a result whose exact port is no longer desired.
    ///
    /// Returns bounded asynchronous reports accumulated since the previous
    /// call, for the live loop to render in its own `live_error` shape.
    pub fn sync(
        &mut self,
        bus: &InputBus,
        audible_generation: u64,
        session_generation: u64,
    ) -> Vec<String> {
        self.sync_allowed(bus, audible_generation, session_generation, |_| true)
    }

    /// [`Self::sync`], opening only the ports whose selector `allowed` admits.
    ///
    /// A port the score asks for and `allowed` refuses is simply not wanted:
    /// it is left out of what the manager is told to hold open, so an input
    /// already open for it is closed on the next pass the same way a port a
    /// score stops asking for is. The command line has no gate and passes
    /// everything; the studio passes the ports a player has ticked.
    pub fn sync_allowed(
        &mut self,
        bus: &InputBus,
        audible_generation: u64,
        session_generation: u64,
        allowed: impl Fn(&str) -> bool,
    ) -> Vec<String> {
        let mut desired = bus.snapshot_for(audible_generation, session_generation);
        desired.retain(|port| allowed(&port.selector));
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if desired.len() > MAX_RETAINED_INPUTS {
            desired.truncate(MAX_RETAINED_INPUTS);
            if !state.desired_limit_reported {
                push_report(
                    &mut state,
                    format!(
                        "at most {MAX_RETAINED_INPUTS} MIDI inputs may be retained across a live cutover"
                    ),
                );
                state.desired_limit_reported = true;
            }
        } else {
            state.desired_limit_reported = false;
        }
        if !same_ports(&state.desired, &desired) {
            state.desired = desired;
            state.revision = state.revision.wrapping_add(1);
            self.shared.wake.notify_one();
        }
        take_reports(&mut state)
    }

    /// How many inputs are actually connected.
    pub fn connected(&self) -> usize {
        self.shared.connected.load(Ordering::Acquire)
    }

    pub fn status(&self) -> MidiInputStatus {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        MidiInputStatus {
            desired: state.desired.len(),
            connected: self.connected(),
            published_revision: state.revision,
            applied_revision: self.shared.applied_revision.load(Ordering::Acquire),
            pending_reports: state.reports.len(),
            reports_dropped: state.reports_dropped,
            worker_alive: self.shared.worker_alive.load(Ordering::Acquire),
        }
    }
}

impl Default for MidiInputs {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MidiInputs {
    fn drop(&mut self) {
        // No manager mutex and no join: this path stays bounded even if the
        // worker is inside an uninterruptible platform open/drop. The idle
        // wait has a finite poll as a backstop against a notify-before-wait
        // race on this atomic stop flag.
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        let _detached = self.worker.take();
    }
}

fn same_ports(left: &[Arc<InputPort>], right: &[Arc<InputPort>]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| Arc::ptr_eq(left, right))
}

fn truncate_report(mut message: String) -> String {
    if message.len() <= MAX_INPUT_REPORT_BYTES {
        return message;
    }
    const SUFFIX: &str = "…";
    let mut end = MAX_INPUT_REPORT_BYTES.saturating_sub(SUFFIX.len());
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push_str(SUFFIX);
    message
}

fn push_report(state: &mut ManagerState, message: String) {
    if state.reports.len() == MAX_PENDING_INPUT_REPORTS {
        state.reports_dropped = state.reports_dropped.saturating_add(1);
        state.reports_dropped_pending = state.reports_dropped_pending.saturating_add(1);
        return;
    }
    state.reports.push_back(truncate_report(message));
}

fn take_reports(state: &mut ManagerState) -> Vec<String> {
    let mut reports = state.reports.drain(..).collect::<Vec<_>>();
    let dropped = std::mem::take(&mut state.reports_dropped_pending);
    if dropped != 0 {
        reports.push(format!(
            "{dropped} additional MIDI input report(s) were dropped by the bounded manager"
        ));
    }
    reports
}

fn retry_delay(attempts: u64, base: Duration) -> Duration {
    let shift = attempts.saturating_sub(1).min(5) as u32;
    base.saturating_mul(1_u32 << shift)
}

fn desired_contains(desired: &[Arc<InputPort>], port: &Arc<InputPort>) -> bool {
    desired.iter().any(|wanted| Arc::ptr_eq(wanted, port))
}

fn run_input_manager(
    shared: Arc<ManagerShared>,
    opener: Arc<dyn InputOpener>,
    config: ManagerConfig,
) {
    shared.worker_alive.store(true, Ordering::Release);
    let mut open = Vec::<OpenInput>::with_capacity(MAX_RETAINED_INPUTS);

    while !shared.stopping.load(Ordering::Acquire) {
        let (revision, desired) = {
            let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.revision, state.desired.clone())
        };

        // Publish the disconnected count before potentially blocking in a
        // listener destructor. The destructor stays entirely on this worker.
        let mut retired = Vec::new();
        let mut retained = Vec::with_capacity(open.len());
        for entry in open.drain(..) {
            if desired_contains(&desired, &entry.port) {
                retained.push(entry);
            } else {
                retired.push(entry);
            }
        }
        open = retained;
        shared.connected.store(
            open.iter().filter(|entry| entry.listener.is_some()).count(),
            Ordering::Release,
        );
        drop(retired);

        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        // A listener destructor above may have blocked while the producer
        // published several newer revisions. Never start opening the stale
        // snapshot after that delay; loop once and take only the newest union.
        if shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revision
            != revision
        {
            continue;
        }

        for port in desired.iter().take(MAX_RETAINED_INPUTS) {
            if !open.iter().any(|entry| Arc::ptr_eq(&entry.port, port)) {
                open.push(OpenInput {
                    listener: None,
                    retry_at: None,
                    port: Arc::clone(port),
                    attempts: 0,
                    failure_reported: false,
                });
            }
        }
        shared.applied_revision.store(revision, Ordering::Release);

        let now = Instant::now();
        let attempt = open.iter().position(|entry| {
            entry.listener.is_none() && entry.retry_at.is_none_or(|retry_at| retry_at <= now)
        });
        if let Some(index) = attempt {
            let port = Arc::clone(&open[index].port);
            let result = opener.open(Arc::clone(&port));

            // Recheck the latest desired revision after the potentially slow
            // open. A removed candidate's listener and error are both stale.
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let wanted =
                !shared.stopping.load(Ordering::Acquire) && desired_contains(&state.desired, &port);
            if wanted {
                let Some(entry) = open
                    .iter_mut()
                    .find(|entry| Arc::ptr_eq(&entry.port, &port))
                else {
                    // Defensive only: the worker is the sole owner of `open`,
                    // but never let a future refactor drop a platform handle
                    // while holding the producer's mailbox mutex.
                    drop(state);
                    drop(result);
                    continue;
                };
                match result {
                    Ok(listener) => {
                        entry.listener = Some(listener);
                        entry.retry_at = None;
                        entry.attempts = 0;
                        entry.failure_reported = false;
                    }
                    Err(message) => {
                        entry.attempts = entry.attempts.saturating_add(1);
                        entry.retry_at = Instant::now()
                            .checked_add(retry_delay(entry.attempts, config.retry_base));
                        if !entry.failure_reported {
                            push_report(&mut state, message);
                            entry.failure_reported = true;
                        }
                    }
                }
            } else {
                // If this is a listener, its potentially blocking Drop occurs
                // here on the worker after releasing the manager mailbox.
                drop(state);
                drop(result);
                continue;
            }
            drop(state);
            shared.connected.store(
                open.iter().filter(|entry| entry.listener.is_some()).count(),
                Ordering::Release,
            );
            continue;
        }

        let next_retry = open
            .iter()
            .filter(|entry| entry.listener.is_none())
            .filter_map(|entry| entry.retry_at)
            .min();
        let wait = next_retry
            .map(|retry_at| retry_at.saturating_duration_since(Instant::now()))
            .unwrap_or(config.idle_poll)
            .min(config.idle_poll);
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.revision == revision && !shared.stopping.load(Ordering::Acquire) {
            let _ = shared
                .wake
                .wait_timeout(state, wait)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    shared.connected.store(0, Ordering::Release);
    // Listener destruction is worker-owned and may block. `MidiInputs::drop`
    // detached this thread before we reach it.
    drop(open);
    shared.worker_alive.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_midi::input::decode;

    fn feed(port: &InputPort, bytes: &[u8]) {
        if let Some(event) = decode(bytes) {
            dispatch(port, 0, event);
        }
    }

    /// The whole reason `dispatch` matches on a decoded event. Upstream writes
    /// `dataBytes[0]` as the CC number whatever the message was, so playing
    /// note 74 moves "CC74". On an Arturia MiniLab the knobs are CC 74-81 and
    /// the pads are notes 36-43, and the keyboard reaches note 74 octave-shifted.
    #[test]
    fn a_note_on_never_writes_a_control_change() {
        let port = InputPort::new("fake".into());
        feed(&port, &[0x90, 74, 100]);
        assert_eq!(
            port.read_cc(74, 0),
            0.0,
            "a note-on corrupted a control change"
        );
        assert!(!port.has_been_touched(74, 0));

        // A real control change still lands, on its channel and on any.
        feed(&port, &[0xb0, 74, 127]);
        assert_eq!(port.read_cc(74, 0), 1.0);
        assert_eq!(port.read_cc(74, 1), 1.0);
        assert_eq!(port.read_cc(74, 2), 0.0);
    }

    #[test]
    fn a_clock_or_active_sensing_byte_touches_nothing() {
        let port = InputPort::new("fake".into());
        feed(&port, &[0xf8]);
        feed(&port, &[0xfe]);
        feed(&port, &[0xfa]);
        assert_eq!(port.messages(), 0);
    }

    #[test]
    fn a_note_on_with_velocity_zero_is_a_note_off_and_never_enters_the_ring() {
        let port = InputPort::new("fake".into());
        feed(&port, &[0x90, 60, 0]);
        let mut out = Vec::new();
        port.keys.select(0.0, 1.0, 0, Some((0, 1)), &mut out);
        assert!(out.is_empty(), "a key release was recorded as a note");

        feed(&port, &[0x90, 60, 100]);
        let mut out = Vec::new();
        port.keys.select(0.0, 1.0, 0, Some((0, 1)), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].note, out[0].velocity), (60, 100));
    }

    #[test]
    fn program_change_and_aftertouch_do_not_land_in_the_control_table() {
        let port = InputPort::new("fake".into());
        // Two-byte messages: a naive reader takes byte 1 as a CC number.
        feed(&port, &[0xc0, 74]);
        feed(&port, &[0xd0, 74]);
        assert_eq!(port.read_cc(74, 0), 0.0);
        assert_eq!(port.messages(), 0);
    }

    struct RecordingListener {
        selector: String,
        dropped: std::sync::mpsc::Sender<String>,
        drop_gate: Option<(
            std::sync::mpsc::SyncSender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    }

    impl Drop for RecordingListener {
        fn drop(&mut self) {
            let _ = self.dropped.send(self.selector.clone());
            if let Some((entered, release)) = self.drop_gate.take() {
                let _ = entered.send(());
                let _ = release.recv();
            }
        }
    }

    type OpenGate = (String, std::sync::mpsc::Receiver<()>);
    type DropGate = (
        String,
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    );

    struct TestOpener {
        attempted: std::sync::mpsc::Sender<String>,
        dropped: std::sync::mpsc::Sender<String>,
        open_gate: Mutex<Option<OpenGate>>,
        drop_gate: Mutex<Option<DropGate>>,
        failure: Option<String>,
    }

    impl InputOpener for TestOpener {
        fn open(&self, port: Arc<InputPort>) -> Result<Box<dyn ManagedListener>, String> {
            let selector = port.selector.clone();
            let _ = self.attempted.send(selector.clone());
            let release = {
                let mut gate = self.open_gate.lock().unwrap_or_else(|e| e.into_inner());
                gate.as_ref()
                    .is_some_and(|(blocked, _)| blocked == &selector)
                    .then(|| gate.take().expect("checked open gate").1)
            };
            if let Some(release) = release {
                let _ = release.recv();
            }
            if let Some(message) = self.failure.as_ref() {
                return Err(message.clone());
            }
            let drop_gate = {
                let mut gate = self.drop_gate.lock().unwrap_or_else(|e| e.into_inner());
                gate.as_ref()
                    .is_some_and(|(blocked, _, _)| blocked == &selector)
                    .then(|| {
                        let (_, entered, release) = gate.take().expect("checked drop gate");
                        (entered, release)
                    })
            };
            Ok(Box::new(RecordingListener {
                selector,
                dropped: self.dropped.clone(),
                drop_gate,
            }))
        }
    }

    fn test_inputs(opener: Arc<TestOpener>) -> MidiInputs {
        MidiInputs::with_opener(
            opener,
            ManagerConfig {
                retry_base: Duration::from_millis(5),
                idle_poll: Duration::from_millis(5),
            },
        )
    }

    fn test_opener(
        open_gate: Option<OpenGate>,
        drop_gate: Option<DropGate>,
        failure: Option<String>,
    ) -> (
        Arc<TestOpener>,
        std::sync::mpsc::Receiver<String>,
        std::sync::mpsc::Receiver<String>,
    ) {
        let (attempted, attempts) = std::sync::mpsc::channel();
        let (dropped, drops) = std::sync::mpsc::channel();
        (
            Arc::new(TestOpener {
                attempted,
                dropped,
                open_gate: Mutex::new(open_gate),
                drop_gate: Mutex::new(drop_gate),
                failure,
            }),
            attempts,
            drops,
        )
    }

    fn wait_for_status(inputs: &MidiInputs, predicate: impl Fn(MidiInputStatus) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !predicate(inputs.status()) {
            assert!(
                Instant::now() < deadline,
                "MIDI input worker did not converge"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drop_before_deadline(inputs: MidiInputs) {
        let (finished, done) = std::sync::mpsc::sync_channel(0);
        std::thread::spawn(move || {
            drop(inputs);
            let _ = finished.send(());
        });
        done.recv_timeout(Duration::from_secs(1))
            .expect("MidiInputs::drop waited for its platform worker");
    }

    #[test]
    fn a_blocking_platform_open_never_blocks_sync_or_manager_drop() {
        let (release_open, blocked_open) = std::sync::mpsc::channel();
        let (opener, attempts, _drops) =
            test_opener(Some(("blocked".into(), blocked_open)), None, None);
        let mut inputs = test_inputs(opener);
        let bus = InputBus::new();
        bus.intern("blocked").unwrap();

        assert!(inputs.sync(&bus, 0, 0).is_empty());
        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "blocked"
        );

        bus.commit_generation(1, Vec::new()).unwrap();
        let started = Instant::now();
        assert!(inputs.sync(&bus, 1, 1).is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "producer sync waited for a blocked platform open"
        );
        drop_before_deadline(inputs);
        let _ = release_open.send(());
    }

    #[test]
    fn a_blocking_listener_drop_never_blocks_sync_or_manager_drop() {
        let (drop_entered, entered_drop) = std::sync::mpsc::sync_channel(0);
        let (release_drop, blocked_drop) = std::sync::mpsc::channel();
        let drop_gate = Some(("sticky".into(), drop_entered, blocked_drop));
        let (opener, attempts, _drops) = test_opener(None, drop_gate, None);
        let mut inputs = test_inputs(opener);
        let bus = InputBus::new();
        bus.intern("sticky").unwrap();
        inputs.sync(&bus, 0, 0);
        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "sticky"
        );
        wait_for_status(&inputs, |status| status.connected == 1);

        bus.commit_generation(1, Vec::new()).unwrap();
        inputs.sync(&bus, 1, 1);
        entered_drop
            .recv_timeout(Duration::from_secs(2))
            .expect("worker did not retire the listener");
        let started = Instant::now();
        assert!(inputs.sync(&bus, 1, 1).is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "producer sync waited for a listener destructor"
        );
        drop_before_deadline(inputs);
        let _ = release_drop.send(());
    }

    #[test]
    fn a_slow_open_result_is_discarded_when_its_generation_is_stale() {
        let (release_open, blocked_open) = std::sync::mpsc::channel();
        let (opener, attempts, drops) = test_opener(Some(("old".into(), blocked_open)), None, None);
        let mut inputs = test_inputs(opener);
        let bus = InputBus::new();
        bus.intern("old").unwrap();
        inputs.sync(&bus, 0, 0);
        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "old"
        );

        let new = Arc::new(InputPort::new("new".into()));
        bus.commit_generation(1, vec![new]).unwrap();
        inputs.sync(&bus, 1, 1);
        release_open.send(()).unwrap();

        assert_eq!(drops.recv_timeout(Duration::from_secs(2)).unwrap(), "old");
        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "new"
        );
        wait_for_status(&inputs, |status| {
            status.desired == 1 && status.connected == 1
        });
    }

    #[test]
    fn revisions_coalesce_while_the_worker_is_inside_a_slow_open() {
        let (release_open, blocked_open) = std::sync::mpsc::channel();
        let (opener, attempts, drops) = test_opener(Some(("old".into(), blocked_open)), None, None);
        let mut inputs = test_inputs(opener);
        let bus = InputBus::new();
        bus.intern("old").unwrap();
        inputs.sync(&bus, 0, 0);
        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "old"
        );

        bus.commit_generation(1, vec![Arc::new(InputPort::new("middle".into()))])
            .unwrap();
        inputs.sync(&bus, 1, 1);
        bus.commit_generation(2, vec![Arc::new(InputPort::new("latest".into()))])
            .unwrap();
        inputs.sync(&bus, 2, 2);
        release_open.send(()).unwrap();

        assert_eq!(drops.recv_timeout(Duration::from_secs(2)).unwrap(), "old");
        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "latest",
            "worker opened an intermediate superseded generation"
        );
        assert!(attempts.recv_timeout(Duration::from_millis(30)).is_err());
    }

    #[test]
    fn revisions_coalesce_while_the_worker_is_inside_a_slow_drop() {
        let (drop_entered, entered_drop) = std::sync::mpsc::sync_channel(0);
        let (release_drop, blocked_drop) = std::sync::mpsc::channel();
        let drop_gate = Some(("old".into(), drop_entered, blocked_drop));
        let (opener, attempts, drops) = test_opener(None, drop_gate, None);
        let mut inputs = test_inputs(opener);
        let bus = InputBus::new();
        bus.intern("old").unwrap();
        inputs.sync(&bus, 0, 0);
        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "old"
        );
        wait_for_status(&inputs, |status| status.connected == 1);

        bus.commit_generation(1, vec![Arc::new(InputPort::new("middle".into()))])
            .unwrap();
        inputs.sync(&bus, 1, 1);
        assert_eq!(drops.recv_timeout(Duration::from_secs(2)).unwrap(), "old");
        entered_drop
            .recv_timeout(Duration::from_secs(2))
            .expect("worker did not enter the slow listener destructor");

        bus.commit_generation(2, vec![Arc::new(InputPort::new("latest".into()))])
            .unwrap();
        inputs.sync(&bus, 2, 2);
        release_drop.send(()).unwrap();

        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "latest",
            "worker opened a revision superseded while listener Drop blocked"
        );
        assert!(attempts.recv_timeout(Duration::from_millis(30)).is_err());
    }

    #[test]
    fn failed_opens_retry_without_repeating_the_same_report() {
        let (opener, attempts, _drops) =
            test_opener(None, None, Some("controller is unavailable".into()));
        let mut inputs = test_inputs(opener);
        let bus = InputBus::new();
        bus.intern("missing").unwrap();
        inputs.sync(&bus, 0, 0);
        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "missing"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        let first = loop {
            let reports = inputs.sync(&bus, 0, 0);
            if !reports.is_empty() {
                break reports;
            }
            assert!(
                Instant::now() < deadline,
                "worker produced no failure report"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(first, ["controller is unavailable"]);
        assert_eq!(
            attempts.recv_timeout(Duration::from_secs(2)).unwrap(),
            "missing"
        );
        assert!(
            inputs.sync(&bus, 0, 0).is_empty(),
            "a retry repeated the already reported failure"
        );
    }

    #[test]
    fn audible_and_candidate_inputs_coexist_only_until_cutover() {
        let (opener, _attempts, drops) = test_opener(None, None, None);
        let mut inputs = test_inputs(opener);
        let bus = InputBus::new();
        let audible = Arc::new(InputPort::new("audible".into()));
        let candidate = Arc::new(InputPort::new("candidate".into()));
        bus.commit_generation(1, vec![audible]).unwrap();
        bus.snapshot_for(1, 1);
        bus.commit_generation(2, vec![candidate]).unwrap();

        inputs.sync(&bus, 1, 2);
        wait_for_status(&inputs, |status| {
            status.desired == 2 && status.connected == 2
        });
        inputs.sync(&bus, 2, 2);
        wait_for_status(&inputs, |status| {
            status.desired == 1 && status.connected == 1
        });
        assert_eq!(
            drops.recv_timeout(Duration::from_secs(2)).unwrap(),
            "audible"
        );
    }

    #[test]
    fn report_retention_and_message_size_are_strictly_bounded() {
        let mut state = ManagerState::default();
        let oversized = "é".repeat(MAX_INPUT_REPORT_BYTES);
        for index in 0..MAX_PENDING_INPUT_REPORTS + 5 {
            push_report(&mut state, format!("{index}:{oversized}"));
        }
        assert_eq!(state.reports.len(), MAX_PENDING_INPUT_REPORTS);
        assert_eq!(state.reports_dropped, 5);
        assert!(
            state
                .reports
                .iter()
                .all(|message| message.len() <= MAX_INPUT_REPORT_BYTES)
        );

        let reports = take_reports(&mut state);
        assert_eq!(reports.len(), MAX_PENDING_INPUT_REPORTS + 1);
        assert!(reports.last().unwrap().contains("5 additional"));
        assert!(state.reports.is_empty());
        assert_eq!(state.reports_dropped_pending, 0);
    }

    #[test]
    fn sync_never_publishes_more_than_the_audible_candidate_limit() {
        let (opener, _attempts, _drops) = test_opener(None, None, None);
        let mut inputs = test_inputs(opener);
        let bus = InputBus::new();
        for index in 0..MAX_INPUT_PORTS {
            bus.intern(&format!("port-{index}")).unwrap();
        }
        inputs.sync(&bus, 0, 0);
        assert!(inputs.status().desired <= MAX_RETAINED_INPUTS);
    }
}
