//! The insert that keeps native synthesis off the audio callback.
//!
//! The callback never waits for Csound. It posts commands into a queue and
//! reads frames from a small ring. The worker thread renders ahead of the
//! callback by a runway the score's own schedule lead pays for: a note
//! reaches the worker about as early as it reaches every other voice, and
//! the worker has each frame ready long before the callback reads it. When
//! the worker falls behind anyway - a first wavetable build, an orchestra
//! that costs more than the runway buys - the ring runs dry, that one
//! insert falls silent for the gap, and the rest of the output plays on.
//!
//! An offline render pulls instead. It has no callback to keep fed, and a
//! runway would cost it exactness: a note the scheduler admits inside its
//! own block lands behind the frontier the runway holds. So the render
//! asks for each block's frames after it delivered the block's notes, the
//! worker renders that far and no further, and every note lands on its
//! own frame the way it did when the engine ran on the render thread.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rustel_audio::{InsertKey, InsertParam, OrbitInsert};

use super::{Counters, Program, engine::Engine};

/// The block the callback asks an insert for at once. The runway stays one
/// such block under the schedule lead, so a note posted at admission still
/// lands on its own frame.
const CALLBACK_BLOCK: u64 = 128;
/// The most audio the worker may render beyond what the callback has read.
/// The lead at a 128-frame buffer is five to six buffers, so the runway
/// takes four of them and leaves the rest for the wake.
const RUNWAY_FRAMES: u64 = 512;
/// The runway before any note has said its lead: two blocks, so the frames
/// of a note the activation posts with no lead of its own are ready before
/// the callback asks. A lead shorter than this, which only a very small
/// host buffer gives, can arrive after the worker passed its frame by the
/// difference, and the note starts that late.
const MIN_RUNWAY_FRAMES: u64 = 256;
/// Ring capacity in frames: the runway, the block in flight, and room for a
/// stop between publishes. A power of two keeps the wrap cheap.
const RING_FRAMES: u64 = 4096;
/// Commands the callback may post before the worker drains them.
const COMMAND_CAPACITY: u64 = 256;
/// The value of a confirmation slot until the worker confirms the end of
/// its control's stale interval.
const UNCONFIRMED: u64 = u64::MAX;
/// Cancellations have their own queue, sized from the existing scheduled-event
/// and command ceilings. Repeated or unknown IDs share this bound. A full queue
/// refuses requests without cutting notes, as reported by `refused_commands`.
/// Under Miri the queue is small, because every worker test builds one.
const CANCEL_CAPACITY: usize = if cfg!(miri) {
    2 * COMMAND_CAPACITY as usize
} else {
    super::MAX_EVENTS + COMMAND_CAPACITY as usize
};

#[derive(Clone, Copy, Debug)]
struct Cancellation {
    id: u64,
    /// Only scores posted before this command cursor can be retracted. An
    /// unknown ID must not cancel a later submission of that ID.
    through: u64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Command {
    /// Plays the prepared score event `id` at frame `due`.
    Score { id: u64, due: u64 },
    /// Ends every note, also those the orchestra scheduled itself or the
    /// host still had waiting, and restarts the orchestra at frame `due`.
    Cut { due: u64, mask: usize },
    /// Drops every scheduled event and restarts the orchestra at `due`.
    Reset { due: u64, mask: usize },
}

/// A cut or reset before it posts. It becomes a command only when it gets
/// a slot for the worker's confirmation.
#[derive(Clone, Copy)]
enum Control {
    Cut,
    Reset,
}

impl Control {
    /// The command that carries this control at frame `due`. The worker
    /// confirms its restart in slot `mask`.
    fn command(self, due: u64, mask: usize) -> Command {
        match self {
            Control::Cut => Command::Cut { due, mask },
            Control::Reset => Command::Reset { due, mask },
        }
    }
}

/// What the callback and the worker share: a ring of interleaved stereo
/// frames the worker fills and the callback drains, a queue of commands the
/// other way, and the indices that hand both over. Every index only grows,
/// so each slot belongs to exactly one side at a time.
struct Channel {
    commands: UnsafeCell<Box<[Command; COMMAND_CAPACITY as usize]>>,
    command_head: AtomicU64,
    command_tail: AtomicU64,
    cancellations: Box<[UnsafeCell<Cancellation>]>,
    cancellation_head: AtomicU64,
    cancellation_tail: AtomicU64,
    samples: UnsafeCell<Box<[f32; RING_FRAMES as usize * 2]>>,
    written: AtomicU64,
    read: AtomicU64,
    /// Each slot confirms the end of one cut's stale audio interval.
    /// The callback reuses it only after acquiring the confirmation.
    clean_from: [AtomicU64; COMMAND_CAPACITY as usize],
    /// The frame where the worker started its orchestra last: 0 for the
    /// orchestra the insert starts with, the frame of the note that asked
    /// for a later one. The callback counts a dry frame as starved only from
    /// one runway past it.
    started: AtomicU64,
    /// The largest note lead the callback has posted.
    lead: AtomicU64,
    /// The frame an offline render asked for last. The worker renders to
    /// it and no further, so its frontier never passes a frame a note can
    /// still land on.
    wanted: AtomicU64,
    /// True once an offline render pulls the worker. Live, the worker runs
    /// ahead of the callback by a runway instead.
    offline: AtomicBool,
    stopping: AtomicBool,
    dead: AtomicBool,
}

// The callback owns each command, cancellation and ring slot until it
// publishes the index that hands it over. The worker owns it until it
// publishes the index that returns it. Release stores and acquire loads of the
// indices order the slot writes before the slot reads.
unsafe impl Sync for Channel {}

impl Channel {
    fn new() -> Self {
        Self {
            commands: UnsafeCell::new(Box::new(
                [Command::Reset { due: 0, mask: 0 }; COMMAND_CAPACITY as usize],
            )),
            command_head: AtomicU64::new(0),
            command_tail: AtomicU64::new(0),
            cancellations: std::iter::repeat_with(|| {
                UnsafeCell::new(Cancellation { id: 0, through: 0 })
            })
            .take(CANCEL_CAPACITY)
            .collect(),
            cancellation_head: AtomicU64::new(0),
            cancellation_tail: AtomicU64::new(0),
            samples: UnsafeCell::new(Box::new([0.0; RING_FRAMES as usize * 2])),
            written: AtomicU64::new(0),
            read: AtomicU64::new(0),
            clean_from: std::array::from_fn(|_| AtomicU64::new(UNCONFIRMED)),
            started: AtomicU64::new(0),
            lead: AtomicU64::new(0),
            wanted: AtomicU64::new(0),
            offline: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            dead: AtomicBool::new(false),
        }
    }

    /// Queues one command. False means the queue is full.
    fn post(&self, command: Command) -> bool {
        let tail = self.command_tail.load(Ordering::Relaxed);
        let head = self.command_head.load(Ordering::Acquire);
        if tail - head >= COMMAND_CAPACITY {
            return false;
        }
        unsafe {
            (*self.commands.get())[(tail % COMMAND_CAPACITY) as usize] = command;
        }
        self.command_tail.store(tail + 1, Ordering::Release);
        true
    }

    /// Takes the next command, in the order posted.
    fn drain(&self) -> Option<Command> {
        let head = self.command_head.load(Ordering::Relaxed);
        let tail = self.command_tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let command = unsafe { (*self.commands.get())[(head % COMMAND_CAPACITY) as usize] };
        self.command_head.store(head + 1, Ordering::Release);
        Some(command)
    }

    /// Posts a cancellation without taking space from score or transport commands.
    fn cancel(&self, id: u64) -> bool {
        let tail = self.cancellation_tail.load(Ordering::Relaxed);
        let head = self.cancellation_head.load(Ordering::Acquire);
        if tail - head >= CANCEL_CAPACITY as u64 {
            return false;
        }
        let cancellation = Cancellation {
            id,
            through: self.command_tail.load(Ordering::Relaxed),
        };
        unsafe {
            *self.cancellations[(tail % CANCEL_CAPACITY as u64) as usize].get() = cancellation;
        }
        self.cancellation_tail.store(tail + 1, Ordering::Release);
        true
    }

    fn take_cancellation(&self, tail: u64) -> Option<Cancellation> {
        let head = self.cancellation_head.load(Ordering::Relaxed);
        if head == tail {
            return None;
        }
        let cancellation =
            unsafe { *self.cancellations[(head % CANCEL_CAPACITY as u64) as usize].get() };
        self.cancellation_head.store(head + 1, Ordering::Release);
        Some(cancellation)
    }

    /// Stores one frame and publishes it. The worker calls this for frame
    /// `at`, then never touches that slot again until the ring wraps past
    /// the frame the callback has read.
    fn publish(&self, frame: [f32; 2], at: u64) {
        let slot = (at % RING_FRAMES) as usize * 2;
        unsafe {
            // Borrow only these slots, since the reader owns other slots.
            (*self.samples.get())[slot] = frame[0];
            (*self.samples.get())[slot + 1] = frame[1];
        }
        self.written.store(at + 1, Ordering::Release);
    }

    /// Takes one published frame.
    fn take(&self, at: u64) -> [f32; 2] {
        let slot = (at % RING_FRAMES) as usize * 2;
        unsafe { [(*self.samples.get())[slot], (*self.samples.get())[slot + 1]] }
    }
}

struct Exit(Arc<Channel>);

impl Drop for Exit {
    fn drop(&mut self) {
        self.0.dead.store(true, Ordering::Release);
    }
}

struct Active(Arc<AtomicUsize>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// All callback operations post commands and read the ring. None of them
/// waits for the worker.
pub(super) struct Insert {
    key: InsertKey,
    channel: Arc<Channel>,
    counters: Arc<Counters>,
    worker: Option<JoinHandle<()>>,
    /// The first frame of the block in flight. Every due frame a method
    /// names anchors here, and the counter moves at the end of the block,
    /// so each anchor lands inside its own block.
    frame: u64,
    /// Unconfirmed cuts own one acknowledgement slot each.
    masks: [Option<u64>; COMMAND_CAPACITY as usize],
    /// Confirmed stale frames, indexed like the audio ring. Only the
    /// callback touches these bits and clears them as it consumes frames.
    stale: [u64; RING_FRAMES as usize / 64],
    /// The frame of the last reset posted. A stopped output repeats its
    /// reset each block, and one post per frame is the whole ask.
    reset_at: Option<u64>,
    /// A cut or reset that the slot table or the queue refused, with its
    /// due frame. Until it posts, scores are refused and later controls
    /// merge into it.
    pending_control: Option<(Control, u64)>,
    failed: bool,
}

impl Insert {
    pub(super) fn new(program: Program, sample_rate: u32) -> Result<Self, String> {
        let key = program.key();
        let counters = Arc::clone(&program.counters);
        let channel = Arc::new(Channel::new());
        let shared = Arc::clone(&channel);
        let (ready, startup) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("csound".into())
            .spawn(move || {
                program.instances.fetch_add(1, Ordering::Relaxed);
                let _active = Active(program.instances.clone());
                let _exit = Exit(Arc::clone(&shared));
                let engine = match Engine::new(&program.sources, sample_rate) {
                    Ok(engine) => engine,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let mut state = State {
                    program,
                    sample_rate,
                    channel: shared,
                    engine: Some(engine),
                    pending: Vec::new(),
                    pending_bytes: 0,
                    write: 0,
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                state.run();
            })
            .map_err(|error| format!("Csound worker could not start: {error}"))?;
        match startup.recv() {
            Ok(Ok(())) => Ok(Self {
                key,
                channel,
                counters,
                worker: Some(worker),
                frame: 0,
                masks: [None; COMMAND_CAPACITY as usize],
                stale: [0; RING_FRAMES as usize / 64],
                reset_at: None,
                pending_control: None,
                failed: false,
            }),
            result => {
                let _ = worker.join();
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => "Csound worker stopped during startup".into(),
                })
            }
        }
    }

    fn reclaim_masks(&mut self) {
        for (slot, due) in self.masks.iter_mut().enumerate() {
            let Some(from) = *due else { continue };
            let end = self.channel.clean_from[slot].load(Ordering::Acquire);
            if end == UNCONFIRMED {
                continue;
            }
            // Only already-rendered, unread frames enter the bitmap.
            let mut from = from.max(self.frame);
            let end = end.min(self.frame.saturating_add(RING_FRAMES));
            while from < end {
                let bit = (from % RING_FRAMES) as usize;
                let count = (end - from).min((64 - bit % 64) as u64) as u32;
                let bits = u64::MAX >> (64 - count);
                self.stale[bit / 64] |= bits << (bit % 64);
                from += u64::from(count);
            }
            *due = None;
        }
    }

    /// Posts a control with a free confirmation slot. False means the slot
    /// table or the queue is full.
    fn try_post_control(&mut self, control: Control, due: u64) -> bool {
        let Some(mask) = self.masks.iter().position(Option::is_none) else {
            return false;
        };
        self.channel.clean_from[mask].store(UNCONFIRMED, Ordering::Relaxed);
        if !self.channel.post(control.command(due, mask)) {
            return false;
        }
        self.masks[mask] = Some(due);
        true
    }

    fn stopped(&self) -> bool {
        self.channel.dead.load(Ordering::Acquire) || self.channel.stopping.load(Ordering::Acquire)
    }

    fn retry_control(&mut self) {
        self.reclaim_masks();
        if let Some((control, due)) = self.pending_control
            && self.try_post_control(control, due)
        {
            self.pending_control = None;
        }
    }

    /// Latches a stopped worker and retries a held control. False means
    /// the insert posts nothing more.
    fn prepare_post(&mut self) -> bool {
        if self.failed {
            return false;
        }
        if self.stopped() {
            self.failed = true;
            return false;
        }
        self.retry_control();
        true
    }

    fn post_score(&mut self, id: u64, due: u64) {
        if !self.prepare_post() {
            return;
        }
        if self.pending_control.is_none() && self.channel.post(Command::Score { id, due }) {
            return;
        }
        // A refused score can stay silent.
        self.counters
            .refused_commands
            .fetch_add(1, Ordering::Relaxed);
    }

    fn post_control(&mut self, control: Control, due: u64) {
        if !self.prepare_post() {
            return;
        }
        if self.pending_control.is_none() && self.try_post_control(control, due) {
            return;
        }
        self.counters
            .refused_commands
            .fetch_add(1, Ordering::Relaxed);
        // A cut must reach the worker before another score, or its
        // stale-frame mask would never end.
        self.hold_control(control, due);
    }

    /// Holds a refused control until `retry_control` posts it. A control
    /// refused while another waits merges into it: a reset absorbs a cut,
    /// and the earlier due frame wins.
    fn hold_control(&mut self, control: Control, due: u64) {
        self.pending_control = Some(match (self.pending_control, control) {
            (None, _) => (control, due),
            (Some((Control::Cut, held)), Control::Cut) => (Control::Cut, due.min(held)),
            (Some((_, held)), _) => (Control::Reset, due.min(held)),
        });
    }
}

impl OrbitInsert for Insert {
    fn key(&self) -> InsertKey {
        self.key
    }

    fn set_param(&mut self, _: InsertParam, _: u32) {}

    fn score(&mut self, id: u64, frames: u32) {
        let due = self.frame.saturating_add(u64::from(frames));
        // The lead of this note says how far ahead the worker may render.
        self.channel
            .lead
            .fetch_max(u64::from(frames), Ordering::Release);
        self.post_score(id, due);
    }

    fn cancel_score(&mut self, id: u64) {
        if self.failed || self.stopped() {
            return;
        }
        if !self.channel.cancel(id) {
            self.counters
                .refused_commands
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn cut_notes(&mut self, frames: u32) {
        let due = self.frame.saturating_add(u64::from(frames));
        self.post_control(Control::Cut, due);
    }

    fn reset(&mut self) {
        let due = self.frame;
        if self.reset_at == Some(due) {
            return;
        }
        self.reset_at = Some(due);
        self.post_control(Control::Reset, due);
    }

    fn busy(&self) -> bool {
        !self.failed && !self.stopped()
    }

    fn renders_ahead(&self) -> bool {
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.submit(left, right);
        self.collect(left, right);
    }

    /// The insert makes its own sound; the input it gets is silence. Only
    /// the block length matters, and collect reads it from its slices.
    fn submit(&mut self, left: &mut [f32], right: &mut [f32]) {
        assert_eq!(left.len(), right.len());
    }

    fn collect(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.retry_control();
        let start = self.frame;
        // One look per block: frames published later belong to the future.
        let written = self.channel.written.load(Ordering::Acquire);
        self.reclaim_masks();
        let pending_from = self
            .masks
            .iter()
            .flatten()
            .copied()
            .chain(self.pending_control.map(|(_, due)| due))
            .min();
        // The worker fills the first runway of an orchestra late: after the
        // first read, or after the compile a note asked for. Those frames,
        // and the silence before that note, read dry by design.
        let warm = self
            .channel
            .started
            .load(Ordering::Acquire)
            .saturating_add(RUNWAY_FRAMES);
        let mut starved = 0u64;
        for (index, (out_left, out_right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let frame = start + index as u64;
            let bit = (frame % RING_FRAMES) as usize;
            let stale = self.stale[bit / 64] & (1 << (bit % 64)) != 0;
            self.stale[bit / 64] &= !(1 << (bit % 64));
            if frame < written && !stale && !pending_from.is_some_and(|due| frame >= due) {
                let [sample_left, sample_right] = self.channel.take(frame);
                *out_left = sample_left;
                *out_right = sample_right;
            } else {
                if frame >= written && frame >= warm {
                    starved += 1;
                }
                *out_left = 0.0;
                *out_right = 0.0;
            }
        }
        self.frame = start.saturating_add(left.len() as u64);
        self.channel.read.store(self.frame, Ordering::Release);
        if starved > 0 {
            self.counters
                .starved_frames
                .fetch_add(starved, Ordering::Relaxed);
        }
        if self.stopped() {
            self.failed = true;
        }
    }

    fn prime(&mut self, frames: usize) {
        self.flush(frames);
        self.channel.offline.store(false, Ordering::Release);
    }

    fn flush(&mut self, frames: usize) {
        while self.pending_control.is_some() {
            if self.stopped() {
                return;
            }
            self.retry_control();
            if self.pending_control.is_some() {
                thread::sleep(Duration::from_micros(50));
            }
        }
        // An offline render reads faster than real time, and it cannot
        // afford a runway either: a note the scheduler admits inside its
        // own block would land behind the frontier a runway holds. So the
        // render pulls the worker. It delivers a block's notes first, then
        // asks for the block's frames, and the worker renders exactly that
        // far and no further. A worker that stopped ends the wait: the
        // frames it still owes read as the silence of a dropped insert.
        self.channel.offline.store(true, Ordering::Release);
        let ready = self.frame.saturating_add(frames as u64);
        self.channel.wanted.store(ready, Ordering::Release);
        while self.channel.written.load(Ordering::Acquire) < ready {
            if self.stopped() {
                return;
            }
            thread::sleep(Duration::from_micros(50));
        }
    }
}

impl Drop for Insert {
    fn drop(&mut self) {
        self.channel.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum Action {
    Score {
        id: u64,
        submitted: u64,
        score: String,
    },
    Cut,
}

struct Scheduled {
    frame: u64,
    action: Action,
}

struct State {
    program: Program,
    sample_rate: u32,
    channel: Arc<Channel>,
    engine: Option<Engine>,
    pending: Vec<Scheduled>,
    pending_bytes: usize,
    /// The frame the worker renders next. It only moves forward.
    write: u64,
}

impl State {
    fn run(&mut self) {
        let mut spins = 0u16;
        let mut cancelled = Vec::new();
        loop {
            if self.channel.stopping.load(Ordering::Acquire) {
                return;
            }
            // The bound comes before the commands, so each command the
            // callback posted before it raised this bound is in the queue
            // now. The callback posts a block's notes before it moves the
            // bound for that block.
            let lead = self.channel.lead.load(Ordering::Acquire);
            let runway = lead
                .saturating_sub(CALLBACK_BLOCK)
                .clamp(MIN_RUNWAY_FRAMES, RUNWAY_FRAMES);
            let read = self.channel.read.load(Ordering::Acquire);
            let target = if self.channel.offline.load(Ordering::Acquire) {
                // The offline render delivers a block's notes before it
                // asks for the block's frames, so the frontier the worker
                // holds at that ask never passes a due the block carries.
                self.channel.wanted.load(Ordering::Acquire)
            } else if read == 0 {
                // Nothing has been read yet. The activation posts an
                // insert's first note before the insert's first collect,
                // so the frontier stays under whatever due that note
                // carries. Frames before it are silence the reader reads
                // for free, also when an always-on instrument plays.
                self.write
            } else {
                read.saturating_add(runway)
            };
            self.drain_commands(&mut cancelled);
            let mut rendered = false;
            while self.write < target {
                self.step();
                rendered = true;
            }
            // Csound reports a score it refuses while it performs, so the
            // messages of the batch just rendered end it.
            if rendered
                && let Some(engine) = self.engine.as_mut()
                && let Err(error) = engine.check_messages()
            {
                self.fail(error);
            }
            if rendered {
                spins = 0;
                continue;
            }
            if spins < 1024 {
                spins += 1;
                std::hint::spin_loop();
            } else {
                thread::sleep(Duration::from_micros(50));
            }
        }
    }

    fn drain_commands(&mut self, cancelled: &mut Vec<Cancellation>) {
        // Acquire the cancellation batch before draining commands: every score
        // covered by its cursors must be visible before cancellation is applied.
        let tail = self.channel.cancellation_tail.load(Ordering::Acquire);
        cancelled.clear();
        while let Some(cancellation) = self.channel.take_cancellation(tail) {
            cancelled.push(cancellation);
        }
        cancelled.sort_unstable_by_key(|cancellation| (cancellation.id, cancellation.through));
        let retract = |id: u64, submitted: u64| {
            let position = cancelled.partition_point(|cancellation| cancellation.id <= id);
            position > 0
                && cancelled[position - 1].id == id
                && submitted < cancelled[position - 1].through
        };
        // Free retired entries before accepting replacement scores, including
        // when the schedule was at its ceiling. Never touch a transport cut.
        if !cancelled.is_empty() {
            self.pending.retain(|event| {
                if let Action::Score {
                    id,
                    submitted,
                    score,
                } = &event.action
                    && retract(*id, *submitted)
                {
                    self.pending_bytes -= score.len();
                    false
                } else {
                    true
                }
            });
        }
        while let Some(command) = self.channel.drain() {
            let submitted = self.channel.command_head.load(Ordering::Relaxed) - 1;
            match command {
                Command::Score { id, due } => {
                    if let Some(score) = self.program.take(id)
                        && !retract(id, submitted)
                        && let Err(error) = self.schedule(
                            due,
                            Action::Score {
                                id,
                                submitted,
                                score,
                            },
                        )
                    {
                        self.program.report_error(error);
                    }
                }
                Command::Cut { due, mask } => {
                    self.pending.retain(|event| {
                        if event.frame >= due
                            && let Action::Score { score, .. } = &event.action
                        {
                            self.pending_bytes -= score.len();
                            false
                        } else {
                            true
                        }
                    });
                    if let Err(error) = self.schedule(due, Action::Cut) {
                        self.program.report_error(error);
                    }
                    self.confirm_cut(mask, due);
                }
                Command::Reset { due, mask } => {
                    self.clear_pending();
                    if self.engine.is_some() {
                        let _ = self.schedule(due, Action::Cut);
                    }
                    self.confirm_cut(mask, due);
                }
            }
        }
    }

    fn schedule(&mut self, due: u64, action: Action) -> Result<(), String> {
        let bytes = match &action {
            Action::Score { score, .. } => score.len(),
            Action::Cut => 0,
        };
        if matches!(action, Action::Cut) && self.pending.len() >= super::MAX_EVENTS {
            let error = "Csound scheduled event limit reached".to_owned();
            if let Some(position) = self
                .pending
                .iter()
                .rposition(|event| matches!(event.action, Action::Score { .. }))
                && let Action::Score { score, .. } = self.pending.remove(position).action
            {
                self.pending_bytes -= score.len();
                self.program.report_error(error);
            } else {
                // A full schedule of cuts cannot lose an acknowledged stop.
                // Silence the orchestra before retiring those controls.
                self.fail(error);
            }
        }
        if self.pending.len() >= super::MAX_EVENTS
            || bytes > super::MAX_EVENT_BYTES.saturating_sub(self.pending_bytes)
        {
            return Err("Csound scheduled event limit reached".into());
        }
        // A due frame behind the frontier fires at the next frame the
        // worker renders.
        let frame = due.max(self.write);
        let position = self.pending.partition_point(|event| event.frame <= frame);
        self.pending.insert(position, Scheduled { frame, action });
        self.pending_bytes += bytes;
        Ok(())
    }

    /// Renders one frame into the ring.
    fn step(&mut self) {
        if self.engine.is_none()
            && let Some(onset) = self
                .pending
                .iter()
                .find(|event| matches!(event.action, Action::Score { .. }))
                .map(|event| event.frame)
        {
            // A cut took the orchestra, and a note has asked for it again.
            // The rest after the cut pays for the compile: the insert is
            // silent now, and the note has its schedule lead. This runs on
            // the worker, where a slow compile costs a dry ring, not a
            // dropout.
            if let Err(error) = self.start_orchestra(onset) {
                self.fail(error);
            }
        }
        while self
            .pending
            .first()
            .is_some_and(|event| event.frame <= self.write)
        {
            match self.pending.remove(0).action {
                Action::Score { score, .. } => {
                    self.pending_bytes -= score.len();
                    if self.engine.is_none()
                        && let Err(error) = self.start_orchestra(self.write)
                    {
                        self.fail(error);
                        continue;
                    }
                    if let Some(engine) = self.engine.as_mut()
                        && let Err(error) = engine.score(&score)
                    {
                        self.fail(error);
                    }
                }
                // Older future scores were retired when the command
                // arrived. Scores posted after it belong to the new sound.
                Action::Cut => {
                    let _ = self.engine.take();
                }
            }
        }
        let frame = match self.engine.as_mut() {
            Some(engine) => engine.frame(),
            None => Ok([0.0; 2]),
        };
        match frame {
            Ok(frame) => self.channel.publish(frame, self.write),
            Err(error) => {
                self.fail(error);
                self.channel.publish([0.0; 2], self.write);
            }
        }
        self.write += 1;
    }

    /// Builds a fresh orchestra for the note at `onset`. The insert is
    /// silent until that note, so the note marks the start. The callback
    /// reads dry past the frontier while the orchestra compiles.
    fn start_orchestra(&mut self, onset: u64) -> Result<(), String> {
        self.channel.started.store(onset, Ordering::Release);
        self.engine = Some(Engine::new(&self.program.sources, self.sample_rate)?);
        Ok(())
    }

    /// Keeps rendering after an error: the event list goes, the orchestra
    /// gives way to silence, and the next note starts a fresh one.
    fn fail(&mut self, error: String) {
        self.program.report_error(error);
        self.engine = None;
        self.clear_pending();
    }

    fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_bytes = 0;
    }

    fn confirm_cut(&self, mask: usize, due: u64) {
        // The scheduled cut runs before any frame at or after this frontier
        // is published. Future cuts therefore have an empty stale interval.
        self.channel.clean_from[mask].store(due.max(self.write), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker thread that fills the ring the way State does: one frame
    /// ahead of the read index, until `frames` frames are published.
    fn fill_ahead(channel: &Channel, frames: u64) {
        for at in 0..frames {
            channel.publish([at as f32, -(at as f32)], at);
        }
    }

    #[test]
    fn the_ring_hands_frames_to_the_reader_in_order_across_threads() {
        let shared = Arc::new(Channel::new());
        let writer = Arc::clone(&shared);
        let frames = RING_FRAMES + CALLBACK_BLOCK;
        let worker = thread::spawn(move || {
            for at in 0..frames {
                while at - writer.read.load(Ordering::Acquire) >= RUNWAY_FRAMES {
                    thread::yield_now();
                }
                writer.publish([at as f32, -(at as f32)], at);
            }
        });
        for frame in 0..frames {
            while shared.written.load(Ordering::Acquire) <= frame {
                thread::yield_now();
            }
            assert_eq!(shared.take(frame), [frame as f32, -(frame as f32)]);
            shared.read.store(frame + 1, Ordering::Release);
        }
        worker.join().unwrap();
    }

    #[test]
    fn the_command_queue_keeps_order_across_threads_and_reports_a_full_queue() {
        let channel = Arc::new(Channel::new());
        for at in 0..COMMAND_CAPACITY {
            assert!(channel.post(Command::Score { id: at, due: at }));
        }
        assert!(!channel.post(Command::Cut { due: 0, mask: 0 }));
        for at in 0..COMMAND_CAPACITY {
            assert_eq!(channel.drain(), Some(Command::Score { id: at, due: at }));
        }
        assert_eq!(channel.drain(), None);
        assert!(channel.post(Command::Cut { due: 7, mask: 0 }));
        assert_eq!(channel.drain(), Some(Command::Cut { due: 7, mask: 0 }));
        let writer = Arc::clone(&channel);
        let commands = COMMAND_CAPACITY * 2;
        let worker = thread::spawn(move || {
            for at in 0..commands {
                while !writer.post(Command::Score { id: at, due: at }) {
                    thread::yield_now();
                }
            }
        });
        for at in 0..commands {
            let command = loop {
                if let Some(command) = channel.drain() {
                    break command;
                }
                thread::yield_now();
            };
            assert_eq!(command, Command::Score { id: at, due: at });
        }
        worker.join().unwrap();
    }

    #[test]
    fn a_command_posted_right_before_the_bound_rises_lands_on_its_frame() {
        const ROUNDS: u64 = if cfg!(miri) { 4 } else { 100_000 };
        for offline in [true, false] {
            let channel = Arc::new(Channel::new());
            channel.offline.store(offline, Ordering::Release);
            let shared = Arc::clone(&channel);
            let worker = thread::spawn(move || {
                State {
                    program: Program::without_orchestra(),
                    sample_rate: 48_000,
                    channel: shared,
                    engine: None,
                    pending: Vec::new(),
                    pending_bytes: 0,
                    write: 0,
                }
                .run();
            });
            // Live, the bound is a runway past the frame read last.
            let runway = if offline { 0 } else { MIN_RUNWAY_FRAMES };
            for round in 1..=ROUNDS {
                let frame = round * CALLBACK_BLOCK;
                // A cut confirms the frame it ran at, and a note needs the
                // native library. So a cut on the last frame the raised
                // bound covers stands for the note.
                let due = frame + runway - 1;
                channel.clean_from[0].store(UNCONFIRMED, Ordering::Relaxed);
                assert!(channel.post(Command::Cut { due, mask: 0 }));
                if offline {
                    channel.wanted.store(frame, Ordering::Release);
                } else {
                    channel.read.store(frame, Ordering::Release);
                }
                while channel.written.load(Ordering::Acquire) <= due {
                    thread::yield_now();
                }
                assert_eq!(channel.clean_from[0].load(Ordering::Acquire), due);
            }
            channel.stopping.store(true, Ordering::Release);
            worker.join().unwrap();
        }
    }

    #[test]
    fn the_cancellation_queue_keeps_order_across_threads_and_wraps() {
        let channel = Arc::new(Channel::new());
        for id in 0..CANCEL_CAPACITY as u64 {
            assert!(channel.cancel(id));
        }
        assert!(!channel.cancel(u64::MAX));
        let tail = channel.cancellation_tail.load(Ordering::Acquire);
        for id in 0..CANCEL_CAPACITY as u64 {
            let cancellation = channel.take_cancellation(tail).unwrap();
            assert_eq!(cancellation.id, id);
            assert_eq!(cancellation.through, 0);
        }
        assert!(channel.take_cancellation(tail).is_none());
        let writer = Arc::clone(&channel);
        let count = CANCEL_CAPACITY as u64 + 17;
        let worker = thread::spawn(move || {
            for id in 0..count {
                while !writer.cancel(id) {
                    thread::yield_now();
                }
            }
        });
        for id in 0..count {
            let cancellation = loop {
                let tail = channel.cancellation_tail.load(Ordering::Acquire);
                if let Some(cancellation) = channel.take_cancellation(tail) {
                    break cancellation;
                }
                thread::yield_now();
            };
            assert_eq!(cancellation.id, id);
        }
        worker.join().unwrap();
    }

    #[test]
    fn a_dead_worker_is_reported_to_the_callback() {
        let channel = Arc::new(Channel::new());
        let worker = {
            let channel = Arc::clone(&channel);
            thread::spawn(move || {
                let _exit = Exit(channel);
            })
        };
        worker.join().unwrap();
        assert!(channel.dead.load(Ordering::Acquire));
    }

    /// A reader over a shared ring whose worker only answers the stop
    /// flag. Each test confirms restarts the way the real worker does.
    fn reader(channel: &Arc<Channel>) -> Insert {
        let watched = Arc::clone(channel);
        let worker = thread::Builder::new()
            .name("mask".into())
            .spawn(move || {
                while !watched.stopping.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(1));
                }
            })
            .unwrap();
        Insert {
            key: InsertKey::default(),
            channel: Arc::clone(channel),
            counters: Arc::new(Counters::default()),
            worker: Some(worker),
            frame: 0,
            masks: [None; COMMAND_CAPACITY as usize],
            stale: [0; RING_FRAMES as usize / 64],
            reset_at: None,
            pending_control: None,
            failed: false,
        }
    }

    fn confirm_cut(channel: &Channel, mask: usize, frame: u64) {
        channel.clean_from[mask].store(frame, Ordering::Release);
    }

    /// One block of the reader: the left channel collect hands back.
    fn play_block(insert: &mut Insert) -> Vec<f32> {
        let mut left = [0.0; CALLBACK_BLOCK as usize];
        let mut right = [0.0; CALLBACK_BLOCK as usize];
        insert.process(&mut left, &mut right);
        left.into()
    }

    fn take_control(channel: &Channel, due: u64, reset: bool) -> usize {
        match channel.drain().unwrap() {
            Command::Reset { due: actual, mask } if reset => {
                assert_eq!(actual, due);
                mask
            }
            Command::Cut { due: actual, mask } if !reset => {
                assert_eq!(actual, due);
                mask
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn a_reset_with_no_orchestra_left_confirms_its_restart() {
        let channel = Arc::new(Channel::new());
        fill_ahead(&channel, 1024);
        let mut insert = reader(&channel);
        insert.reset();
        let mask = take_control(&channel, 0, true);
        confirm_cut(&channel, mask, 384);
        for _ in 0..3 {
            assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
        }
        assert_eq!(play_block(&mut insert).first(), Some(&384.0));
        insert.reset();
        let mask = take_control(&channel, 512, true);
        confirm_cut(&channel, mask, 512);
        assert_eq!(play_block(&mut insert).first(), Some(&512.0));
    }

    #[test]
    fn a_reset_between_a_cut_and_its_retire_still_retires() {
        let channel = Arc::new(Channel::new());
        fill_ahead(&channel, 2048);
        let mut insert = reader(&channel);
        insert.reset();
        let mask = take_control(&channel, 0, true);
        confirm_cut(&channel, mask, 384);
        for due in [128, 256] {
            assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
            insert.reset();
            let mask = take_control(&channel, due, true);
            confirm_cut(&channel, mask, 384);
        }
        assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
        assert_eq!(play_block(&mut insert).first(), Some(&384.0));
    }

    #[test]
    fn a_refused_reset_keeps_the_confirmation_its_mask_owes() {
        let channel = Arc::new(Channel::new());
        fill_ahead(&channel, 2048);
        let mut insert = reader(&channel);
        insert.reset();
        let mask = take_control(&channel, 0, true);
        confirm_cut(&channel, mask, 384);
        assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
        for at in 0..COMMAND_CAPACITY {
            assert!(channel.post(Command::Score { id: at, due: at }));
        }
        insert.reset();
        assert_eq!(insert.counters.refused_commands.load(Ordering::Relaxed), 1);
        assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
        while channel.drain().is_some() {}
        assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
        let mask = take_control(&channel, 128, true);
        confirm_cut(&channel, mask, 512);
        assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
        assert_eq!(play_block(&mut insert).first(), Some(&512.0));
    }

    #[test]
    fn each_cut_waits_for_its_own_restart_before_unmasking() {
        for (first, second) in [(32, 64), (64, 32)] {
            for confirmed_before_second in [false, true] {
                let channel = Arc::new(Channel::new());
                fill_ahead(&channel, 1024);
                let mut insert = reader(&channel);
                insert.cut_notes(first);
                let first_mask = take_control(&channel, first.into(), false);
                if confirmed_before_second {
                    confirm_cut(&channel, first_mask, 384);
                }
                insert.cut_notes(second);
                let second_mask = take_control(&channel, second.into(), false);
                if !confirmed_before_second {
                    confirm_cut(&channel, first_mask, 384);
                }
                for _ in 0..4 {
                    let played = play_block(&mut insert);
                    let start = insert.frame - CALLBACK_BLOCK;
                    for (offset, sample) in played.into_iter().enumerate() {
                        if start + offset as u64 >= u64::from(first.min(second)) {
                            assert_eq!(sample, 0.0);
                        }
                    }
                }
                confirm_cut(&channel, second_mask, 768);
                for _ in 0..2 {
                    assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
                }
                assert_eq!(play_block(&mut insert).first(), Some(&768.0));
            }
        }
        // Confirmed intervals preserve clean audio between cuts, also
        // across the audio ring's wrap and after a slot is reused.
        let channel = Arc::new(Channel::new());
        let mut insert = reader(&channel);
        insert.frame = RING_FRAMES - CALLBACK_BLOCK;
        for at in insert.frame..insert.frame + 4 * CALLBACK_BLOCK {
            channel.publish([1.0, 1.0], at);
        }
        for (from, end) in [(32, 64), (192, 256), (288, 320)] {
            insert.cut_notes(from);
            let mask = take_control(&channel, insert.frame + u64::from(from), false);
            confirm_cut(&channel, mask, insert.frame + end);
        }
        for block in 0..4 {
            for (index, sample) in play_block(&mut insert).into_iter().enumerate() {
                let frame = block * CALLBACK_BLOCK + index as u64;
                let stale = (32..64).contains(&frame)
                    || (192..256).contains(&frame)
                    || (288..320).contains(&frame);
                assert_eq!(sample, if stale { 0.0 } else { 1.0 });
            }
        }
        for at in insert.frame..insert.frame + RING_FRAMES {
            channel.publish([1.0, 1.0], at);
        }
        for _ in 0..RING_FRAMES / CALLBACK_BLOCK {
            assert!(play_block(&mut insert).iter().all(|v| *v == 1.0));
        }
    }

    #[test]
    fn a_refused_first_cut_or_reset_recovers_when_the_queue_drains() {
        for reset in [false, true] {
            let channel = Arc::new(Channel::new());
            fill_ahead(&channel, 1024);
            let mut insert = reader(&channel);
            for at in 0..COMMAND_CAPACITY {
                assert!(channel.post(Command::Score { id: at, due: at }));
            }
            if reset {
                insert.reset();
            } else {
                insert.cut_notes(0);
            }
            assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
            while channel.drain().is_some() {}
            insert.score(999, 512);
            let mask = take_control(&channel, 0, reset);
            assert_eq!(channel.drain(), Some(Command::Score { id: 999, due: 640 }));
            confirm_cut(&channel, mask, 384);
            for _ in 0..2 {
                assert!(play_block(&mut insert).iter().all(|v| *v == 0.0));
            }
            assert_eq!(play_block(&mut insert).first(), Some(&384.0));
        }
        let channel = Arc::new(Channel::new());
        let mut insert = reader(&channel);
        for _ in 0..2 {
            for frame in 0..COMMAND_CAPACITY {
                insert.cut_notes((frame + RING_FRAMES) as u32);
            }
            insert.reset();
            while let Some(command) = channel.drain() {
                match command {
                    Command::Cut { due, mask } | Command::Reset { due, mask } => {
                        confirm_cut(&channel, mask, due);
                    }
                    Command::Score { .. } => unreachable!(),
                }
            }
            insert.retry_control();
            while let Some(command) = channel.drain() {
                if let Command::Reset { due, mask } = command {
                    confirm_cut(&channel, mask, due);
                }
            }
            insert.reset_at = None;
        }
        insert.flush(0);
        assert!(insert.pending_control.is_none());
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn a_full_command_queue_does_not_turn_cancellation_into_a_reset() {
        let channel = Arc::new(Channel::new());
        let program = Program::without_orchestra();
        let mut state = State {
            program: program.clone(),
            sample_rate: 48_000,
            channel: Arc::clone(&channel),
            engine: None,
            pending: Vec::new(),
            pending_bytes: 0,
            write: 0,
        };
        let mut cancelled = Vec::new();
        let mut ids = Vec::new();
        for frame in 0..COMMAND_CAPACITY + 2 {
            if frame == 2 {
                state.drain_commands(&mut cancelled);
            }
            let id = program
                .prepare(crate::Event {
                    instrument: crate::Instrument::Named("probe".into()),
                    p4: 440.0,
                    p5: 0.2,
                    p6: String::new(),
                    duration: 0.01,
                })
                .unwrap();
            ids.push(id);
            assert!(channel.post(Command::Score {
                id,
                due: frame * 128
            }));
        }
        let mut insert = reader(&channel);
        assert!(rustel_audio::tripwire::allocator_is_armed());
        let before = rustel_audio::tripwire::Violations::capture();
        // The first note precedes the takeover. Retract every later onset,
        // including more IDs than the full command queue could have held.
        rustel_audio::tripwire::audio_scope(|| {
            for id in &ids[1..] {
                insert.cancel_score(*id);
            }
        });
        assert!(
            rustel_audio::tripwire::Violations::capture()
                .since(before)
                .clean()
        );
        assert!(insert.pending_control.is_none());
        assert!(insert.masks.iter().all(Option::is_none));
        assert_eq!(insert.counters.refused_commands.load(Ordering::Relaxed), 0);
        state.drain_commands(&mut cancelled);
        let held: Vec<_> = state
            .pending
            .iter()
            .map(|event| {
                let Action::Score { id, .. } = event.action else {
                    panic!("cancellation posted a cut")
                };
                id
            })
            .collect();
        assert_eq!(held, [ids[0]], "the pre-takeover note must survive");
        let bytes: usize = state
            .pending
            .iter()
            .map(|event| match &event.action {
                Action::Score { score, .. } => score.len(),
                Action::Cut => 0,
            })
            .sum();
        assert_eq!(state.pending_bytes, bytes);
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow to fill the schedule under Miri")]
    fn cancellation_frees_schedule_capacity_before_admitting_replacement_scores() {
        let channel = Arc::new(Channel::new());
        let program = Program::without_orchestra();
        let replacement = program
            .prepare(crate::Event {
                instrument: crate::Instrument::Named("probe".into()),
                p4: 880.0,
                p5: 0.2,
                p6: String::new(),
                duration: 0.01,
            })
            .unwrap();
        let mut state = State {
            program,
            sample_rate: 48_000,
            channel: Arc::clone(&channel),
            engine: None,
            pending: Vec::new(),
            pending_bytes: 0,
            write: 0,
        };
        for index in 0..crate::MAX_EVENTS as u64 {
            state
                .schedule(
                    640,
                    Action::Score {
                        id: replacement + index + 1,
                        submitted: index,
                        score: "old".into(),
                    },
                )
                .unwrap();
        }
        // The cursors name the commands that built the full pending schedule.
        channel
            .command_head
            .store(crate::MAX_EVENTS as u64, Ordering::Relaxed);
        channel
            .command_tail
            .store(crate::MAX_EVENTS as u64, Ordering::Relaxed);
        assert!(channel.cancel(replacement + 1));
        assert!(channel.post(Command::Score {
            id: replacement,
            due: 640
        }));
        state.drain_commands(&mut Vec::new());
        assert_eq!(state.pending.len(), crate::MAX_EVENTS);
        assert!(
            state
                .pending
                .iter()
                .any(|event| matches!(event.action, Action::Score { id, .. } if id == replacement))
        );
        assert!(
            state.program.take_errors().is_empty(),
            "replacement score must not hit the schedule ceiling"
        );
    }

    #[test]
    fn an_unknown_cancellation_cannot_retract_a_later_submission() {
        let channel = Arc::new(Channel::new());
        let program = Program::without_orchestra();
        let id = program
            .prepare(crate::Event {
                instrument: crate::Instrument::Named("probe".into()),
                p4: 440.0,
                p5: 0.2,
                p6: String::new(),
                duration: 0.01,
            })
            .unwrap();
        assert!(channel.cancel(id));
        assert!(channel.post(Command::Score { id, due: 640 }));
        let mut state = State {
            program,
            sample_rate: 48_000,
            channel: Arc::clone(&channel),
            engine: None,
            pending: Vec::new(),
            pending_bytes: 0,
            write: 0,
        };
        let mut cancelled = Vec::new();
        state.drain_commands(&mut cancelled);
        assert_eq!(state.pending.len(), 1);
        assert!(channel.cancel(id));
        state.drain_commands(&mut cancelled);
        assert!(state.pending.is_empty());
        assert_eq!(state.pending_bytes, 0);
    }

    #[test]
    fn the_first_runway_of_an_orchestra_reads_dry_without_starving() {
        let channel = Arc::new(Channel::new());
        // This worker renders nothing, so each frame reads dry.
        let mut insert = reader(&channel);
        let starved = |insert: &Insert| insert.counters.starved_frames.load(Ordering::Relaxed);
        for _ in 0..RUNWAY_FRAMES / CALLBACK_BLOCK {
            play_block(&mut insert);
        }
        assert_eq!(starved(&insert), 0);
        play_block(&mut insert);
        assert_eq!(starved(&insert), CALLBACK_BLOCK);
        // A note after a cut starts the next orchestra at its own frame, one
        // block past the reader here. The silence before it does not count.
        channel
            .started
            .store(insert.frame + CALLBACK_BLOCK, Ordering::Release);
        for _ in 0..=RUNWAY_FRAMES / CALLBACK_BLOCK {
            play_block(&mut insert);
        }
        assert_eq!(starved(&insert), CALLBACK_BLOCK);
        play_block(&mut insert);
        assert_eq!(starved(&insert), 2 * CALLBACK_BLOCK);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri does not load the native library")]
    fn a_note_that_starts_an_orchestra_marks_its_frame() {
        let channel = Arc::new(Channel::new());
        let mut state = State {
            program: Program::without_orchestra(),
            sample_rate: 48_000,
            channel: Arc::clone(&channel),
            engine: None,
            pending: Vec::new(),
            pending_bytes: 0,
            write: 300,
        };
        let score = "i \"triangle\" 0 0.1 440 0.2 \"\"\n".to_owned();
        // The note sounds past the frontier, and the frames before it are
        // silent.
        state
            .schedule(
                900,
                Action::Score {
                    id: 1,
                    submitted: 0,
                    score,
                },
            )
            .unwrap();
        // The mark comes before the compile, which fails with no library.
        state.step();
        assert_eq!(channel.started.load(Ordering::Acquire), 900);
    }

    #[test]
    fn refused_controls_merge_into_one_with_the_earliest_due() {
        // Each case refuses two controls, a cut as its frames or a reset as
        // None, and names the one control that posts once the queue drains.
        for (first, second, reset, due) in [
            (Some(64), Some(32), false, 32),
            (Some(32), Some(64), false, 32),
            (Some(64), None, true, 0),
            (None, Some(64), true, 0),
        ] {
            let channel = Arc::new(Channel::new());
            let mut insert = reader(&channel);
            for at in 0..COMMAND_CAPACITY {
                assert!(channel.post(Command::Score { id: at, due: at }));
            }
            for control in [first, second] {
                match control {
                    Some(frames) => insert.cut_notes(frames),
                    None => insert.reset(),
                }
            }
            assert_eq!(insert.counters.refused_commands.load(Ordering::Relaxed), 2);
            while channel.drain().is_some() {}
            insert.score(999, 512);
            take_control(&channel, due, reset);
            assert_eq!(channel.drain(), Some(Command::Score { id: 999, due: 512 }));
            assert_eq!(channel.drain(), None);
        }
    }
}
