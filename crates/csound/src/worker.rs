use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rustel_audio::{InsertKey, InsertParam, OrbitInsert};

use super::{Program, engine::Engine};

const IDLE: u8 = 0;
const REQUEST: u8 = 1;
const COMPLETE: u8 = 2;
const DEAD: u8 = 3;
const BLOCK: usize = 128;

#[derive(Clone, Copy)]
enum Command {
    Score(u64, u32),
    Cut(u32),
    Reset,
    Render(usize),
    Stop,
}

struct Work {
    command: Command,
    left: [f32; BLOCK],
    right: [f32; BLOCK],
}

struct Mailbox {
    state: AtomicU8,
    work: UnsafeCell<Work>,
}

// One producer owns Work in IDLE or COMPLETE. The worker owns it only in
// REQUEST. Release/acquire transitions transfer exclusive access each time.
unsafe impl Sync for Mailbox {}

impl Mailbox {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(IDLE),
            work: UnsafeCell::new(Work {
                command: Command::Reset,
                left: [0.0; BLOCK],
                right: [0.0; BLOCK],
            }),
        }
    }

    fn wait(&self) -> bool {
        let mut spins = 0;
        loop {
            match self.state.load(Ordering::Acquire) {
                COMPLETE => return true,
                DEAD => return false,
                _ => pause(&mut spins),
            }
        }
    }
}

fn pause(spins: &mut u8) {
    if *spins < 64 {
        *spins += 1;
        std::hint::spin_loop();
    } else {
        thread::yield_now();
    }
}

struct Exit(Arc<Mailbox>);

impl Drop for Exit {
    fn drop(&mut self) {
        self.0.state.store(DEAD, Ordering::Release);
    }
}

struct Active(Arc<AtomicUsize>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// All callback operations transfer fixed data to one synthesis thread.
pub(super) struct Insert {
    key: InsertKey,
    mailbox: Arc<Mailbox>,
    worker: Option<JoinHandle<()>>,
    pending: bool,
    failed: bool,
}

impl Insert {
    pub(super) fn new(program: Program, sample_rate: u32) -> Result<Self, String> {
        let key = program.key();
        let mailbox = Arc::new(Mailbox::new());
        let shared = mailbox.clone();
        let (ready, startup) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("csound".into())
            .spawn(move || {
                program.instances.fetch_add(1, Ordering::Relaxed);
                let _active = Active(program.instances.clone());
                let _exit = Exit(shared.clone());
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
                    engine: Some(engine),
                    pending: Vec::new(),
                    pending_bytes: 0,
                    clock: 0,
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                state.run(&shared);
            })
            .map_err(|error| format!("Csound worker could not start: {error}"))?;
        match startup.recv() {
            Ok(Ok(())) => Ok(Self {
                key,
                mailbox,
                worker: Some(worker),
                pending: false,
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

    fn submit_command(&mut self, command: Command) {
        self.finish(None);
        if self.failed || self.mailbox.state.load(Ordering::Acquire) == DEAD {
            self.failed = true;
            return;
        }
        // No request is pending. The worker cannot access Work until REQUEST.
        unsafe { (*self.mailbox.work.get()).command = command };
        self.pending = true;
        self.mailbox.state.store(REQUEST, Ordering::Release);
    }

    fn finish(&mut self, output: Option<(&mut [f32], &mut [f32])>) {
        if !self.pending {
            if let Some((left, right)) = output {
                left.fill(0.0);
                right.fill(0.0);
            }
            return;
        }
        self.pending = false;
        if !self.mailbox.wait() {
            self.failed = true;
            if let Some((left, right)) = output {
                left.fill(0.0);
                right.fill(0.0);
            }
            return;
        }
        if let Some((left, right)) = output {
            // COMPLETE transfers Work back to this thread until IDLE/REQUEST.
            let work = unsafe { &*self.mailbox.work.get() };
            left.copy_from_slice(&work.left[..left.len()]);
            right.copy_from_slice(&work.right[..right.len()]);
        }
        self.mailbox.state.store(IDLE, Ordering::Release);
    }

    fn command(&mut self, command: Command) {
        self.submit_command(command);
        self.finish(None);
    }
}

impl OrbitInsert for Insert {
    fn key(&self) -> InsertKey {
        self.key
    }

    fn set_param(&mut self, _: InsertParam, _: u32) {}

    fn score(&mut self, id: u64, frames: u32) {
        self.command(Command::Score(id, frames));
    }

    fn cut_notes(&mut self, frames: u32) {
        self.command(Command::Cut(frames));
    }

    fn reset(&mut self) {
        self.command(Command::Reset);
    }

    fn busy(&self) -> bool {
        !self.failed && self.mailbox.state.load(Ordering::Acquire) != DEAD
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.submit(left, right);
        self.collect(left, right);
    }

    fn submit(&mut self, left: &mut [f32], right: &mut [f32]) {
        assert_eq!(left.len(), right.len());
        assert!(left.len() <= BLOCK);
        self.submit_command(Command::Render(left.len()));
    }

    fn collect(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.finish(Some((left, right)));
    }
}

impl Drop for Insert {
    fn drop(&mut self) {
        self.command(Command::Stop);
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
    engine: Option<Engine>,
    pending: Vec<Scheduled>,
    pending_bytes: usize,
    clock: u64,
}

impl State {
    fn run(&mut self, mailbox: &Mailbox) {
        let mut spins = 0;
        loop {
            if mailbox.state.load(Ordering::Acquire) != REQUEST {
                if spins < 1024 {
                    spins += 1;
                    std::hint::spin_loop();
                } else {
                    thread::sleep(Duration::from_micros(50));
                }
                continue;
            }
            spins = 0;
            // REQUEST transfers Work from the producer until COMPLETE.
            let work = unsafe { &mut *mailbox.work.get() };
            let start = self.clock;
            let result = match work.command {
                Command::Score(id, frames) => match self.program.take(id) {
                    Some(score) => self.schedule(frames, Action::Score(score)),
                    None => Ok(()),
                },
                Command::Cut(frames) => self.schedule(frames, Action::Cut),
                Command::Reset => {
                    self.pending.clear();
                    self.pending_bytes = 0;
                    self.engine = None;
                    self.clock = 0;
                    Ok(())
                }
                Command::Render(frames) => self.render(work, frames),
                Command::Stop => return,
            };
            if let Err(error) = result {
                self.program.report_error(error);
                self.pending.clear();
                self.pending_bytes = 0;
                self.engine = None;
                work.left.fill(0.0);
                work.right.fill(0.0);
                if let Command::Render(frames) = work.command {
                    self.clock = start.saturating_add(frames as u64);
                }
            }
            mailbox.state.store(COMPLETE, Ordering::Release);
        }
    }

    fn schedule(&mut self, frames: u32, action: Action) -> Result<(), String> {
        let bytes = match &action {
            Action::Score(score) => score.len(),
            Action::Cut => 0,
        };
        if self.pending.len() >= super::MAX_EVENTS
            || bytes > super::MAX_EVENT_BYTES.saturating_sub(self.pending_bytes)
        {
            return Err("Csound scheduled event limit reached".into());
        }
        let frame = self.clock.saturating_add(u64::from(frames));
        let position = self.pending.partition_point(|event| event.frame <= frame);
        self.pending.insert(position, Scheduled { frame, action });
        self.pending_bytes += bytes;
        Ok(())
    }

    fn engine(&mut self) -> Result<&mut Engine, String> {
        if self.engine.is_none() {
            self.engine = Some(Engine::new(&self.program.sources, self.sample_rate)?);
        }
        Ok(self.engine.as_mut().unwrap())
    }

    fn render(&mut self, work: &mut Work, frames: usize) -> Result<(), String> {
        for index in 0..frames {
            while self
                .pending
                .first()
                .is_some_and(|event| event.frame <= self.clock)
            {
                match self.pending.remove(0).action {
                    Action::Score(score) => {
                        self.pending_bytes -= score.len();
                        self.engine()?.score(&score)?;
                    }
                    // Rebuilding also removes notes scheduled by an orchestra.
                    // Globals restart. Compilation runs on this worker thread.
                    Action::Cut => {
                        if self.engine.take().is_some() {
                            self.engine()?;
                        }
                    }
                }
            }
            let [left, right] = match &mut self.engine {
                Some(engine) => engine.frame()?,
                None => [0.0; 2],
            };
            work.left[index] = left;
            work.right[index] = right;
            self.clock = self.clock.saturating_add(1);
        }
        if let Some(engine) = &mut self.engine {
            engine.check_messages()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_transfers_exclusive_access_and_reports_worker_exit() {
        let mailbox = Arc::new(Mailbox::new());
        let shared = mailbox.clone();
        let worker = thread::spawn(move || {
            let _exit = Exit(shared.clone());
            for _ in 0..32 {
                while shared.state.load(Ordering::Acquire) != REQUEST {
                    thread::yield_now();
                }
                let work = unsafe { &mut *shared.work.get() };
                let Command::Render(length) = work.command else {
                    panic!("unexpected command")
                };
                work.left[..length].fill(length as f32);
                work.right[..length].fill(-(length as f32));
                shared.state.store(COMPLETE, Ordering::Release);
            }
            while shared.state.load(Ordering::Acquire) != REQUEST {
                thread::yield_now();
            }
        });
        for length in 1..=32 {
            unsafe { (*mailbox.work.get()).command = Command::Render(length) };
            mailbox.state.store(REQUEST, Ordering::Release);
            assert!(mailbox.wait());
            let work = unsafe { &*mailbox.work.get() };
            assert_eq!(work.left[length - 1], length as f32);
            assert_eq!(work.right[length - 1], -(length as f32));
            mailbox.state.store(IDLE, Ordering::Release);
        }
        mailbox.state.store(REQUEST, Ordering::Release);
        assert!(!mailbox.wait());
        worker.join().unwrap();
    }
}
