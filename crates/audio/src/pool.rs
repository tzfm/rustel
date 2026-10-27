//! Plugin chains on several cores.
//!
//! The callback hands the block calls of a plugin chain to a worker thread
//! and joins them in the same sub-block. One bus in one sub-block is one
//! job: the instrument of the bus renders a block on silence, or its effect
//! stages process the block of the bus in signal order. The plugins of one
//! chain run in order on one thread, and the chains of a block share the
//! workers and the callback. A job ends in its own sub-block, so the pool
//! adds no latency, and an insert that keeps the float mode gives the
//! samples of the serial engine.
//!
//! ```text
//!   callback   post A B C   voices   join A   run C, join C   join B
//!   worker 0          [ A ]
//!   worker 1             [           B           ]
//! ```
//!
//! A job slot belongs to one thread at a time. Each kind of job has a
//! posted word and a done word, with one bit for each bus:
//!
//! - post: the callback writes the job, then sets its posted bit.
//! - claim: the thread whose RMW clears the posted bit owns the job.
//! - done: the owner sets the done bit after its last write.
//! - join: the callback reads the done bit, clears it and reads the job.
//!
//! A worker with no job leaves the idle count, sets its parked bit and looks
//! for a post once more, then waits on a wake of its own. The callback sets
//! the posted bit, then reads the idle count and the parked bits. All of
//! these are SeqCst, so either the worker sees the post or the callback sees
//! the parked worker and signals it. A signal before the wait stays set.
//!
//! The callback waits only for a worker in its own real-time class. A worker
//! that does not get the class ends before its first job.
//!
//! A gate keeps a light set on the callback. It opens when the serial load,
//! the load of the callback with each job on it, passes half of a buffer on
//! average or 80% of one buffer. The 80% test also runs after each sub-block
//! on the buffer so far, so a heavy start posts its jobs from its second
//! sub-block. The gate closes when the average falls below 35%.

use std::any::Any;
use std::cell::UnsafeCell;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(all(windows, not(miri)))]
use crate::mmcss::WorkerWake;

use crate::insert::{EFFECT_CHAIN, MAX_INSERT_BUSES, NO_STAGE, OrbitInsert};
use crate::reverb::REVERB_BLOCK;
use crate::scalar::{
    InsertMix, effect_stage_wakes, enable_denormal_flush, fade_effect_stage, ordered_stages,
    render_leaving_insert, settle_insert_block,
};
use crate::tripwire;

/// The workers of one pool at most.
const MAX_PLUGIN_WORKERS: usize = 8;
/// An instrument job and an effect job for each bus.
const JOB_SLOTS: usize = 2 * MAX_INSERT_BUSES;
/// A shorter sub-block stays on the callback: the split at a new instrument
/// key gives such a sub-block.
const MIN_POOLED_FRAMES: usize = 32;
/// The gate opens when the average serial load passes the first value, or
/// one buffer passes the second, also the part of a buffer so far. It closes
/// when the average falls below the third after a buffer below the second.
/// In basis points of the period of a buffer.
const GATE_OPEN_LOAD: u64 = 5_000;
const GATE_PEAK_LOAD: u64 = 8_000;
const GATE_CLOSE_LOAD: u64 = 3_500;
/// How often the producer reads the power mode while a worker takes jobs,
/// in nanoseconds. On Linux a read opens three files.
const POWER_MODE_READ_NANOS: u64 = 1_000_000_000;
/// A job is late when the callback waited for it longer than this, in
/// nanoseconds, and the system did not run its worker for longer than this
/// in the job: it ran another thread, or it demoted the worker.
const LATE_WAIT_NANOS: u64 = 500_000;
/// The first cool-down of a late worker, and the longest, in nanoseconds.
const COOL_DOWN_NANOS: u64 = 1_000_000_000;
const MAX_COOL_DOWN_NANOS: u64 = 30_000_000_000;
/// A worker that took jobs this long past its last cool-down with no late
/// job starts again at the first cool-down, in nanoseconds.
const COOL_DOWN_RESET_NANOS: u64 = 10_000_000_000;
/// The stack a worker writes at its start, so its first job finds the pages
/// in memory.
const STACK_TOUCH_BYTES: usize = 256 * 1024;

/// The two jobs of a bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobKind {
    /// The instrument renders one block on silence.
    Instrument,
    /// The effect stages process the block of the bus in signal order.
    Effect,
}

impl JobKind {
    const fn index(self) -> usize {
        self as usize
    }

    const fn slot(self, bus: usize) -> usize {
        self.index() * MAX_INSERT_BUSES + bus
    }

    /// The kind of the job in `slot`.
    const fn of(slot: usize) -> Self {
        if slot < MAX_INSERT_BUSES {
            Self::Instrument
        } else {
            Self::Effect
        }
    }
}

/// The work of one bus in one sub-block, and the boxes that do it.
pub(crate) struct Job {
    /// The frames of the block, 1 to 128.
    pub(crate) frames: usize,
    /// The input of an effect job, and the output of each job.
    pub(crate) block: [[f32; REVERB_BLOCK]; 2],
    /// The boxes of the bus while the job is out. An instrument job holds
    /// its instrument at 0. An effect job holds the box of each stage at the
    /// number of the stage.
    pub(crate) boxes: [Option<Box<dyn OrbitInsert>>; EFFECT_CHAIN],
    /// The copies in the leaving places of the bus that fade while the job
    /// is out, at the places of `boxes`.
    pub(crate) leaving: [Option<Box<dyn OrbitInsert>>; EFFECT_CHAIN],
    /// The levels of the copies of each effect stage.
    pub(crate) mixes: [InsertMix; EFFECT_CHAIN],
    /// The input of an effect stage whose levels move.
    pub(crate) dry: [[f32; REVERB_BLOCK]; 2],
    /// The block of a leaving copy: the output of a leaving instrument on
    /// silence, or of a leaving effect on `dry`.
    pub(crate) leaving_block: [[f32; REVERB_BLOCK]; 2],
    /// The frames each effect stage had no input and no output.
    pub(crate) idle: [u32; EFFECT_CHAIN],
    /// The effect stages that run, in signal order, with [`NO_STAGE`] after
    /// the last.
    pub(crate) order: [u8; EFFECT_CHAIN],
    /// The place in `order` of the first stage that woke on the callback,
    /// and whether its input was fed.
    pub(crate) first: usize,
    pub(crate) fed: bool,
    /// The idle frames after which an effect stage sleeps.
    pub(crate) idle_limit: u32,
    /// The time of the run in nanoseconds, and the worker that ran it:
    /// `None` for the callback.
    run_nanos: u64,
    worker: Option<usize>,
    /// The nanoseconds of the run in which the system did not run the
    /// worker. 0 for a run of [`LATE_WAIT_NANOS`] or less, and where the
    /// system gives no thread time.
    lost_nanos: u64,
    /// The payload of a panic in the run. The join raises it again.
    panic: Option<Box<dyn Any + Send>>,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            frames: 0,
            block: [[0.0; REVERB_BLOCK]; 2],
            boxes: Default::default(),
            leaving: Default::default(),
            mixes: [InsertMix::default(); EFFECT_CHAIN],
            dry: [[0.0; REVERB_BLOCK]; 2],
            leaving_block: [[0.0; REVERB_BLOCK]; 2],
            idle: [0; EFFECT_CHAIN],
            order: [NO_STAGE; EFFECT_CHAIN],
            first: 0,
            fed: false,
            idle_limit: 0,
            run_nanos: 0,
            worker: None,
            lost_nanos: 0,
            panic: None,
        }
    }
}

/// One job and the cost of its bus. A slot fills its own cache lines, so
/// two threads that write two jobs share no line.
#[derive(Default)]
#[repr(align(128))]
struct Slot {
    job: UnsafeCell<Job>,
    /// The run time of the jobs of this slot in nanoseconds: an average that
    /// weighs each new run by 1/4. 0 before the first run.
    cost: AtomicU32,
}

/// A value in a cell of 128 bytes, apart from the atomics other threads
/// write.
#[derive(Default)]
#[repr(align(128))]
struct Padded<T>(T);

/// The cool-down of one worker, in nanoseconds of the pool clock.
#[derive(Default)]
struct CoolDown {
    /// The worker takes no job before this time.
    until: AtomicU64,
    /// The length of its last cool-down.
    length: AtomicU64,
}

/// What the callback and the workers of one pool share.
pub(crate) struct PoolShared {
    slots: Box<[Slot]>,
    /// The jobs posted and not claimed, one bit for each bus, for each kind.
    posted: [Padded<AtomicU64>; 2],
    /// The jobs a thread ran and the callback did not join yet.
    done: [Padded<AtomicU64>; 2],
    /// The workers that wait on their wake, one bit for each.
    parked: Padded<AtomicU64>,
    /// The workers that are awake and have no job.
    idle: Padded<AtomicUsize>,
    wakes: Box<[WorkerWake]>,
    cool_downs: Box<[Padded<CoolDown>]>,
    /// The workers that have their real-time class and take jobs.
    ready: AtomicUsize,
    /// Set when the pool stops: each worker ends.
    closing: AtomicBool,
    /// Set from the first post of a buffer of the callback to its end. A
    /// worker spins for its next job only inside the buffer.
    in_buffer: AtomicBool,
    /// Set in a power-saving mode: a worker parks as soon as it has no job.
    power_saving: AtomicBool,
    /// The buffer of the stream the workers serve: its frames in the high
    /// 32 bits and its rate in the low 32 bits. The rate sets how long a
    /// worker spins. On macOS a worker that took its class for another
    /// buffer takes it again between jobs.
    period: AtomicU64,
    /// Set while a handle holds the pool, so one callback posts.
    attached: AtomicBool,
    /// Set while jobs post. The handle moves it at the end of each buffer,
    /// and opens it after a sub-block. The gate lives here, so a new handle
    /// after a stop or a reopen keeps it.
    gate_open: AtomicBool,
    /// The serial load of the callback in basis points of its period: an
    /// average that weighs each buffer by 1/4.
    serial_load: AtomicU64,
    /// Set where the ticks of [`thread_ticks`] count at a fixed rate, so the
    /// pool finds a late job. Off on Windows on Arm, where a worker never
    /// sits out.
    late_rule: bool,
    /// The ticks of [`thread_ticks`] in a microsecond: the most a worker had
    /// in a run past [`LATE_WAIT_NANOS`], a run the system did not cut. 0
    /// before such a run.
    ticks_per_micro: AtomicU64,
    /// The clock of the cool-downs.
    origin: Instant,
    /// Workers take no job while this is set.
    #[cfg(test)]
    holding: AtomicBool,
    /// The ticks a test gives the workers in place of [`thread_ticks`].
    #[cfg(test)]
    given_ticks: std::sync::OnceLock<Arc<AtomicU64>>,
    /// Jobs the callback leaves to a worker.
    #[cfg(any(test, feature = "test-support"))]
    worker_only: [AtomicU64; 2],
    /// The gate stays open while this is set.
    #[cfg(any(test, feature = "test-support"))]
    gate_held: AtomicBool,
}

// SAFETY: a job slot belongs to one thread at a time. The callback owns each
// job that is not out. Its post writes the job, then sets the posted bit
// with a SeqCst RMW, and the one thread whose RMW clears that bit owns the
// job and sees each write of the post. The owner sets the done bit with a
// Release RMW after its last write, and the Acquire load of that bit gives
// the job back to the callback. One handle at a time holds the pool, so one
// callback posts. No thread writes `late_rule` or `origin` after `new`. The
// other fields are atomics, and a wake and the `OnceLock` of the given ticks
// are Sync.
unsafe impl Sync for PoolShared {}

impl PoolShared {
    /// `None` when the system makes no wake for a worker.
    pub(crate) fn new(workers: usize, late_rule: bool) -> Option<Self> {
        let wakes = (0..workers)
            .map(|_| WorkerWake::new())
            .collect::<Option<_>>()?;
        Some(Self {
            // Each slot is written here, on the producer, so the first post
            // finds its pages in memory.
            slots: (0..JOB_SLOTS).map(|_| Slot::default()).collect(),
            posted: Default::default(),
            done: Default::default(),
            parked: Padded::default(),
            idle: Padded::default(),
            wakes,
            cool_downs: (0..workers).map(|_| Padded::default()).collect(),
            ready: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
            in_buffer: AtomicBool::new(false),
            power_saving: AtomicBool::new(false),
            period: AtomicU64::new(0),
            attached: AtomicBool::new(false),
            gate_open: AtomicBool::new(false),
            serial_load: AtomicU64::new(0),
            late_rule,
            ticks_per_micro: AtomicU64::new(0),
            origin: Instant::now(),
            #[cfg(test)]
            holding: AtomicBool::new(false),
            #[cfg(test)]
            given_ticks: std::sync::OnceLock::new(),
            #[cfg(any(test, feature = "test-support"))]
            worker_only: Default::default(),
            #[cfg(any(test, feature = "test-support"))]
            gate_held: AtomicBool::new(false),
        })
    }

    /// A worker spins for its next job only outside a power-saving mode.
    fn set_power_saving(&self, saving: bool) {
        self.power_saving.store(saving, Ordering::Relaxed);
    }

    /// The workers serve a stream of `frames` at `rate` from here on, after
    /// a reopen at another buffer or rate.
    #[cfg(any(test, feature = "device-audio"))]
    pub(crate) fn follow_stream(&self, frames: u32, rate: u32) {
        self.period.store(period(frames, rate), Ordering::Relaxed);
    }

    /// The buffer frames and the rate of the stream the workers serve.
    pub(crate) fn period(&self) -> (u32, u32) {
        let period = self.period.load(Ordering::Relaxed);
        ((period >> 32) as u32, period as u32)
    }

    /// How long a worker spins for its next job while the callback is inside
    /// its buffer: one sub-block at the rate of the stream. So a worker stays
    /// awake from one sub-block to the next, and the wake of a core the
    /// system parked does not delay a job. 0 in a power-saving mode, and with
    /// no stream.
    fn spin_nanos(&self) -> u64 {
        if self.power_saving.load(Ordering::Relaxed) {
            return 0;
        }
        (REVERB_BLOCK as u64 * 1_000_000_000)
            .checked_div(u64::from(self.period().1))
            .unwrap_or(0)
    }

    /// The job of `slot`.
    ///
    /// # Safety
    ///
    /// The calling thread owns the slot: the callback while the job is not
    /// out, or the thread whose claim cleared the posted bit, until it sets
    /// the done bit. No other reference to the job lives.
    #[allow(clippy::mut_from_ref)]
    unsafe fn job(&self, slot: usize) -> &mut Job {
        // SAFETY: the caller owns the slot, as this function requires.
        unsafe { &mut *self.slots[slot].job.get() }
    }

    /// Nanoseconds since the pool started.
    fn now(&self) -> u64 {
        nanos(self.origin.elapsed())
    }

    /// The worker that takes the jobs of `bus` first.
    fn home(&self, bus: usize) -> usize {
        bus % self.wakes.len()
    }

    /// The posted bits of each kind.
    fn open(&self) -> [u64; 2] {
        self.posted
            .each_ref()
            .map(|posted| posted.0.load(Ordering::Relaxed))
    }

    /// True when a job waits for a worker. SeqCst, for the park.
    fn has_open(&self) -> bool {
        #[cfg(test)]
        if self.holding.load(Ordering::SeqCst) {
            return false;
        }
        self.posted
            .iter()
            .any(|posted| posted.0.load(Ordering::SeqCst) != 0)
    }

    /// True when this thread took the job of `slot`. It then owns the job
    /// until it sets the done bit.
    fn claim(&self, slot: usize) -> bool {
        let bit = 1u64 << (slot % MAX_INSERT_BUSES);
        self.posted[slot / MAX_INSERT_BUSES]
            .0
            .fetch_and(!bit, Ordering::SeqCst)
            & bit
            != 0
    }

    /// Gives a job that ran back to the callback.
    fn finish(&self, slot: usize) {
        let bit = 1u64 << (slot % MAX_INSERT_BUSES);
        self.done[slot / MAX_INSERT_BUSES]
            .0
            .fetch_or(bit, Ordering::Release);
    }

    /// The slot of the costliest job in `candidates`, or of the cheapest.
    fn pick(&self, candidates: [u64; 2], costliest: bool) -> Option<usize> {
        let mut best: Option<(u32, usize)> = None;
        for (kind, mut buses) in candidates.into_iter().enumerate() {
            while buses != 0 {
                let slot = kind * MAX_INSERT_BUSES + buses.trailing_zeros() as usize;
                buses &= buses - 1;
                let cost = self.slots[slot].cost.load(Ordering::Relaxed);
                let better = match best {
                    None => true,
                    Some((best, _)) if costliest => cost > best,
                    Some((best, _)) => cost < best,
                };
                if better {
                    best = Some((cost, slot));
                }
            }
        }
        best.map(|(_, slot)| slot)
    }

    /// Claims the costliest open job for a worker, of its `home` buses
    /// first.
    fn claim_costliest(&self, home: u64) -> Option<usize> {
        #[cfg(test)]
        if self.holding.load(Ordering::Relaxed) {
            return None;
        }
        loop {
            let open = self.open();
            let homes = open.map(|buses| buses & home);
            let slot = self.pick(if homes == [0; 2] { open } else { homes }, true)?;
            if self.claim(slot) {
                return Some(slot);
            }
        }
    }

    /// Claims the cheapest open job for the callback, of the `wanted` jobs
    /// first.
    fn claim_cheapest(&self, wanted: [u64; 2]) -> Option<usize> {
        loop {
            let open = self.open();
            #[cfg(any(test, feature = "test-support"))]
            let open =
                [0, 1].map(|kind| open[kind] & !self.worker_only[kind].load(Ordering::Relaxed));
            let needed = [0, 1].map(|kind| open[kind] & wanted[kind]);
            let slot = self.pick(if needed == [0; 2] { open } else { needed }, false)?;
            if self.claim(slot) {
                return Some(slot);
            }
        }
    }

    /// Signals one parked worker when the open jobs outnumber the idle
    /// awake workers: the `home` worker first. The SeqCst loads pair with
    /// the park of a worker, after the RMW that posted or claimed a job.
    fn wake(&self, home: usize) {
        let open: u32 = self
            .posted
            .iter()
            .map(|posted| posted.0.load(Ordering::SeqCst).count_ones())
            .sum();
        if open as usize <= self.idle.0.load(Ordering::SeqCst) {
            return;
        }
        let mut parked = self.parked.0.load(Ordering::SeqCst);
        let home = 1u64 << home;
        while parked != 0 {
            let bit = if parked & home != 0 {
                home
            } else {
                parked.isolate_lowest_one()
            };
            // The thread that clears the bit signals the worker.
            if self.parked.0.fetch_and(!bit, Ordering::SeqCst) & bit != 0 {
                self.wakes[bit.trailing_zeros() as usize].signal();
                return;
            }
            parked &= !bit;
        }
    }

    /// Makes `worker` sit out after a late job: the first cool-down, or
    /// twice the last one up to the longest. A worker that took jobs for
    /// [`COOL_DOWN_RESET_NANOS`] past its last cool-down starts again at the
    /// first.
    fn cool_down(&self, worker: usize, now: u64) {
        let cool = &self.cool_downs[worker].0;
        let until = cool.until.load(Ordering::Relaxed);
        let last = cool.length.load(Ordering::Relaxed);
        let length = if last == 0 || now >= until.saturating_add(COOL_DOWN_RESET_NANOS) {
            COOL_DOWN_NANOS
        } else {
            last.saturating_mul(2).min(MAX_COOL_DOWN_NANOS)
        };
        cool.length.store(length, Ordering::Relaxed);
        cool.until
            .store(now.saturating_add(length), Ordering::Release);
    }

    /// The nanoseconds of a run of `run` nanoseconds in which the system did
    /// not run the calling worker, from its thread ticks at the start of the
    /// run. A run of [`LATE_WAIT_NANOS`] or less gives 0 and reads no clock.
    /// A longer run also gives the pool its ticks of a microsecond. A tick is
    /// a cycle on Windows, so the pool learns them.
    fn lost_nanos(&self, started: Option<u64>, run: u64) -> u64 {
        let Some(ticks) = started
            .filter(|_| run > LATE_WAIT_NANOS)
            .and_then(|started| Some(self.worker_ticks()?.saturating_sub(started)))
        else {
            return 0;
        };
        let rate = ticks.saturating_mul(1_000) / run;
        let most = self
            .ticks_per_micro
            .fetch_max(rate, Ordering::Relaxed)
            .max(rate);
        ticks
            .saturating_mul(1_000)
            .checked_div(most)
            .map_or(0, |ran| run.saturating_sub(ran))
    }

    /// The ticks of the calling worker: [`thread_ticks`], or the ticks a
    /// test gives.
    fn worker_ticks(&self) -> Option<u64> {
        #[cfg(test)]
        if let Some(ticks) = self.given_ticks.get() {
            return Some(ticks.load(Ordering::Relaxed));
        }
        thread_ticks()
    }
}

#[cfg(test)]
impl PoolShared {
    /// Workers take no job while `hold` is true, so the callback runs each
    /// job itself.
    pub(crate) fn hold_claims(&self, hold: bool) {
        self.holding.store(hold, Ordering::SeqCst);
    }

    /// The callback leaves the job of `kind` and `bus` to a worker, and
    /// waits for it at the join.
    pub(crate) fn claim_before_join(&self, kind: JobKind, bus: usize) {
        self.worker_only[kind.index()].fetch_or(1 << bus, Ordering::SeqCst);
    }

    /// The workers read the ticks of `clock` in place of [`thread_ticks`].
    /// A test gives one clock, before the first job.
    fn give_ticks(&self, clock: Arc<AtomicU64>) {
        self.given_ticks.set(clock).expect("one given clock");
    }

    /// True while jobs post.
    #[cfg(feature = "test-support")]
    pub(crate) fn gate_is_open(&self) -> bool {
        self.gate_open.load(Ordering::Relaxed)
    }

    /// The workers that wait on their wake.
    #[cfg(feature = "test-support")]
    pub(crate) fn parked_workers(&self) -> u32 {
        self.parked.0.load(Ordering::SeqCst).count_ones()
    }
}

/// The wake of one worker off Windows and under Miri: a flag and the park
/// of the standard library. An unpark before the park makes the park return
/// at once, so no signal is lost.
#[cfg(any(not(windows), miri))]
struct WorkerWake {
    waiter: std::sync::OnceLock<thread::Thread>,
    signaled: AtomicBool,
}

#[cfg(any(not(windows), miri))]
impl WorkerWake {
    fn new() -> Option<Self> {
        Some(Self {
            waiter: std::sync::OnceLock::new(),
            signaled: AtomicBool::new(false),
        })
    }

    /// Names the thread that waits, before the first signal.
    fn set_waiter(&self, waiter: &thread::Thread) {
        let _ = self.waiter.set(waiter.clone());
    }

    fn signal(&self) {
        self.signaled.store(true, Ordering::Release);
        if let Some(waiter) = self.waiter.get() {
            waiter.unpark();
        }
    }

    /// Waits for a signal, `timeout` at most.
    fn wait(&self, timeout: Option<Duration>) {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        while !self.signaled.swap(false, Ordering::Acquire) {
            match deadline {
                None => thread::park(),
                Some(deadline) => {
                    let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                        return;
                    };
                    thread::park_timeout(left);
                }
            }
        }
    }
}

/// The worker threads of one output, started on the producer. The drop
/// stops them: each worker ends its job and then ends.
pub(crate) struct PluginPool {
    shared: Arc<PoolShared>,
    threads: Vec<JoinHandle<()>>,
    /// The pool clock at the last read of the power mode.
    power_read: Option<u64>,
}

impl PluginPool {
    /// Starts `workers` workers that each take a real-time class for a
    /// device buffer of `frames` at `rate`. On Windows and Linux each worker
    /// keeps to the performance cores. A worker the system refuses the class
    /// ends before its first job. The late rule is off where the ticks of a
    /// thread have no fixed rate.
    #[cfg(feature = "device-audio")]
    pub(crate) fn start(workers: usize, frames: u32, rate: u32) -> Option<Self> {
        #[cfg(windows)]
        let late_rule = crate::mmcss::cycles_at_fixed_rate();
        #[cfg(not(windows))]
        let late_rule = true;
        Self::spawn(workers, period(frames, rate), late_rule, move || {
            pin_to_performance_cores();
            real_time(frames, rate)
        })
    }

    /// Starts [`MAX_PLUGIN_WORKERS`] workers at most. Each worker takes
    /// jobs only with the class that `class` gives it for the buffer of
    /// `period`, which it holds to its end. A pool of period 0 has workers
    /// that take no class again. With `late_rule`, a worker sits out after a
    /// late job. `None` with no worker.
    fn spawn<C>(
        workers: usize,
        period: u64,
        late_rule: bool,
        class: impl Fn() -> Option<C> + Copy + Send + 'static,
    ) -> Option<Self> {
        let workers = workers.min(MAX_PLUGIN_WORKERS);
        if workers == 0 {
            return None;
        }
        let mut pool = Self {
            shared: Arc::new(PoolShared::new(workers, late_rule)?),
            threads: Vec::with_capacity(workers),
            power_read: None,
        };
        pool.shared.period.store(period, Ordering::Relaxed);
        for worker in 0..workers {
            let shared = Arc::clone(&pool.shared);
            let thread = thread::Builder::new()
                .name(format!("rustel-plugin-{worker}"))
                .spawn(move || {
                    let Some(_class) = class() else {
                        return;
                    };
                    touch_stack();
                    tripwire::audio_scope(|| work(&shared, worker, period));
                })
                .ok()?;
            #[cfg(any(not(windows), miri))]
            pool.shared.wakes[worker].set_waiter(thread.thread());
            pool.threads.push(thread);
        }
        Some(pool)
    }

    pub(crate) fn shared(&self) -> &Arc<PoolShared> {
        &self.shared
    }

    /// The workers that have their real-time class and take jobs.
    pub(crate) fn ready_workers(&self) -> usize {
        self.shared.ready.load(Ordering::Acquire)
    }

    /// Gives the workers the power mode: a worker spins for its next job
    /// only outside a power-saving mode. The producer reads the mode once a
    /// second at most, and only while a worker takes jobs.
    pub(crate) fn follow_power_mode(&mut self) {
        let now = self.shared.now();
        if self.ready_workers() == 0
            || self
                .power_read
                .is_some_and(|read| now.saturating_sub(read) < POWER_MODE_READ_NANOS)
        {
            return;
        }
        self.power_read = Some(now);
        self.shared
            .set_power_saving(crate::power_mode::power_saving() == Some(true));
    }
}

#[cfg(any(test, feature = "test-support"))]
impl PluginPool {
    /// A pool whose workers need no real-time class. With `held`, the gate
    /// stays open, so each sub-block of a plugin chain posts its jobs.
    pub(crate) fn start_test_pool(workers: usize, held: bool) -> Option<Self> {
        let pool = Self::spawn(workers, 0, true, || Some(()))?;
        pool.shared.gate_open.store(held, Ordering::Relaxed);
        pool.shared.gate_held.store(held, Ordering::Relaxed);
        // Each worker is ready here, so the first sub-block posts its jobs.
        while pool.ready_workers() < pool.threads.len() {
            thread::yield_now();
        }
        Some(pool)
    }
}

#[cfg(test)]
impl PluginPool {
    /// A pool whose workers get no real-time class, so each ends before its
    /// first job.
    pub(crate) fn start_without_class(workers: usize) -> Option<Self> {
        Self::spawn(workers, 0, true, || None::<()>)
    }
}

/// True on the thread of a plugin worker, by the name its spawn gives it.
#[cfg(test)]
pub(crate) fn on_plugin_worker() -> bool {
    thread::current()
        .name()
        .is_some_and(|name| name.starts_with("rustel-plugin"))
}

/// The workers of [`set_plugin_pool_for_test`], or [`usize::MAX`] for none.
#[cfg(any(test, feature = "test-support"))]
static TEST_WORKERS: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Test support, a switch of the process: with `Some(workers)`, each output
/// that needs plugin workers starts that many workers with no real-time
/// class, and its gate stays open. With `None`, an output starts real-time
/// workers again.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn set_plugin_pool_for_test(workers: Option<usize>) {
    TEST_WORKERS.store(workers.unwrap_or(usize::MAX), Ordering::SeqCst);
}

/// The workers of [`set_plugin_pool_for_test`].
#[cfg(all(feature = "device-audio", any(test, feature = "test-support")))]
pub(crate) fn test_workers() -> Option<usize> {
    Some(TEST_WORKERS.load(Ordering::SeqCst)).filter(|workers| *workers != usize::MAX)
}

impl Drop for PluginPool {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        for wake in &self.shared.wakes {
            wake.signal();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// The workers of a pool: the cores of the process minus 2, which stay for
/// the callback and the threads that are not real time. The hardware
/// threads of one core count as one core. A CPU with two kinds of cores
/// counts its performance cores. On Linux an Intel or an Arm CPU names
/// them, and another CPU counts each core. The pool starts
/// [`MAX_PLUGIN_WORKERS`] at most, and none below 3 cores.
#[cfg(feature = "device-audio")]
pub(crate) fn plugin_workers() -> usize {
    cores().map_or(0, workers_for)
}

fn workers_for(cores: usize) -> usize {
    cores.saturating_sub(2)
}

/// A device buffer of `frames` at `rate` in one word.
#[cfg(any(test, feature = "device-audio"))]
fn period(frames: u32, rate: u32) -> u64 {
    (u64::from(frames) << 32) | u64::from(rate)
}

/// Gives the calling worker its real-time class, held to the end of the
/// thread: MMCSS "Pro Audio" at the critical priority on Windows, and the
/// class of the `audio_thread_priority` crate for a buffer of `frames` at
/// `rate` on macOS and Linux. `None` when the system refuses it.
#[cfg(all(feature = "device-audio", windows))]
fn real_time(_frames: u32, _rate: u32) -> Option<impl Sized> {
    crate::mmcss::ProAudioThread::attach_worker()
}

#[cfg(all(
    feature = "device-audio",
    any(target_os = "macos", target_os = "linux")
))]
fn real_time(frames: u32, rate: u32) -> Option<impl Sized> {
    audio_thread_priority::promote_current_thread_to_real_time(frames, rate).ok()
}

/// The other systems give a worker no real-time class, so a pool there has
/// no ready worker.
#[cfg(all(
    feature = "device-audio",
    not(any(windows, target_os = "macos", target_os = "linux"))
))]
fn real_time(_frames: u32, _rate: u32) -> Option<impl Sized> {
    None::<()>
}

/// Keeps the calling worker on the performance cores of a CPU with two
/// kinds of cores, so a job does not run on an efficiency core while the
/// callback waits for it. On Windows the worker selects their CPU sets.
#[cfg(all(feature = "device-audio", windows))]
fn pin_to_performance_cores() {
    crate::mmcss::select_performance_cores();
}

/// On Linux the worker keeps to the CPUs that the count of workers reads.
#[cfg(all(feature = "device-audio", target_os = "linux"))]
fn pin_to_performance_cores() {
    if let Some(cpus) = performance_cpus() {
        // SAFETY: `cpus` holds the bytes its size names, and pid 0 names the
        // calling thread.
        unsafe { libc::sched_setaffinity(0, size_of_val(&cpus), &cpus) };
    }
}

/// macOS places a worker itself, and the other systems name no performance
/// cores here.
#[cfg(all(feature = "device-audio", not(any(windows, target_os = "linux"))))]
fn pin_to_performance_cores() {}

/// The cores of the process. `None` when the system does not say.
#[cfg(all(feature = "device-audio", windows))]
fn cores() -> Option<usize> {
    crate::mmcss::performance_cores()
}

#[cfg(all(feature = "device-audio", target_os = "macos"))]
fn cores() -> Option<usize> {
    // Apple silicon names its performance cores level 0, an Intel Mac has
    // one kind of core, and macOS gives no affinity mask.
    [c"hw.perflevel0.physicalcpu", c"hw.physicalcpu"]
        .into_iter()
        .find_map(|name| {
            let mut cores: libc::c_int = 0;
            let mut size = size_of::<libc::c_int>();
            // SAFETY: the name ends in NUL, `cores` holds the `size` bytes
            // the call writes, and no new value is set.
            let read = unsafe {
                libc::sysctlbyname(
                    name.as_ptr(),
                    (&raw mut cores).cast(),
                    &mut size,
                    std::ptr::null_mut(),
                    0,
                )
            };
            usize::try_from(cores)
                .ok()
                .filter(|cores| read == 0 && *cores > 0)
        })
}

#[cfg(all(feature = "device-audio", target_os = "linux"))]
fn cores() -> Option<usize> {
    let cpus = performance_cpus()?;
    // Each hardware thread of one core lists the same siblings.
    let mut cores: Vec<String> = Vec::new();
    for cpu in 0..libc::CPU_SETSIZE as usize {
        // SAFETY: `cpu` is below `CPU_SETSIZE`, the bits of the set.
        if !unsafe { libc::CPU_ISSET(cpu, &cpus) } {
            continue;
        }
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list");
        let siblings = std::fs::read_to_string(path).ok()?;
        if !cores.contains(&siblings) {
            cores.push(siblings);
        }
    }
    Some(cores.len())
}

/// The CPUs of the performance cores that the calling thread runs on: each
/// hardware thread of such a core. `None` when the system does not say.
#[cfg(all(feature = "device-audio", target_os = "linux"))]
fn performance_cpus() -> Option<libc::cpu_set_t> {
    // SAFETY: a CPU set is an array of bits, and all zeros is the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: `set` holds the bytes its size names, and pid 0 names the
    // calling thread.
    if unsafe { libc::sched_getaffinity(0, size_of_val(&set), &mut set) } != 0 {
        return None;
    }
    let read = |path: &str| std::fs::read_to_string(path).ok();
    // An Intel CPU with two kinds of cores lists its performance cores.
    let listed = read("/sys/devices/cpu_core/cpus");
    Some(performance_cpus_of(set, listed.as_deref(), |cpu| {
        read(&format!("/sys/devices/system/cpu/cpu{cpu}/cpu_capacity"))
    }))
}

/// The CPUs of `affinity` on the performance cores. An Intel CPU with two
/// kinds of cores lists them in `listed`. Else the kernel gives the largest
/// cores of the system the capacity 1024, and `capacity` reads the capacity
/// of a CPU. With no list and no capacity, each CPU counts.
#[cfg(all(feature = "device-audio", target_os = "linux"))]
fn performance_cpus_of(
    mut affinity: libc::cpu_set_t,
    listed: Option<&str>,
    capacity: impl Fn(usize) -> Option<String>,
) -> libc::cpu_set_t {
    for cpu in 0..libc::CPU_SETSIZE as usize {
        // SAFETY: `cpu` is below `CPU_SETSIZE`, the bits of the set.
        if !unsafe { libc::CPU_ISSET(cpu, &affinity) } {
            continue;
        }
        let performance = match listed {
            Some(list) => cpu_listed(list, cpu),
            None => capacity(cpu)
                .and_then(|capacity| capacity.trim().parse::<u32>().ok())
                .is_none_or(|capacity| capacity >= 1024),
        };
        if !performance {
            // SAFETY: as above.
            unsafe { libc::CPU_CLR(cpu, &mut affinity) };
        }
    }
    affinity
}

/// True when `list`, a list of CPUs such as `0-7,16`, holds `cpu`.
#[cfg(any(test, all(feature = "device-audio", target_os = "linux")))]
fn cpu_listed(list: &str, cpu: usize) -> bool {
    list.trim().split(',').any(|range| {
        let (first, last) = range.split_once('-').unwrap_or((range, range));
        first.parse().is_ok_and(|first: usize| first <= cpu)
            && last.parse().is_ok_and(|last: usize| cpu <= last)
    })
}

/// The other systems name no cores here. No worker gets a real-time class
/// there either.
#[cfg(all(
    feature = "device-audio",
    not(any(windows, target_os = "macos", target_os = "linux"))
))]
fn cores() -> Option<usize> {
    None
}

/// The time the calling thread ran, in ticks: cycles on Windows,
/// nanoseconds on macOS and Linux. `None` where the system does not say.
#[cfg(all(windows, not(miri)))]
fn thread_ticks() -> Option<u64> {
    crate::mmcss::thread_cycles()
}

#[cfg(all(
    feature = "device-audio",
    any(target_os = "macos", target_os = "linux"),
    not(miri)
))]
fn thread_ticks() -> Option<u64> {
    // SAFETY: a time is a pair of integers, and all zeros is time 0.
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: `time` is a valid out-pointer, and the clock is the time the
    // calling thread ran.
    let read = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    (read == 0).then(|| time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64)
}

/// The other systems and Miri give no thread time, so no job is late
/// there.
#[cfg(not(any(
    all(windows, not(miri)),
    all(
        feature = "device-audio",
        any(target_os = "macos", target_os = "linux"),
        not(miri)
    )
)))]
fn thread_ticks() -> Option<u64> {
    None
}

/// Writes the stack a job uses, so the first job finds its pages in memory.
fn touch_stack() {
    let stack = [0u8; STACK_TOUCH_BYTES];
    std::hint::black_box(&stack);
}

/// A worker's place in the counts of the pool. Its drop leaves them when
/// the worker ends, also in a panic outside a job. A job the worker holds
/// then ends with a payload, so the callback does not wait for it forever.
struct WorkerState<'a> {
    shared: &'a PoolShared,
    worker: usize,
    idle: bool,
    held: Option<usize>,
}

impl<'a> WorkerState<'a> {
    fn start(shared: &'a PoolShared, worker: usize) -> Self {
        shared.ready.fetch_add(1, Ordering::SeqCst);
        shared.idle.0.fetch_add(1, Ordering::SeqCst);
        Self {
            shared,
            worker,
            idle: true,
            held: None,
        }
    }

    fn enter_idle(&mut self) {
        self.shared.idle.0.fetch_add(1, Ordering::SeqCst);
        self.idle = true;
    }

    fn leave_idle(&mut self) {
        self.shared.idle.0.fetch_sub(1, Ordering::SeqCst);
        self.idle = false;
    }
}

impl Drop for WorkerState<'_> {
    fn drop(&mut self) {
        if self.idle {
            self.leave_idle();
        }
        if let Some(slot) = self.held.take() {
            // SAFETY: the claim of this worker still holds the job.
            let job = unsafe { self.shared.job(slot) };
            job.panic = Some(Box::new("a plugin worker failed outside its job"));
            job.worker = Some(self.worker);
            self.shared.finish(slot);
        }
        self.shared.ready.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The loop of one worker, inside the audio scope of the tripwire.
/// `promoted` is the buffer the worker took its class for.
fn work(shared: &PoolShared, worker: usize, mut promoted: u64) {
    let mut state = WorkerState::start(shared, worker);
    let home = (0..MAX_INSERT_BUSES)
        .filter(|bus| shared.home(*bus) == worker)
        .fold(0u64, |buses, bus| buses | 1 << bus);
    let parked = 1u64 << worker;
    let mut since = shared.now();
    while !shared.closing.load(Ordering::Acquire) {
        // A reopen at another buffer or rate gives the stream a new period.
        // The time-constraint policy of macOS follows it before the next
        // job, and a refused promotion keeps the last policy. The class on
        // Windows and Linux does not depend on the period, and the workers
        // of a test pool take no class.
        let period = shared.period.load(Ordering::Relaxed);
        if promoted != 0 && period != promoted {
            promoted = period;
            #[cfg(all(feature = "device-audio", target_os = "macos"))]
            let _ = real_time((period >> 32) as u32, period as u32);
        }
        let until = shared.cool_downs[worker].0.until.load(Ordering::Acquire);
        let now = shared.now();
        if now < until {
            // A late worker sits out, and a parked worker takes its jobs.
            state.leave_idle();
            shared.wake(worker);
            shared.wakes[worker].wait(Some(Duration::from_nanos(until - now)));
            state.enter_idle();
            since = shared.now();
        } else if let Some(slot) = shared.claim_costliest(home) {
            state.leave_idle();
            state.held = Some(slot);
            shared.wake(worker);
            enable_denormal_flush();
            // SAFETY: the claim gives this worker the job until its done bit.
            let job = unsafe { shared.job(slot) };
            let started = shared.late_rule.then(|| shared.worker_ticks()).flatten();
            run_job(JobKind::of(slot), job, Some(worker));
            job.lost_nanos = shared.lost_nanos(started, job.run_nanos);
            shared.finish(slot);
            state.held = None;
            state.enter_idle();
            since = shared.now();
        } else if shared.in_buffer.load(Ordering::Relaxed)
            && now.saturating_sub(since) < shared.spin_nanos()
        {
            std::hint::spin_loop();
        } else {
            state.leave_idle();
            shared.parked.0.fetch_or(parked, Ordering::SeqCst);
            if !shared.has_open() && !shared.closing.load(Ordering::SeqCst) {
                shared.wakes[worker].wait(None);
            }
            shared.parked.0.fetch_and(!parked, Ordering::SeqCst);
            state.enter_idle();
            since = shared.now();
        }
    }
}

/// Runs a job and keeps the payload of a panic in it, for its join to raise
/// again.
fn run_job(kind: JobKind, job: &mut Job, worker: Option<usize>) {
    let started = Instant::now();
    let run = std::panic::catch_unwind(AssertUnwindSafe(|| match kind {
        JobKind::Instrument => run_instrument_job(job),
        JobKind::Effect => run_effect_job(job),
    }));
    job.run_nanos = nanos(started.elapsed());
    job.worker = worker;
    job.panic = run.err();
}

/// The instrument renders one block on silence: the submit and the collect
/// of the serial engine, in their order. An instrument that leaves its slot
/// in a fade renders its own block on silence too.
fn run_instrument_job(job: &mut Job) {
    let [left, right] = &mut job.block;
    let (left, right) = (&mut left[..job.frames], &mut right[..job.frames]);
    left.fill(0.0);
    right.fill(0.0);
    if let Some(instrument) = job.boxes[0].as_mut() {
        instrument.submit(left, right);
        instrument.collect(left, right);
    }
    if let Some(leaving) = job.leaving[0].as_deref_mut() {
        render_leaving_insert(leaving, None, &mut job.leaving_block, job.frames, false);
    }
}

/// The effect stages process the block in signal order, from the first
/// stage that woke on the callback. Each stage settles its output and counts
/// its idle frames, and the next stage of the order that wakes takes that
/// output, as in the rounds of the serial engine. A stage whose levels move
/// mixes its output with the output of its leaving copy and its input, as
/// the rounds do.
fn run_effect_job(job: &mut Job) {
    let Job {
        frames,
        block,
        boxes,
        leaving,
        mixes,
        dry,
        leaving_block,
        idle,
        order,
        first,
        fed,
        idle_limit,
        ..
    } = job;
    let [left, right] = block;
    let (left, right) = (&mut left[..*frames], &mut right[..*frames]);
    let mut next = Some((*first, *fed));
    while let Some((place, fed)) = next {
        let stage = usize::from(order[place]);
        let Some(effect) = boxes[stage].as_mut() else {
            break;
        };
        let moving = mixes[stage].moving();
        if moving {
            dry[0][..*frames].copy_from_slice(left);
            dry[1][..*frames].copy_from_slice(right);
        }
        effect.submit(left, right);
        effect.collect(left, right);
        let silent = settle_insert_block(left, right);
        idle[stage] = if fed || !silent {
            0
        } else {
            idle[stage].saturating_add(*frames as u32)
        };
        if moving {
            let leaving = leaving[stage].as_deref_mut();
            fade_effect_stage(left, right, dry, leaving, leaving_block, &mut mixes[stage]);
        }
        next = ordered_stages(order, place + 1).find_map(|(place, stage)| {
            let effect = boxes[stage].as_deref()?;
            effect_stage_wakes(effect, left, right, idle[stage], *idle_limit, &mixes[stage])
                .map(|fed| (place, fed))
        });
    }
}

/// The cost of a slot after a run of `run` nanoseconds: the first run, or
/// an average that weighs the run by 1/4. Never 0 after a run.
fn next_cost(average: u32, run: u64) -> u32 {
    let run = run.min(u64::from(u32::MAX));
    let next = if average == 0 {
        run
    } else {
        (3 * u64::from(average) + run) / 4
    };
    next.max(1) as u32
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// `nanos` in basis points of `period_nanos`. 0 for a period of 0.
fn basis_points(nanos: u64, period_nanos: u64) -> u64 {
    (u128::from(nanos) * 10_000)
        .checked_div(u128::from(period_nanos))
        .map_or(0, |points| u64::try_from(points).unwrap_or(u64::MAX))
}

/// The jobs of one buffer of the callback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PoolFigures {
    /// The jobs the callback ran itself.
    pub(crate) inline_jobs: u64,
    /// The time the callback spun for a worker, in nanoseconds.
    pub(crate) join_wait_nanos: u64,
    /// The time workers ran jobs, in nanoseconds.
    pub(crate) worker_nanos: u64,
    /// The jobs the callback waited for that ran late.
    pub(crate) late_jobs: u64,
}

/// The pool as one callback holds it, with the figures of its buffer.
pub(crate) struct PoolHandle {
    shared: Arc<PoolShared>,
    figures: PoolFigures,
    /// The jobs this handle posted and did not join, for each kind. A guard
    /// that never drops leaves its jobs out here.
    out: [u64; 2],
    /// Set until the end of the first buffer. The callback has its class
    /// from the start of the next buffer on, so the gate opens inside a
    /// buffer only after the first one. The callback then has its class
    /// before its first job.
    first_buffer: bool,
}

impl PoolHandle {
    /// Takes the pool for one callback. `None` while another handle holds
    /// it.
    pub(crate) fn new(shared: Arc<PoolShared>) -> Option<Self> {
        shared
            .attached
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .ok()?;
        Some(Self {
            shared,
            figures: PoolFigures::default(),
            out: [0; 2],
            first_buffer: true,
        })
    }

    /// True when the jobs of a sub-block of `frames` post: the sub-block
    /// has [`MIN_POOLED_FRAMES`] at least, the gate is open, and a worker
    /// has its class.
    pub(crate) fn runs(&self, frames: usize) -> bool {
        frames >= MIN_POOLED_FRAMES
            && self.shared.gate_open.load(Ordering::Relaxed)
            && self.shared.ready.load(Ordering::Acquire) > 0
    }

    /// The figures of the buffer so far.
    #[cfg(test)]
    pub(crate) fn figures(&self) -> PoolFigures {
        self.figures
    }

    /// The callback leaves each job to a worker, and waits for it at the
    /// join.
    #[cfg(feature = "test-support")]
    pub(crate) fn leave_jobs_to_workers(&self) {
        for jobs in &self.shared.worker_only {
            jobs.store(u64::MAX, Ordering::SeqCst);
        }
    }

    /// The jobs of one sub-block.
    pub(crate) fn begin(&mut self) -> JoinGuard<'_> {
        JoinGuard {
            pool: self,
            spun: [0; 2],
        }
    }

    /// Ends a sub-block of a buffer of the callback. The frames of the
    /// buffer so far last `rendered_nanos`, and `busy` gives the time the
    /// callback took for them. With the gate closed the callback ran each
    /// job, so that time is the serial load. The gate opens when it passes
    /// [`GATE_PEAK_LOAD`], so a heavy start posts its jobs from its next
    /// sub-block. `busy` runs only while the gate is closed, and not in the
    /// first buffer.
    pub(crate) fn end_sub_block(&self, rendered_nanos: u64, busy: impl FnOnce() -> u64) {
        let gate = &self.shared.gate_open;
        if self.first_buffer || gate.load(Ordering::Relaxed) {
            return;
        }
        if basis_points(busy(), rendered_nanos) > GATE_PEAK_LOAD {
            gate.store(true, Ordering::Relaxed);
        }
    }

    /// Ends a buffer of the callback of `period_nanos` that took
    /// `busy_nanos`, and gives the figures of its jobs. A worker with no job
    /// parks. The serial load of the buffer is its busy time with no wait
    /// for a worker, plus the time workers ran its jobs. The gate follows
    /// that load, except in a stopped buffer. A buffer past the peak keeps
    /// the gate open for the next buffer, also when its own sub-blocks
    /// opened it.
    pub(crate) fn end_buffer(
        &mut self,
        busy_nanos: u64,
        period_nanos: u64,
        stopped: bool,
    ) -> PoolFigures {
        let shared = &*self.shared;
        shared.in_buffer.store(false, Ordering::Relaxed);
        self.first_buffer = false;
        let figures = std::mem::take(&mut self.figures);
        if stopped || period_nanos == 0 {
            return figures;
        }
        #[cfg(any(test, feature = "test-support"))]
        if shared.gate_held.load(Ordering::Relaxed) {
            return figures;
        }
        let serial = busy_nanos
            .saturating_sub(figures.join_wait_nanos)
            .saturating_add(figures.worker_nanos);
        let load = basis_points(serial, period_nanos);
        let average = shared
            .serial_load
            .load(Ordering::Relaxed)
            .saturating_mul(3)
            .saturating_add(load)
            / 4;
        shared.serial_load.store(average, Ordering::Relaxed);
        let open = load > GATE_PEAK_LOAD
            || if shared.gate_open.load(Ordering::Relaxed) {
                average >= GATE_CLOSE_LOAD
            } else {
                average > GATE_OPEN_LOAD
            };
        shared.gate_open.store(open, Ordering::Relaxed);
        figures
    }
}

impl Drop for PoolHandle {
    fn drop(&mut self) {
        // A handle with a job out keeps the pool, so no other handle hands
        // out a job that a worker owns.
        if self.out == [0; 2] {
            self.shared.attached.store(false, Ordering::Release);
        }
    }
}

/// The jobs one sub-block posts. The callback fills a job, posts it and
/// joins it before the sub-block ends. While a job is out, its boxes are in
/// the job and their slots in the backend are empty. The guard drops at the
/// end of its sub-block, and a debug build checks there that each job
/// joined.
///
/// The drop of a guard with a job out takes back each job no worker claimed
/// and waits for each claimed one, so no worker touches a box once the frame
/// of the callback unwinds. In a panic it also drops the boxes of each job.
pub(crate) struct JoinGuard<'a> {
    pool: &'a mut PoolHandle,
    /// The spins of the set the callback joins, for each kind, in
    /// nanoseconds. The last job of the set takes them.
    spun: [u64; 2],
}

impl JoinGuard<'_> {
    /// The job of `kind` and `bus`: the callback fills it before the post
    /// and reads it after the join.
    pub(crate) fn job(&mut self, kind: JobKind, bus: usize) -> &mut Job {
        assert!(
            self.pool.out[kind.index()] & (1 << bus) == 0,
            "the job is out"
        );
        // SAFETY: the job is not out, so it is the callback's. A worker owns
        // a slot only from the claim of a post to its done bit. The out bits
        // live on the handle, so a guard that never drops leaves its jobs
        // out. A handle with a job out keeps the pool, and one handle holds
        // the pool. The guard is borrowed mutably for this reference.
        unsafe { self.pool.shared.job(kind.slot(bus)) }
    }

    /// Posts the job of `kind` and `bus`, which [`Self::job`] filled, and
    /// wakes a worker when the open jobs outnumber the idle ones.
    pub(crate) fn post(&mut self, kind: JobKind, bus: usize) {
        let bit = 1u64 << bus;
        assert!(self.pool.out[kind.index()] & bit == 0, "the job is out");
        self.pool.out[kind.index()] |= bit;
        let shared = &*self.pool.shared;
        if !shared.in_buffer.load(Ordering::Relaxed) {
            shared.in_buffer.store(true, Ordering::Relaxed);
        }
        shared.posted[kind.index()]
            .0
            .fetch_or(bit, Ordering::SeqCst);
        shared.wake(shared.home(bus));
    }

    /// Joins a job of `kind` among `buses` that a thread finished, and
    /// gives its bus. `None` when none is done.
    pub(crate) fn try_join(&mut self, kind: JobKind, buses: u64) -> Option<usize> {
        self.take_done(kind, buses, 0)
    }

    /// Joins a job of `kind` among `buses`, which are out, and gives its
    /// bus. `buses` are the jobs of one set that the callback did not join
    /// yet. A job among them that no worker claimed runs on the callback.
    /// While each runs on a worker, the callback runs the cheapest open job,
    /// and with no open job it spins for the workers.
    pub(crate) fn join(&mut self, kind: JobKind, buses: u64) -> usize {
        assert!(
            buses != 0 && buses & !self.pool.out[kind.index()] == 0,
            "each job to join is out"
        );
        let mut waited = 0;
        let mut spinning: Option<Instant> = None;
        loop {
            let wait = waited + spinning.map_or(0, |since| nanos(since.elapsed()));
            if let Some(bus) = self.take_done(kind, buses, wait) {
                self.pool.figures.join_wait_nanos += wait;
                return bus;
            }
            if self.help(kind, buses) {
                waited = wait;
                spinning = None;
            } else {
                spinning.get_or_insert_with(Instant::now);
                std::hint::spin_loop();
            }
        }
    }

    /// Runs an open job on the callback: the cheapest job of `kind` among
    /// `buses`, else the cheapest of all. False when no job is open.
    fn help(&mut self, kind: JobKind, buses: u64) -> bool {
        let shared = &*self.pool.shared;
        let mut wanted = [0; 2];
        wanted[kind.index()] = buses;
        let Some(slot) = shared.claim_cheapest(wanted) else {
            return false;
        };
        // SAFETY: the claim gives the callback the job until its done bit.
        let job = unsafe { shared.job(slot) };
        run_job(JobKind::of(slot), job, None);
        shared.finish(slot);
        self.pool.figures.inline_jobs += 1;
        true
    }

    /// Joins a done job of `kind` among `buses`, after the callback spun
    /// `spun` nanoseconds in this join. The cost of the bus moves, and a
    /// panic of the job raises again here. The spins of a set add up, and
    /// the last job of the set takes them. When that job ran late, its
    /// worker cools down.
    fn take_done(&mut self, kind: JobKind, buses: u64, spun: u64) -> Option<usize> {
        let shared = &*self.pool.shared;
        let index = kind.index();
        let done = shared.done[index].0.load(Ordering::Acquire) & buses & self.pool.out[index];
        if done == 0 {
            return None;
        }
        let bus = done.trailing_zeros() as usize;
        let bit = 1u64 << bus;
        shared.done[index].0.fetch_and(!bit, Ordering::Relaxed);
        self.pool.out[index] &= !bit;
        // A job that ends while another job of its set is out held nothing.
        // The callback waits on for that job.
        self.spun[index] += spun;
        let waited = if buses & self.pool.out[index] == 0 {
            std::mem::take(&mut self.spun[index])
        } else {
            0
        };
        let slot = kind.slot(bus);
        // SAFETY: the Acquire load of the done bit gave the job back to the
        // callback.
        let job = unsafe { shared.job(slot) };
        let cost = &shared.slots[slot].cost;
        let average = cost.load(Ordering::Relaxed);
        let figures = &mut self.pool.figures;
        if let Some(worker) = job.worker {
            figures.worker_nanos += job.run_nanos;
            if waited > LATE_WAIT_NANOS && job.lost_nanos > LATE_WAIT_NANOS {
                figures.late_jobs += 1;
                shared.cool_down(worker, shared.now());
            }
        }
        cost.store(next_cost(average, job.run_nanos), Ordering::Relaxed);
        if let Some(payload) = job.panic.take() {
            std::panic::resume_unwind(payload);
        }
        Some(bus)
    }
}

impl Drop for JoinGuard<'_> {
    fn drop(&mut self) {
        let panicking = thread::panicking();
        let shared = &*self.pool.shared;
        if self.pool.out == [0; 2] && !panicking {
            // The sub-block joined each job it posted, so no bit is left.
            debug_assert!(
                (0..2).all(|kind| shared.posted[kind].0.load(Ordering::Relaxed) == 0
                    && shared.done[kind].0.load(Ordering::Relaxed) == 0),
                "a sub-block ended with a posted or a done bit"
            );
            return;
        }
        for (kind, out) in self.pool.out.into_iter().enumerate() {
            // A job no worker claimed comes back here. A claimed job ends on
            // its worker.
            let taken = shared.posted[kind].0.fetch_and(!out, Ordering::SeqCst) & out;
            let running = out & !taken;
            while shared.done[kind].0.load(Ordering::Acquire) & running != running {
                std::hint::spin_loop();
            }
            shared.done[kind].0.fetch_and(!out, Ordering::Relaxed);
        }
        self.pool.out = [0; 2];
        debug_assert!(panicking, "a sub-block ended with a job out");
        if panicking {
            // The frame unwinds and drops the boxes of the backend. The boxes
            // of the jobs go with them.
            for slot in 0..JOB_SLOTS {
                // SAFETY: no job is out, so each slot is the callback's.
                let job = unsafe { shared.job(slot) };
                job.boxes = Default::default();
                job.leaving = Default::default();
                job.panic = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insert::{InsertKey, InsertParam};

    type Chain = [Option<Box<dyn OrbitInsert>>; EFFECT_CHAIN];

    /// An insert that adds 1 to each sample after `spin` of work and a
    /// `pause` off the CPU, or panics with a fault, and counts its calls.
    /// With `ticks`, each call adds its time less its pause to that clock,
    /// one tick a nanosecond. So a stop of the thread in the call reads as
    /// work, and only the pause reads as lost time.
    #[derive(Default)]
    struct Probe {
        runs: Arc<AtomicU64>,
        in_call: AtomicBool,
        spin: Duration,
        pause: Duration,
        fault: bool,
        ticks: Option<Arc<AtomicU64>>,
    }

    impl OrbitInsert for Probe {
        fn key(&self) -> InsertKey {
            InsertKey::default()
        }

        fn set_param(&mut self, _: InsertParam, _: u32) {}

        fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
            assert!(
                !self.in_call.swap(true, Ordering::SeqCst),
                "two calls overlap"
            );
            let start = Instant::now();
            while start.elapsed() < self.spin {
                std::hint::spin_loop();
            }
            let paused = if self.pause.is_zero() {
                Duration::ZERO
            } else {
                let since = Instant::now();
                thread::sleep(self.pause);
                since.elapsed()
            };
            if self.fault {
                panic!("a plugin fault");
            }
            for sample in left.iter_mut().chain(right.iter_mut()) {
                *sample += 1.0;
            }
            if let Some(ticks) = &self.ticks {
                let ran = start.elapsed().saturating_sub(paused);
                ticks.fetch_add(nanos(ran), Ordering::SeqCst);
            }
            self.runs.fetch_add(1, Ordering::SeqCst);
            self.in_call.store(false, Ordering::SeqCst);
        }
    }

    fn probe(spin: Duration, fault: bool) -> (Box<dyn OrbitInsert>, Arc<AtomicU64>) {
        let runs = Arc::new(AtomicU64::new(0));
        let probe = Probe {
            runs: Arc::clone(&runs),
            in_call: AtomicBool::new(false),
            spin,
            pause: Duration::ZERO,
            fault,
            ticks: None,
        };
        (Box::new(probe), runs)
    }

    /// A test pool with each worker ready, and its handle.
    fn ready_pool(workers: usize) -> (PluginPool, PoolHandle) {
        let pool = PluginPool::start_test_pool(workers, true).expect("a pool");
        let handle = PoolHandle::new(Arc::clone(&pool.shared)).expect("one handle");
        (pool, handle)
    }

    /// Runs `f` with no panic message, and gives its panic.
    fn caught<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = std::panic::catch_unwind(AssertUnwindSafe(f));
        std::panic::set_hook(hook);
        caught
    }

    /// Fills the job of `kind` and `bus` with one box at stage 0, and posts
    /// it.
    fn post(guard: &mut JoinGuard<'_>, kind: JobKind, bus: usize, insert: Box<dyn OrbitInsert>) {
        let job = guard.job(kind, bus);
        job.frames = 1;
        job.boxes[0] = Some(insert);
        job.order = [0, NO_STAGE, NO_STAGE, NO_STAGE];
        job.fed = true;
        guard.post(kind, bus);
    }

    #[test]
    fn each_posted_job_runs_once_when_the_callback_and_the_workers_race() {
        // Miri runs each step of the 4 threads, so the count is small there.
        let rounds: u64 = if cfg!(miri) { 2 } else { 100_000 };
        let (pool, mut handle) = ready_pool(3);
        // One handle at a time holds the pool, so one callback posts.
        assert!(PoolHandle::new(Arc::clone(&pool.shared)).is_none());
        // The workers of a stream spin between the rounds, except under
        // Miri, which counts each spin of a worker as a step.
        pool.shared.follow_stream(480, 48_000);
        pool.shared.set_power_saving(cfg!(miri));
        // An instrument job has 1 probe. An effect job runs stages 0 and 1
        // and skips stage 2, which is not in its order.
        let mut chains: Vec<Chain> = Vec::new();
        let mut runs = Vec::new();
        for slot in 0..JOB_SLOTS {
            let mut chain = Chain::default();
            let stages = match JobKind::of(slot) {
                JobKind::Instrument => 1,
                JobKind::Effect => 3,
            };
            for (stage, place) in chain.iter_mut().enumerate().take(stages) {
                let (insert, count) = probe(Duration::ZERO, false);
                *place = Some(insert);
                runs.push((slot, stage, count));
            }
            chains.push(chain);
        }
        let mut inline = 0;
        for round in 0..rounds {
            let frames = 1 + (round % 128) as usize;
            let input = (round % 1000) as f32 + 1.0;
            let mut guard = handle.begin();
            for (slot, chain) in chains.iter_mut().enumerate() {
                let (kind, bus) = (JobKind::of(slot), slot % MAX_INSERT_BUSES);
                let job = guard.job(kind, bus);
                job.frames = frames;
                for channel in &mut job.block {
                    channel[..frames].fill(input);
                }
                job.boxes = std::mem::take(chain);
                job.idle = [5; EFFECT_CHAIN];
                job.order = [0, 1, NO_STAGE, NO_STAGE];
                job.first = 0;
                job.fed = true;
                job.idle_limit = u32::MAX;
                guard.post(kind, bus);
            }
            if round == 0 {
                // A job that is out takes no second post and gives the
                // callback no reference. A join needs a job that is out.
                assert!(caught(|| guard.post(JobKind::Effect, 0)).is_err());
                assert!(caught(|| _ = guard.job(JobKind::Effect, 0)).is_err());
                assert!(caught(|| guard.join(JobKind::Effect, 0)).is_err());
                // A guard that never drops leaves its jobs out. The next
                // guard gives no reference to them and joins them.
                std::mem::forget(guard);
                guard = handle.begin();
                assert!(caught(|| _ = guard.job(JobKind::Effect, 0)).is_err());
            }
            for (kind, output) in [(JobKind::Instrument, 1.0), (JobKind::Effect, input + 2.0)] {
                let mut left = u64::MAX;
                while left != 0 {
                    let bus = guard.join(kind, left);
                    left &= !(1 << bus);
                    let job = guard.job(kind, bus);
                    for channel in &job.block {
                        assert!(
                            channel[..frames].iter().all(|sample| *sample == output),
                            "round {round}, {kind:?} job of bus {bus}"
                        );
                    }
                    if kind == JobKind::Effect {
                        assert_eq!(job.idle, [0, 0, 5, 5], "the stages that ran are fed");
                    }
                    inline += u64::from(job.worker.is_none());
                    chains[kind.slot(bus)] = std::mem::take(&mut job.boxes);
                }
            }
        }
        for (slot, stage, count) in &runs {
            let expected = if *stage == 2 { 0 } else { rounds };
            assert_eq!(
                count.load(Ordering::SeqCst),
                expected,
                "slot {slot}, stage {stage}"
            );
        }
        assert_eq!(handle.figures.inline_jobs, inline);
        drop(handle);
        assert!(PoolHandle::new(Arc::clone(&pool.shared)).is_some());
        drop(pool);
    }

    #[test]
    fn a_worker_that_parks_never_misses_a_post() {
        let rounds = if cfg!(miri) { 20 } else { 100_000 };
        let (pool, mut handle) = ready_pool(2);
        // A test pool has no stream, so a worker parks as soon as it has no
        // job.
        let (mut insert, runs) = probe(Duration::ZERO, false);
        for round in 0..rounds {
            // The bus moves, so the home worker of the post changes.
            let bus = round % MAX_INSERT_BUSES;
            let mut guard = handle.begin();
            post(&mut guard, JobKind::Instrument, bus, insert);
            // The callback runs no job here: only a worker that wakes for
            // the post ends it.
            let deadline = Instant::now() + Duration::from_secs(10);
            while guard.try_join(JobKind::Instrument, 1 << bus).is_none() {
                assert!(
                    Instant::now() < deadline,
                    "no worker woke for round {round}"
                );
                std::hint::spin_loop();
            }
            let job = guard.job(JobKind::Instrument, bus);
            assert!(job.worker.is_some());
            insert = job.boxes[0].take().expect("the probe");
            drop(guard);
            handle.end_buffer(0, 0, false);
        }
        assert_eq!(runs.load(Ordering::SeqCst), rounds as u64);
        drop(pool);
    }

    #[test]
    fn a_panic_in_a_job_reaches_the_join() {
        let (pool, mut handle) = ready_pool(2);
        let shared = Arc::clone(&pool.shared);
        let (slow, slow_runs) = probe(Duration::from_millis(20), false);
        let (fault, _) = probe(Duration::ZERO, true);
        let (leaving, leaving_runs) = probe(Duration::ZERO, false);
        let raised = caught(|| {
            let mut guard = handle.begin();
            // The slow job also holds a copy in a leaving place.
            guard.job(JobKind::Effect, 1).leaving[0] = Some(leaving);
            // Each job runs on a worker of its own.
            for (bus, insert) in [(1, slow), (2, fault)] {
                shared.claim_before_join(JobKind::Effect, bus);
                post(&mut guard, JobKind::Effect, bus, insert);
                while shared.posted[1].0.load(Ordering::SeqCst) != 0 {
                    thread::yield_now();
                }
            }
            guard.join(JobKind::Effect, 1 << 2);
        });
        let payload = raised.expect_err("the panic of the job reaches the join");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"a plugin fault"));
        // The guard waited for the slow job before the frame unwound, then
        // dropped the boxes of both jobs.
        assert_eq!(slow_runs.load(Ordering::SeqCst), 1);
        assert_eq!(Arc::strong_count(&slow_runs), 1);
        assert_eq!(Arc::strong_count(&leaving_runs), 1);
        // The next sub-block finds the pool clean.
        let (insert, runs) = probe(Duration::ZERO, false);
        let mut guard = handle.begin();
        post(&mut guard, JobKind::Instrument, 0, insert);
        assert_eq!(guard.join(JobKind::Instrument, 1), 0);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        guard.job(JobKind::Instrument, 0).boxes[0] = None;
    }

    #[test]
    fn stop_joins_each_worker_and_no_job_runs_after_it() {
        // 2 cores stay for the callback and the threads that are not real
        // time, so 2 cores start no pool. A pool has 8 workers at most.
        assert!(PluginPool::start_test_pool(workers_for(2), true).is_none());
        #[cfg(all(
            feature = "device-audio",
            any(windows, target_os = "macos", target_os = "linux")
        ))]
        assert!(cores().is_some_and(|cores| cores >= 1));
        // A list of CPUs holds each CPU of its ranges.
        assert!(cpu_listed("0-15\n", 15));
        assert!(!cpu_listed("0-15\n", 16));
        assert!(cpu_listed("0-3,8,10-11", 8));
        assert!(!cpu_listed("0-3,8,10-11", 9));
        // The affinity keeps its performance cores: the CPUs an Intel CPU
        // lists, else the CPUs of capacity 1024 on an Arm CPU. With no list
        // and no capacity, each CPU stays.
        #[cfg(all(feature = "device-audio", target_os = "linux"))]
        {
            // SAFETY: a CPU set is an array of bits, and all zeros is the
            // empty set.
            let mut affinity: libc::cpu_set_t = unsafe { std::mem::zeroed() };
            for cpu in (0..16).filter(|cpu| *cpu != 3) {
                // SAFETY: `cpu` is below `CPU_SETSIZE`, the bits of the set.
                unsafe { libc::CPU_SET(cpu, &mut affinity) };
            }
            let cpus = |set: libc::cpu_set_t| {
                (0..libc::CPU_SETSIZE as usize)
                    // SAFETY: as above.
                    .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, &set) })
                    .collect::<Vec<_>>()
            };
            let intel = performance_cpus_of(affinity, Some("0-7\n"), |_| None);
            assert_eq!(cpus(intel), [0, 1, 2, 4, 5, 6, 7]);
            let arm = performance_cpus_of(affinity, None, |cpu| {
                Some(if cpu < 4 { "1024\n" } else { "512\n" }.to_owned())
            });
            assert_eq!(cpus(arm), [0, 1, 2]);
            let one_kind = performance_cpus_of(affinity, None, |_| None);
            assert_eq!(cpus(one_kind), cpus(affinity));
        }
        for (cores, workers) in [(3, 1), (12, MAX_PLUGIN_WORKERS)] {
            let (pool, mut handle) = ready_pool(workers_for(cores));
            assert_eq!(pool.threads.len(), workers);
            let shared = Arc::clone(&pool.shared);
            let mut guard = handle.begin();
            // Each worker runs a job when the stop comes.
            let mut runs = Vec::new();
            for bus in 0..workers {
                let (insert, count) = probe(Duration::from_millis(5), false);
                shared.claim_before_join(JobKind::Effect, bus);
                post(&mut guard, JobKind::Effect, bus, insert);
                runs.push(count);
            }
            while shared.posted[1].0.load(Ordering::SeqCst) != 0 {
                thread::yield_now();
            }
            drop(pool);
            // The stop waited for the job of each worker, and each worker
            // ended.
            assert_eq!(
                shared.done[1].0.load(Ordering::SeqCst).count_ones() as usize,
                workers
            );
            assert!(runs.iter().all(|count| count.load(Ordering::SeqCst) == 1));
            assert_eq!(shared.ready.load(Ordering::SeqCst), 0);
            assert_eq!(shared.idle.0.load(Ordering::SeqCst), 0);
            // A job posted after the stop waits for no worker. Its join runs
            // it on the callback at once, before a cheaper open job.
            let (insert, after) = probe(Duration::ZERO, false);
            post(&mut guard, JobKind::Instrument, 0, insert);
            let (cheap, cheap_runs) = probe(Duration::ZERO, false);
            shared.slots[JobKind::Instrument.slot(0)]
                .cost
                .store(2, Ordering::Relaxed);
            shared.slots[JobKind::Effect.slot(workers)]
                .cost
                .store(1, Ordering::Relaxed);
            post(&mut guard, JobKind::Effect, workers, cheap);
            thread::sleep(Duration::from_millis(20));
            assert_eq!(after.load(Ordering::SeqCst), 0, "a job ran after the stop");
            assert_eq!(guard.join(JobKind::Instrument, 1), 0);
            assert_eq!(
                cheap_runs.load(Ordering::SeqCst),
                0,
                "the join ran a cheaper job first"
            );
            assert_eq!(guard.job(JobKind::Instrument, 0).worker, None);
            guard.job(JobKind::Instrument, 0).boxes[0] = None;
            for _ in 0..=workers {
                let bus = guard.join(JobKind::Effect, guard.pool.out[1]);
                guard.job(JobKind::Effect, bus).boxes[0] = None;
            }
        }
    }

    #[test]
    fn a_late_job_cools_its_worker_down() {
        // Where the system gives the ticks of a thread, they move while the
        // thread spins.
        if let Some(start) = thread_ticks() {
            let deadline = Instant::now() + Duration::from_secs(1);
            while thread_ticks().is_none_or(|now| now <= start) {
                assert!(Instant::now() < deadline, "the thread ticks did not move");
                std::hint::spin_loop();
            }
        }
        let ticks = Arc::new(AtomicU64::new(0));
        // A job of `spin` of work and a `pause` off the CPU on the worker of
        // `handle`. The callback joins it at once, or once it is done when
        // `done` is true. Gives the late jobs so far.
        let run = |handle: &mut PoolHandle, spin: u64, pause: u64, done: bool| {
            let insert = Box::new(Probe {
                spin: Duration::from_millis(spin),
                pause: Duration::from_millis(pause),
                ticks: Some(Arc::clone(&ticks)),
                ..Probe::default()
            });
            let mut guard = handle.begin();
            post(&mut guard, JobKind::Instrument, 0, insert);
            let deadline = Instant::now() + Duration::from_secs(5);
            while done && guard.try_join(JobKind::Instrument, 1).is_none() {
                assert!(Instant::now() < deadline, "the job did not end");
                thread::sleep(Duration::from_millis(1));
            }
            if !done {
                guard.join(JobKind::Instrument, 1);
            }
            guard.job(JobKind::Instrument, 0).boxes[0] = None;
            drop(guard);
            handle.figures.late_jobs
        };
        // A pool whose late rule is off, as on Windows on Arm, reads no
        // ticks. A pause the callback waits for is not late, and the worker
        // does not sit out.
        let pool = PluginPool::spawn(1, 0, false, || Some(())).expect("a pool");
        while pool.ready_workers() == 0 {
            thread::yield_now();
        }
        let mut handle = PoolHandle::new(Arc::clone(&pool.shared)).expect("one handle");
        let shared = &pool.shared;
        shared.claim_before_join(JobKind::Instrument, 0);
        assert_eq!(run(&mut handle, 1, 0, false), 0);
        assert_eq!(run(&mut handle, 0, 40, false), 0);
        assert!(handle.figures.join_wait_nanos >= 40_000_000);
        assert_eq!(shared.ticks_per_micro.load(Ordering::SeqCst), 0);
        assert_eq!(shared.cool_downs[0].0.until.load(Ordering::SeqCst), 0);
        drop(pool);
        // A pool of a device reads the late rule from the machine.
        #[cfg(all(feature = "device-audio", windows))]
        {
            let pool = PluginPool::start(1, 480, 48_000).expect("a pool");
            assert_eq!(pool.shared.late_rule, crate::mmcss::cycles_at_fixed_rate());
        }
        // The probes the worker runs give the ticks of this pool, so its
        // late rule reads no thread time of the system.
        let (pool, mut handle) = ready_pool(1);
        let shared = Arc::clone(&pool.shared);
        shared.give_ticks(Arc::clone(&ticks));
        shared.claim_before_join(JobKind::Instrument, 0);
        // Runs of 1 ms of work give the pool the ticks of a microsecond.
        for _ in 0..8 {
            assert_eq!(run(&mut handle, 1, 0, false), 0);
        }
        assert!(shared.ticks_per_micro.load(Ordering::SeqCst) > 0);
        // A run of work is not late, also at three times the last runs
        // while the callback waits for it: the plugin needs that time.
        assert_eq!(run(&mut handle, 3, 0, false), 0);
        // A pause the callback does not wait for is not late.
        assert_eq!(run(&mut handle, 0, 12, true), 0);
        // A pause the callback waits for is late: the system did not run the
        // worker. Its worker sits out for 1 s.
        assert_eq!(run(&mut handle, 0, 40, false), 1);
        assert!(handle.figures.join_wait_nanos >= 40_000_000);
        let cool = &shared.cool_downs[0].0;
        let left = cool.until.load(Ordering::SeqCst) - shared.now();
        assert!((900_000_000..=COOL_DOWN_NANOS).contains(&left), "{left} ns");
        // The worker takes no job while it cools down: the callback runs it.
        while shared.idle.0.load(Ordering::SeqCst) != 0 {
            thread::yield_now();
        }
        let (insert, runs) = probe(Duration::ZERO, false);
        let mut guard = handle.begin();
        post(&mut guard, JobKind::Instrument, 1, insert);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "a cooling worker took a job"
        );
        assert_eq!(guard.join(JobKind::Instrument, 1 << 1), 1);
        assert_eq!(guard.job(JobKind::Instrument, 1).worker, None);
        guard.job(JobKind::Instrument, 1).boxes[0] = None;
        // After its cool-down the worker takes jobs again.
        let insert = Box::new(Probe {
            ticks: Some(Arc::clone(&ticks)),
            ..Probe::default()
        });
        post(&mut guard, JobKind::Instrument, 0, insert);
        let deadline = Instant::now() + Duration::from_secs(5);
        while guard.try_join(JobKind::Instrument, 1).is_none() {
            assert!(Instant::now() < deadline, "the worker did not come back");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(shared.now() >= cool.until.load(Ordering::SeqCst));
        assert_eq!(guard.job(JobKind::Instrument, 0).worker, Some(0));
        guard.job(JobKind::Instrument, 0).boxes[0] = None;
        drop(guard);
        // A slow job that ends while another job of its set is out is not
        // late. The last job of the set takes each spin of the set. It has
        // no pause, so the pool finds no lost time in it.
        shared.claim_before_join(JobKind::Instrument, 2);
        let late = handle.figures.late_jobs;
        let mut guard = handle.begin();
        for (bus, spin, pause) in [(0, 0, 40), (2, 0, 0)] {
            let insert = Box::new(Probe {
                spin: Duration::from_millis(spin),
                pause: Duration::from_millis(pause),
                ticks: Some(Arc::clone(&ticks)),
                ..Probe::default()
            });
            post(&mut guard, JobKind::Instrument, bus, insert);
        }
        for (buses, bus) in [(0b101, 0), (0b100, 2)] {
            assert_eq!(guard.join(JobKind::Instrument, buses), bus);
            guard.job(JobKind::Instrument, bus).boxes[0] = None;
        }
        drop(guard);
        assert_eq!(handle.figures.late_jobs, late);
        // Each late job doubles the cool-down, up to 30 s. A worker 10 s past
        // its cool-down starts again at 1 s.
        let mut now = cool.until.load(Ordering::SeqCst);
        for seconds in [2, 4, 8, 16, 30, 30] {
            shared.cool_down(0, now);
            assert_eq!(
                cool.length.load(Ordering::SeqCst),
                seconds * COOL_DOWN_NANOS
            );
            now = cool.until.load(Ordering::SeqCst);
        }
        shared.cool_down(0, now + COOL_DOWN_RESET_NANOS);
        assert_eq!(cool.length.load(Ordering::SeqCst), COOL_DOWN_NANOS);
        drop(pool);
    }

    /// A pool reads the power mode only while a worker takes jobs. The first
    /// turn then reads it, and a turn within a second keeps that read. A
    /// worker spins for one sub-block of its stream, and not at all in a
    /// power-saving mode or with no stream.
    #[test]
    fn a_pool_reads_the_power_mode_once_a_second() {
        let mut idle = PluginPool::start_without_class(1).expect("a pool");
        idle.follow_power_mode();
        assert_eq!(idle.power_read, None);
        let mut pool = PluginPool::start_test_pool(1, false).expect("a pool");
        pool.follow_power_mode();
        let read = pool.power_read.expect("the first turn reads");
        pool.follow_power_mode();
        assert_eq!(pool.power_read, Some(read));
        let shared = &pool.shared;
        shared.set_power_saving(false);
        assert_eq!(shared.spin_nanos(), 0, "a pool with no stream spins");
        // 128 frames last 2.67 ms at 48 kHz and 2.90 ms at 44.1 kHz.
        shared.follow_stream(480, 48_000);
        assert_eq!(shared.spin_nanos(), 2_666_666);
        shared.follow_stream(512, 44_100);
        assert_eq!(shared.spin_nanos(), 2_902_494);
        // At 8 Hz a sub-block lasts 16 s, longer than the wait for the park
        // below. After its job the worker spins for the next one while the
        // callback is inside its buffer.
        shared.follow_stream(480, 8);
        let mut handle = PoolHandle::new(Arc::clone(shared)).expect("one handle");
        shared.claim_before_join(JobKind::Instrument, 0);
        let (insert, _) = probe(Duration::ZERO, false);
        let mut guard = handle.begin();
        post(&mut guard, JobKind::Instrument, 0, insert);
        guard.join(JobKind::Instrument, 1);
        guard.job(JobKind::Instrument, 0).boxes[0] = None;
        drop(guard);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(
            shared.parked.0.load(Ordering::SeqCst),
            0,
            "the worker parked"
        );
        // A power-saving mode parks the worker at its next turn.
        shared.set_power_saving(true);
        assert_eq!(shared.spin_nanos(), 0, "a power-saving mode spins");
        let deadline = Instant::now() + Duration::from_secs(10);
        while shared.parked.0.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "the worker spun in a power-saving mode"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
}
