//! Measures how fast a parked real-time worker wakes, and how the system
//! serves real-time threads under load. Plays no audio.
//!
//! Build with `cargo build --profile local -p rustel-audio --example worker_probe`.
//! Run `worker_probe --threads=4 --duty=50 --seconds=10`.
//! One periodic thread stands for the audio callback. At the start of each
//! period it wakes a parked worker and spins until the worker answers. A
//! period that ends after its deadline is a miss.
//! `--threads=4` adds 4 threads that work for `--duty=50` percent of each
//! period, as busy plugin workers do. A duty of 100 on each core leaves the
//! rest of the system little time.
//! `--frames=128` and `--rate=48000` set the period.
//! Each thread asks for the real-time class of a plugin worker: MMCSS
//! "Pro Audio" at the critical priority on Windows, and the class of the
//! `audio_thread_priority` crate on macOS and Linux. A kernel event wakes
//! the worker on Windows, and the park of the standard library elsewhere.
//! On Windows the probe also reads the efficiency class of the core under
//! each thread at each period.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the probe prints its measurements"
)]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Barrier, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Options {
    threads: usize,
    duty: u32,
    frames: u32,
    rate: u32,
    seconds: f64,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            threads: 0,
            duty: 100,
            frames: 128,
            rate: 48_000,
            seconds: 10.0,
        };
        for arg in args {
            let Some((name, value)) = arg.split_once('=') else {
                return Err(format!("unknown option {arg}, use --name=value"));
            };
            match name {
                "--threads" => options.threads = value.parse().map_err(|_| "invalid threads")?,
                "--duty" => options.duty = value.parse().map_err(|_| "invalid duty")?,
                "--frames" => options.frames = value.parse().map_err(|_| "invalid frames")?,
                "--rate" => options.rate = value.parse().map_err(|_| "invalid rate")?,
                "--seconds" => {
                    options.seconds = value.parse().map_err(|_| "invalid seconds")?;
                }
                _ => return Err(format!("unknown option {name}")),
            }
        }
        if options.threads > 64
            || options.duty > 100
            || !(16..=4096).contains(&options.frames)
            || !(8_000..=192_000).contains(&options.rate)
            || !(0.1..=600.0).contains(&options.seconds)
        {
            return Err("use threads 0..64, duty 0..100, frames 16..4096, rate 8000..192000 and seconds 0.1..600".into());
        }
        Ok(options)
    }
}

/// Asks for the real-time class of a plugin worker for the calling thread.
/// `None` when the system refuses. The thread keeps the value to its end.
#[cfg(windows)]
fn real_time(_frames: u32, _rate: u32) -> Option<impl Sized> {
    rustel_audio::mmcss::ProAudioThread::attach_worker()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn real_time(frames: u32, rate: u32) -> Option<impl Sized> {
    audio_thread_priority::promote_current_thread_to_real_time(frames, rate).ok()
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn real_time(_frames: u32, _rate: u32) -> Option<impl Sized> {
    None::<()>
}

/// The wake of the parked worker: a kernel event that resets itself. A
/// signal before the wait stays set.
#[cfg(windows)]
mod wake {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, INFINITE, SetEvent, WaitForSingleObject,
    };

    pub const KIND: &str = "kernel event";

    pub struct Wake(HANDLE);

    // SAFETY: an event handle is valid on each thread of the process, and
    // the kernel orders the calls on one event.
    unsafe impl Send for Wake {}
    unsafe impl Sync for Wake {}

    impl Wake {
        pub fn new() -> Self {
            // SAFETY: an event with no security attributes and no name,
            // which resets itself at each wake and starts unset.
            let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
            assert!(!event.is_null(), "the system made no event");
            Self(event)
        }

        /// A kernel event needs no name of the thread that waits.
        pub fn set_waiter(&self, _waiter: &std::thread::Thread) {}

        pub fn signal(&self) {
            // SAFETY: the handle is open until the drop.
            unsafe { SetEvent(self.0) };
        }

        pub fn wait(&self) {
            // SAFETY: as above.
            unsafe { WaitForSingleObject(self.0, INFINITE) };
        }
    }

    impl Drop for Wake {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateEventW`, and the thread
            // that waits on it ended before the drop.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// The wake of the parked worker: a flag, and the park of the standard
/// library.
#[cfg(not(windows))]
mod wake {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::Thread;

    pub const KIND: &str = "park";

    pub struct Wake {
        waiter: OnceLock<Thread>,
        posted: AtomicBool,
    }

    impl Wake {
        pub fn new() -> Self {
            Self {
                waiter: OnceLock::new(),
                posted: AtomicBool::new(false),
            }
        }

        /// Names the thread that waits. The thread that signals names it
        /// before its first signal.
        pub fn set_waiter(&self, waiter: &Thread) {
            let _ = self.waiter.set(waiter.clone());
        }

        pub fn signal(&self) {
            self.posted.store(true, Ordering::Release);
            if let Some(waiter) = self.waiter.get() {
                waiter.unpark();
            }
        }

        pub fn wait(&self) {
            // An unpark before the park makes the park return at once.
            while !self.posted.swap(false, Ordering::Acquire) {
                std::thread::park();
            }
        }
    }
}

/// The efficiency class of each logical processor, read one time.
#[cfg(windows)]
mod cores {
    use windows_sys::Win32::System::Kernel::PROCESSOR_NUMBER;
    use windows_sys::Win32::System::Threading::GetCurrentProcessorNumberEx;

    pub struct Cores {
        /// The class of each logical processor, at its group x 64 + its
        /// number.
        classes: Vec<u8>,
        /// The highest class: the class of the performance cores.
        top: u8,
        /// The physical cores of the highest class, and of the others.
        performance: usize,
        other: usize,
    }

    impl Cores {
        pub fn read() -> Option<Self> {
            let cores = rustel_audio::mmcss::processor_cores()?;
            let top = cores.iter().map(|core| core.EfficiencyClass).max()?;
            let mut classes = Vec::new();
            for core in &cores {
                // A core sits in one processor group.
                let group = core.GroupMask[0];
                for bit in 0..usize::BITS as usize {
                    if (group.Mask >> bit) & 1 == 1 {
                        let index = usize::from(group.Group) * 64 + bit;
                        if classes.len() <= index {
                            classes.resize(index + 1, 0);
                        }
                        classes[index] = core.EfficiencyClass;
                    }
                }
            }
            let performance = cores
                .iter()
                .filter(|core| core.EfficiencyClass == top)
                .count();
            Some(Self {
                classes,
                top,
                performance,
                other: cores.len() - performance,
            })
        }

        /// True when the calling thread runs on a performance core now.
        pub fn on_performance_core(&self) -> bool {
            let mut number = PROCESSOR_NUMBER {
                Group: 0,
                Number: 0,
                Reserved: 0,
            };
            // SAFETY: the call writes the processor of the calling thread.
            unsafe { GetCurrentProcessorNumberEx(&mut number) };
            let index = usize::from(number.Group) * 64 + usize::from(number.Number);
            self.classes.get(index) == Some(&self.top)
        }

        /// The physical cores of the highest class, and of the others.
        pub fn counts(&self) -> (usize, usize) {
            (self.performance, self.other)
        }
    }
}

/// The other systems give the probe no efficiency class: the type has no
/// value.
#[cfg(not(windows))]
mod cores {
    pub enum Cores {}

    impl Cores {
        pub fn read() -> Option<Self> {
            None
        }

        pub fn on_performance_core(&self) -> bool {
            match *self {}
        }

        pub fn counts(&self) -> (usize, usize) {
            match *self {}
        }
    }
}

/// How many periods of one kind of thread ran on a performance core.
#[derive(Default)]
struct Placement {
    samples: AtomicU64,
    on_performance: AtomicU64,
}

impl Placement {
    fn sample(&self, cores: Option<&cores::Cores>) {
        if let Some(cores) = cores {
            let on = u64::from(cores.on_performance_core());
            self.samples.fetch_add(1, Ordering::Relaxed);
            self.on_performance.fetch_add(on, Ordering::Relaxed);
        }
    }

    /// The share of the periods on a performance core. `None` with no
    /// period.
    fn share(&self) -> Option<f64> {
        let samples = self.samples.load(Ordering::Relaxed);
        let on = self.on_performance.load(Ordering::Relaxed);
        (samples > 0).then(|| on as f64 / samples as f64)
    }
}

/// Sleeps until `due`. Past `due` the thread yields one time, so a thread
/// of the same class on its core gets a turn.
fn sleep_until(due: Instant) {
    match due.checked_duration_since(Instant::now()) {
        Some(wait) => thread::sleep(wait),
        None => thread::yield_now(),
    }
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}

/// The median, the 99th percentile and the largest of `values`.
fn distribution(mut values: Vec<f64>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let last = values.len() - 1;
    json!({
        "p50": values[values.len() / 2],
        "p99": values[last * 99 / 100],
        "max": values[last],
    })
}

fn run() -> Result<(), String> {
    let Options {
        threads,
        duty,
        frames,
        rate,
        seconds,
    } = Options::parse(std::env::args().skip(1))?;
    let period = Duration::from_secs_f64(f64::from(frames) / f64::from(rate));
    let periods = (seconds / period.as_secs_f64()).ceil() as u32;
    let cores = cores::Cores::read();
    let cores = cores.as_ref();
    let wake = wake::Wake::new();
    // Each thread meets the others two times: when each one has its class,
    // and when the start of the first period is known.
    let ready = Barrier::new(threads + 2);
    let start = OnceLock::new();
    let stop = AtomicBool::new(false);
    let answered = AtomicBool::new(false);
    let origin = Instant::now();
    let nanos = |at: Instant| at.duration_since(origin).as_nanos() as u64;
    // The time of the last wake of the worker, in nanoseconds from `origin`.
    let woke = AtomicU64::new(0);
    let worker_class = AtomicBool::new(false);
    let busy_classes = AtomicUsize::new(0);
    // The callback, the worker and the busy threads.
    let places: [Placement; 3] = Default::default();

    let (callback_class, wakes, lates, misses) = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let class = real_time(frames, rate);
            worker_class.store(class.is_some(), Ordering::Relaxed);
            ready.wait();
            ready.wait();
            loop {
                wake.wait();
                let at = Instant::now();
                if stop.load(Ordering::Acquire) {
                    break;
                }
                woke.store(nanos(at), Ordering::Relaxed);
                answered.store(true, Ordering::Release);
                places[1].sample(cores);
            }
        });
        wake.set_waiter(worker.thread());
        for _ in 0..threads {
            scope.spawn(|| {
                let class = real_time(frames, rate);
                busy_classes.fetch_add(usize::from(class.is_some()), Ordering::Relaxed);
                ready.wait();
                ready.wait();
                let start = *start.get().expect("the start of the first period");
                let mut index = 0;
                while !stop.load(Ordering::Relaxed) {
                    let due = start + period * index;
                    sleep_until(due);
                    let end = due + period * duty / 100;
                    while Instant::now() < end {
                        std::hint::spin_loop();
                    }
                    places[2].sample(cores);
                    index += 1;
                }
            });
        }

        // This thread stands for the audio callback.
        let class = real_time(frames, rate);
        ready.wait();
        let first = Instant::now() + Duration::from_millis(20);
        start.set(first).expect("one start");
        ready.wait();
        let mut wakes = Vec::with_capacity(periods as usize);
        let mut lates = Vec::with_capacity(periods as usize);
        let mut misses = 0u32;
        for index in 0..periods {
            let due = first + period * index;
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
            lates.push(micros(Instant::now().saturating_duration_since(due)));
            let posted = Instant::now();
            wake.signal();
            while !answered.swap(false, Ordering::Acquire) {
                std::hint::spin_loop();
            }
            let wake_nanos = woke.load(Ordering::Relaxed).saturating_sub(nanos(posted));
            wakes.push(wake_nanos as f64 / 1_000.0);
            misses += u32::from(Instant::now() > due + period);
            places[0].sample(cores);
        }
        stop.store(true, Ordering::Release);
        wake.signal();
        (class.is_some(), wakes, lates, misses)
    });

    println!(
        "{}",
        json!({
            "kind": "worker_probe",
            "system": std::env::consts::OS,
            "wake": wake::KIND,
            "frames": frames,
            "rate": rate,
            "period_us": micros(period),
            "threads": threads,
            "duty": duty,
            "periods": periods,
            "real_time": {
                "callback": callback_class,
                "worker": worker_class.load(Ordering::Relaxed),
                "threads": busy_classes.load(Ordering::Relaxed),
            },
            "wake_us": distribution(wakes),
            "start_late_us": distribution(lates),
            "deadline_misses": misses,
            "cores": cores.map(|cores| {
                let (performance, other) = cores.counts();
                json!({ "performance": performance, "other": other })
            }),
            "on_performance_cores": cores.map(|_| json!({
                "callback": places[0].share(),
                "worker": places[1].share(),
                "threads": places[2].share(),
            })),
        })
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn options_out_of_range_are_refused() {
        let set = options(&["--threads=4", "--duty=50", "--frames=480", "--seconds=2"]).unwrap();
        assert_eq!(
            (set.threads, set.duty, set.frames, set.rate),
            (4, 50, 480, 48_000)
        );
        assert_eq!(set.seconds, 2.0);
        for arg in [
            "--threads=65",
            "--duty=101",
            "--frames=15",
            "--rate=7999",
            "--seconds=0",
            "--seconds=NaN",
            "--threads",
            "--unknown=1",
        ] {
            assert!(options(&[arg]).is_err(), "{arg}");
        }
    }
}
