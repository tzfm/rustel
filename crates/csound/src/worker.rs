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
/// The runway before any note has said its lead: two blocks keep the ring
/// ahead of the callback from the first frame. A lead shorter than this,
/// which only a very small host buffer gives, can arrive after the worker
/// passed its frame by the difference, and the note starts that late.
const MIN_RUNWAY_FRAMES: u64 = 256;
/// Ring capacity in frames: the runway, the block in flight, and room for a
/// stop between publishes. A power of two keeps the wrap cheap.
const RING_FRAMES: u64 = 4096;
/// Commands the callback may post before the worker drains them.
const COMMAND_CAPACITY: u64 = 256;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Command {
    /// Plays the prepared score event `id` at frame `due`.
    Score { id: u64, due: u64 },
    /// Ends every note, also those the orchestra scheduled itself or the
    /// host still had waiting, and restarts the orchestra at frame `due`.
    Cut { due: u64 },
    /// Drops every scheduled event and restarts the orchestra at `due`.
    Reset { due: u64 },
}

/// What the callback and the worker share: a ring of interleaved stereo
/// frames the worker fills and the callback drains, a queue of commands the
/// other way, and the indices that hand both over. Every index only grows,
/// so each slot belongs to exactly one side at a time.
struct Channel {
    commands: UnsafeCell<Box<[Command; COMMAND_CAPACITY as usize]>>,
    command_head: AtomicU64,
    command_tail: AtomicU64,
    samples: UnsafeCell<Box<[f32; RING_FRAMES as usize * 2]>>,
    written: AtomicU64,
    read: AtomicU64,
    /// The first frame rendered after the last restart. The callback zeroes
    /// the frames between a cut and this one, which the orchestra before
    /// the restart already rendered ahead.
    clean_from: AtomicU64,
    /// The largest note lead the callback has posted.
    lead: AtomicU64,
    stopping: AtomicBool,
    dead: AtomicBool,
}

// The callback owns a command slot and a ring frame until it publishes the
// index that hands them over; the worker owns them from there until the
// index that takes them back. Release stores and acquire loads of the
// indices order the slot writes before the slot reads.
unsafe impl Sync for Channel {}

impl Channel {
    fn new() -> Self {
        Self {
            commands: UnsafeCell::new(Box::new(
                [Command::Reset { due: 0 }; COMMAND_CAPACITY as usize],
            )),
            command_head: AtomicU64::new(0),
            command_tail: AtomicU64::new(0),
            samples: UnsafeCell::new(Box::new([0.0; RING_FRAMES as usize * 2])),
            written: AtomicU64::new(0),
            read: AtomicU64::new(0),
            clean_from: AtomicU64::new(0),
            lead: AtomicU64::new(0),
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

    /// Stores one frame and publishes it. The worker calls this for frame
    /// `at`, then never touches that slot again until the ring wraps past
    /// the frame the callback has read.
    fn publish(&self, frame: [f32; 2], at: u64) {
        let slot = (at % RING_FRAMES) as usize * 2;
        unsafe {
            let samples = &mut *self.samples.get();
            samples[slot] = frame[0];
            samples[slot + 1] = frame[1];
        }
        self.written.store(at + 1, Ordering::Release);
    }

    /// Takes one published frame.
    fn take(&self, at: u64) -> [f32; 2] {
        let slot = (at % RING_FRAMES) as usize * 2;
        unsafe {
            let samples = &*self.samples.get();
            [samples[slot], samples[slot + 1]]
        }
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
    /// Ring frames from here stay silent until the worker's next clean
    /// frame: the orchestra before a cut rendered them ahead already.
    mask_from: Option<u64>,
    /// The clean frontier when the mask was armed. The worker confirms a
    /// restart by moving it; until then every frame the mask covers is
    /// stale, also the ones the worker has not rendered yet.
    clean_at_post: u64,
    /// The frame of the last reset posted. A stopped output repeats its
    /// reset each block, and one post per frame is the whole ask.
    reset_at: Option<u64>,
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
                mask_from: None,
                clean_at_post: 0,
                reset_at: None,
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

    /// Marks ring frames from `due` as stale. The worker renders past the
    /// reader, so the frames a cut ends may already sit in the ring.
    fn arm_mask(&mut self, due: u64) {
        self.mask_from = Some(self.mask_from.map_or(due, |held| held.min(due)));
        self.clean_at_post = self.channel.clean_from.load(Ordering::Acquire);
    }

    fn stopped(&self) -> bool {
        self.channel.dead.load(Ordering::Acquire) || self.channel.stopping.load(Ordering::Acquire)
    }

    fn post(&mut self, command: Command) {
        if self.failed {
            return;
        }
        if self.stopped() {
            self.failed = true;
            return;
        }
        if !self.channel.post(command) {
            self.counters
                .refused_commands
                .fetch_add(1, Ordering::Relaxed);
        }
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
        self.post(Command::Score { id, due });
    }

    fn cut_notes(&mut self, frames: u32) {
        let due = self.frame.saturating_add(u64::from(frames));
        self.arm_mask(due);
        self.post(Command::Cut { due });
    }

    fn reset(&mut self) {
        let due = self.frame;
        if self.reset_at == Some(due) {
            return;
        }
        self.reset_at = Some(due);
        self.arm_mask(due);
        self.post(Command::Reset { due });
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
        let start = self.frame;
        // One look per block: frames published later belong to the future.
        let written = self.channel.written.load(Ordering::Acquire);
        let clean_from = self.channel.clean_from.load(Ordering::Acquire);
        // A restart the worker confirmed after the mask was armed.
        let restarted = clean_from > self.clean_at_post;
        let mask_from = self.mask_from;
        let mut starved = 0u64;
        for (index, (out_left, out_right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let frame = start + index as u64;
            // Until the worker confirms its restart, every frame the mask
            // covers is stale, also one the worker has not rendered yet.
            if frame < written
                && !mask_from
                    .is_some_and(|from| frame >= from && (!restarted || frame < clean_from))
            {
                let [sample_left, sample_right] = self.channel.take(frame);
                *out_left = sample_left;
                *out_right = sample_right;
            } else {
                if frame >= written {
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
        // The worker confirmed its restart and rendered past the mask.
        if mask_from.is_some() && restarted && self.frame >= clean_from {
            self.mask_from = None;
        }
        if self.stopped() {
            self.failed = true;
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
    Score(String),
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
        loop {
            if self.channel.stopping.load(Ordering::Acquire) {
                return;
            }
            while let Some(command) = self.channel.drain() {
                match command {
                    Command::Score { id, due } => {
                        if let Some(score) = self.program.take(id)
                            && let Err(error) = self.schedule(due, Action::Score(score))
                        {
                            self.program.report_error(error);
                        }
                    }
                    Command::Cut { due } => {
                        if let Err(error) = self.schedule(due, Action::Cut) {
                            self.program.report_error(error);
                        }
                    }
                    Command::Reset { due } => {
                        self.pending.clear();
                        self.pending_bytes = 0;
                        // A stopped output repeats its reset each block.
                        // With no orchestra running there is nothing to
                        // end, and the repeated resets cost nothing.
                        if self.engine.is_some() {
                            let _ = self.schedule(due, Action::Cut);
                        }
                    }
                }
            }
            let lead = self.channel.lead.load(Ordering::Acquire);
            let runway = lead
                .saturating_sub(CALLBACK_BLOCK)
                .clamp(MIN_RUNWAY_FRAMES, RUNWAY_FRAMES);
            let target = self
                .channel
                .read
                .load(Ordering::Acquire)
                .saturating_add(runway);
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

    fn schedule(&mut self, due: u64, action: Action) -> Result<(), String> {
        let bytes = match &action {
            Action::Score(score) => score.len(),
            Action::Cut => 0,
        };
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
            && self
                .pending
                .iter()
                .any(|event| matches!(event.action, Action::Score(_)))
        {
            // A cut took the orchestra, and a note has asked for it again.
            // The rest after the cut pays for the compile: the insert is
            // silent now, and the note has its schedule lead. This runs on
            // the worker, where a slow compile costs a dry ring, not a
            // dropout.
            match Engine::new(&self.program.sources, self.sample_rate) {
                Ok(engine) => self.engine = Some(engine),
                Err(error) => {
                    self.program.report_error(error);
                    self.pending.clear();
                    self.pending_bytes = 0;
                }
            }
        }
        while self
            .pending
            .first()
            .is_some_and(|event| event.frame <= self.write)
        {
            match self.pending.remove(0).action {
                Action::Score(score) => {
                    self.pending_bytes -= score.len();
                    if let Some(engine) = self.engine.as_mut()
                        && let Err(error) = engine.score(&score)
                    {
                        self.fail(error);
                    }
                }
                // The restart ends each note, also those the orchestra
                // scheduled itself, and the globals start over. A host
                // note still waiting here was retired before it sounded,
                // so it goes too: without this drop, the next step's
                // rebuild would give it an orchestra back. The store must
                // move past every snapshot the reader took before it
                // posted the command, so a cut that fires before the
                // first rendered frame still counts as a restart.
                Action::Cut => {
                    let _ = self.engine.take();
                    let dropped: usize = self
                        .pending
                        .iter()
                        .map(|event| match &event.action {
                            Action::Score(score) => score.len(),
                            Action::Cut => 0,
                        })
                        .sum();
                    self.pending_bytes -= dropped;
                    self.pending
                        .retain(|event| matches!(event.action, Action::Cut));
                    let clean = self
                        .write
                        .max(self.channel.clean_from.load(Ordering::Relaxed) + 1);
                    self.channel.clean_from.store(clean, Ordering::Release);
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

    /// Keeps rendering after an error: the event list goes, the orchestra
    /// gives way to silence, and the next note starts a fresh one.
    fn fail(&mut self, error: String) {
        self.program.report_error(error);
        self.pending.clear();
        self.pending_bytes = 0;
        self.engine = None;
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
    fn the_ring_hands_frames_to_the_reader_in_order() {
        let channel = Channel::new();
        let shared = Arc::new(channel);
        let writer = Arc::clone(&shared);
        let worker = thread::spawn(move || fill_ahead(&writer, 600));
        worker.join().unwrap();
        // The reader takes blocks of 128, the way the callback does.
        for block in 0..4u64 {
            for index in 0..128u64 {
                let frame = block * 128 + index;
                let [left, right] = shared.take(frame);
                assert_eq!(left, frame as f32);
                assert_eq!(right, -(frame as f32));
            }
            shared.read.store((block + 1) * 128, Ordering::Release);
        }
    }

    #[test]
    fn the_command_queue_keeps_order_and_reports_a_full_queue() {
        let channel = Channel::new();
        for at in 0..COMMAND_CAPACITY {
            assert!(channel.post(Command::Score { id: at, due: at }));
        }
        assert!(!channel.post(Command::Cut { due: 0 }));
        for at in 0..COMMAND_CAPACITY {
            assert_eq!(channel.drain(), Some(Command::Score { id: at, due: at }));
        }
        assert_eq!(channel.drain(), None);
        assert!(channel.post(Command::Cut { due: 7 }));
        assert_eq!(channel.drain(), Some(Command::Cut { due: 7 }));
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
}
