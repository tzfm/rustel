//! A plugin bundle in a process of its own.
//!
//! A plugin is native code. In the process of the host, a fault in a plugin
//! ends the host, and a live set with it. So a host with a worker program
//! runs each bundle in one more process, the worker, and talks to the
//! worker over local sockets:
//!
//! ```text
//!   host process                          worker process
//!   plugin thread ── control, JSON lines ─▶ main thread: load, build, end
//!   audio callback ─ one socket a copy ───▶ one thread a copy: process
//! ```
//!
//! The sockets are from the standard library, so the same code runs on each
//! system. Each exchange has a time limit. A worker that ends, or gives no
//! answer in the limit, takes its plugins out of the set and no more: an
//! effect passes its input, an instrument is silent, and the host starts
//! the bundle again at the next use.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use rustel_audio::{InsertKey, InsertNote, InsertParam, OrbitInsert};
use serde::{Deserialize, Serialize};

use crate::host::{WorkerProgram, build_instance, load_bundle};
use crate::instance::{Instance, MAX_BLOCK};
use crate::{Job, ParamInfo};

/// The worker reads the port and the token of its host from this variable.
const WORKER_ENV: &str = "RUSTEL_VST3_WORKER";
/// A worker connects to its host in this time.
const CONNECT_LIMIT: Duration = Duration::from_secs(20);
/// A load or a build that runs longer than this does not answer.
const WORK_LIMIT: Duration = Duration::from_secs(120);
/// A worker answers one audio block in this time. The audio callback waits
/// for the answer, so the limit is also the longest stop of the sound
/// before the host takes the plugin out.
const ANSWER_LIMIT: Duration = Duration::from_secs(1);
/// The limit for the first block of a copy: a plugin can do work in its
/// first block that it does one time only.
const FIRST_ANSWER_LIMIT: Duration = Duration::from_secs(5);
/// A worker that got the word to end has this long before the host ends
/// its process.
const QUIT_LIMIT: Duration = Duration::from_secs(2);
/// A worker sends its token right after it connects. A connection with no
/// token in this time is not the worker, and must not hold up the worker.
const HELLO_LIMIT: Duration = Duration::from_millis(250);
/// The events one copy holds for its next block: parameter values, notes
/// and clock values. One note with 8 parameter values is 10 events, so this
/// is room for 51 such notes in one block of the audio callback.
const MAX_EVENTS: usize = 512;
const EVENT_BYTES: usize = 24;
const BLOCK_BYTES: usize = MAX_BLOCK * 8;
const TOKEN_BYTES: usize = 16;

/// One plugin of a bundle, as its worker read the plugin.
#[derive(Serialize, Deserialize)]
pub(crate) struct RemotePlugin {
    pub name: String,
    pub vendor: String,
    pub categories: String,
    pub params: Vec<ParamInfo>,
}

#[derive(Serialize, Deserialize)]
enum Request {
    /// Make one running copy of the plugin at this place of the bundle.
    /// The preset is the content of its file.
    Build {
        plugin: usize,
        preset: Option<Vec<u8>>,
        sample_rate: u32,
        copy: u64,
    },
    Quit,
}

#[derive(Serialize, Deserialize)]
enum Answer {
    Loaded(Vec<RemotePlugin>),
    Built,
    Failed(String),
}

/// One thing a copy does before its next block. The numbers are the
/// arguments of the [`OrbitInsert`] call with the same name.
#[derive(Clone, Copy)]
enum Event {
    Param(InsertParam, u32),
    Sync(f64, f32, u32),
    Note(InsertNote, u32),
    Cut(u32),
    Reset,
    Restore(u32),
}

impl Event {
    fn encode(self) -> [u8; EVENT_BYTES] {
        let mut bytes = [0u8; EVENT_BYTES];
        let mut put = |at: usize, value: &[u8]| bytes[at..at + value.len()].copy_from_slice(value);
        match self {
            Self::Param(param, frames) => {
                put(0, &[1]);
                put(4, &frames.to_le_bytes());
                put(8, &param.id.to_le_bytes());
                put(12, &param.value.to_le_bytes());
            }
            Self::Sync(beats, tempo, frames) => {
                put(0, &[2]);
                put(4, &frames.to_le_bytes());
                put(8, &beats.to_le_bytes());
                put(16, &tempo.to_le_bytes());
            }
            Self::Note(note, frames) => {
                put(0, &[3]);
                put(4, &frames.to_le_bytes());
                put(8, &note.pitch.to_le_bytes());
                put(12, &note.velocity.to_le_bytes());
                put(16, &note.frames.to_le_bytes());
            }
            Self::Cut(frames) => {
                put(0, &[4]);
                put(4, &frames.to_le_bytes());
            }
            Self::Reset => put(0, &[5]),
            Self::Restore(frames) => {
                put(0, &[6]);
                put(4, &frames.to_le_bytes());
            }
        }
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let four = |at: usize| -> [u8; 4] { bytes[at..at + 4].try_into().expect("4 bytes") };
        let frames = u32::from_le_bytes(four(4));
        Some(match bytes[0] {
            1 => {
                let id = u32::from_le_bytes(four(8));
                let value = f32::from_le_bytes(four(12));
                Self::Param(InsertParam { id, value }, frames)
            }
            2 => {
                let beats = f64::from_le_bytes(bytes[8..16].try_into().expect("8 bytes"));
                Self::Sync(beats, f32::from_le_bytes(four(16)), frames)
            }
            3 => {
                let note = InsertNote {
                    pitch: f32::from_le_bytes(four(8)),
                    velocity: f32::from_le_bytes(four(12)),
                    frames: u32::from_le_bytes(four(16)),
                };
                Self::Note(note, frames)
            }
            4 => Self::Cut(frames),
            5 => Self::Reset,
            6 => Self::Restore(frames),
            _ => return None,
        })
    }

    fn apply(self, insert: &mut dyn OrbitInsert) {
        match self {
            Self::Param(param, frames) => insert.set_param(param, frames),
            Self::Sync(beats, tempo, frames) => insert.sync(beats, tempo, frames),
            Self::Note(note, frames) => insert.note(note, frames),
            Self::Cut(frames) => insert.cut_notes(frames),
            Self::Reset => insert.reset(),
            Self::Restore(frames) => insert.restore_params(frames),
        }
    }
}

fn samples_to_bytes(samples: &[f32], bytes: &mut [u8]) {
    for (sample, bytes) in samples.iter().zip(bytes.as_chunks_mut::<4>().0) {
        *bytes = sample.to_le_bytes();
    }
}

fn bytes_to_samples(bytes: &[u8], samples: &mut [f32]) {
    for (sample, bytes) in samples.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *sample = f32::from_le_bytes(*bytes);
    }
}

/// The worker: loads the bundle, serves its host, and returns the exit code
/// of the process. The worker program calls this with the bundle path the
/// host gave as the last argument. The worker ends when its host says so,
/// and when its host is gone.
pub fn serve(bundle: &Path) -> i32 {
    let Some((port, token)) = worker_address() else {
        return 2;
    };
    let connect = |copy: u64| -> std::io::Result<TcpStream> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        stream.set_nodelay(true)?;
        stream.write_all(&token)?;
        stream.write_all(&copy.to_le_bytes())?;
        Ok(stream)
    };
    let Ok(control) = connect(0) else {
        return 2;
    };
    let Ok(mut answers) = control.try_clone() else {
        return 2;
    };
    let mut answer = |answer: &Answer| {
        let mut line = serde_json::to_vec(answer).unwrap_or_default();
        line.push(b'\n');
        answers.write_all(&line).is_ok()
    };
    // This thread is the plugin thread of the bundle: each plugin call that
    // is not audio runs here.
    let plugins = match load_bundle(bundle) {
        Ok(plugins) => plugins,
        Err(reason) => {
            answer(&Answer::Failed(reason));
            return 0;
        }
    };
    let list = plugins
        .iter()
        .map(|plugin| RemotePlugin {
            name: plugin.name().to_owned(),
            vendor: plugin.vendor().to_owned(),
            categories: plugin.categories().to_owned(),
            params: plugin.params().to_vec(),
        })
        .collect();
    if !answer(&Answer::Loaded(list)) {
        return 0;
    }
    // A thread reads the requests, so this thread also ends the copies
    // their threads give back.
    let (asked, requests) = mpsc::channel::<Request>();
    std::thread::spawn(move || {
        for line in BufReader::new(control).lines() {
            // A line that is no request, and the end of the lines, end the
            // worker: the host is gone or not this host.
            let request = line.ok().and_then(|line| serde_json::from_str(&line).ok());
            let request = request.unwrap_or(Request::Quit);
            let last = matches!(request, Request::Quit);
            if asked.send(request).is_err() || last {
                break;
            }
        }
        let _ = asked.send(Request::Quit);
        // The main thread ends the worker in the ordinary way. A plugin
        // call with no end holds that thread, so the process ends here
        // after a moment: no worker stays after its host.
        std::thread::sleep(QUIT_LIMIT);
        std::process::exit(0);
    });
    let (retire, retired) = mpsc::channel::<Job>();
    let mut copies = Vec::new();
    loop {
        for job in retired.try_iter() {
            job();
        }
        match requests.recv_timeout(Duration::from_millis(10)) {
            Ok(Request::Build {
                plugin,
                preset,
                sample_rate,
                copy,
            }) => {
                let built = plugins
                    .get(plugin)
                    .ok_or_else(|| "the bundle has no such plugin".to_owned())
                    .and_then(|plugin| {
                        let key = InsertKey::default();
                        build_instance(plugin, preset.as_deref(), key, sample_rate, retire.clone())
                    })
                    .and_then(|instance| {
                        let stream = connect(copy).map_err(|error| error.to_string())?;
                        let thread = std::thread::Builder::new().name("rustel-vst3-copy".into());
                        thread
                            .spawn(move || serve_copy(instance, stream))
                            .map_err(|error| error.to_string())
                    });
                let sent = match built {
                    Ok(thread) => {
                        copies.push(thread);
                        answer(&Answer::Built)
                    }
                    Err(reason) => answer(&Answer::Failed(reason)),
                };
                if !sent {
                    break;
                }
            }
            Ok(Request::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        copies.retain(|copy| !copy.is_finished());
    }
    // The host closed the socket of each copy before the word to end. A
    // copy that still runs has a moment, then the process ends with it.
    let start = Instant::now();
    while copies.iter().any(|copy| !copy.is_finished()) && start.elapsed() < QUIT_LIMIT / 2 {
        std::thread::sleep(Duration::from_millis(5));
    }
    for job in retired.try_iter() {
        job();
    }
    if copies.iter().all(|copy| copy.is_finished()) {
        drop(plugins);
    }
    0
}

/// The port and the token from the variable of the worker.
fn worker_address() -> Option<(u16, [u8; TOKEN_BYTES])> {
    let address = std::env::var(WORKER_ENV).ok()?;
    let (port, hex) = address.split_once(':')?;
    let mut token = [0u8; TOKEN_BYTES];
    if hex.len() != TOKEN_BYTES * 2 || !hex.is_ascii() {
        return None;
    }
    for (byte, pair) in token.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some((port.parse().ok()?, token))
}

/// One copy in the worker: reads the events and the input of a block,
/// runs the plugin, and writes the output. The end of the socket ends the
/// copy, and the plugin goes back to the main thread of the worker.
fn serve_copy(mut instance: Instance, mut stream: TcpStream) {
    let mut message = [0u8; MAX_EVENTS * EVENT_BYTES + BLOCK_BYTES];
    let mut reply = [0u8; 1 + BLOCK_BYTES];
    let (mut left, mut right) = ([0f32; MAX_BLOCK], [0f32; MAX_BLOCK]);
    loop {
        let mut head = [0u8; 4];
        if stream.read_exact(&mut head).is_err() {
            return;
        }
        let frames = usize::from(u16::from_le_bytes([head[0], head[1]]));
        let events = usize::from(u16::from_le_bytes([head[2], head[3]]));
        if frames > MAX_BLOCK || events > MAX_EVENTS {
            return;
        }
        let (events, audio) = (events * EVENT_BYTES, frames * 8);
        if stream.read_exact(&mut message[..events + audio]).is_err() {
            return;
        }
        for event in message[..events].as_chunks::<EVENT_BYTES>().0 {
            if let Some(event) = Event::decode(event) {
                event.apply(&mut instance);
            }
        }
        let (input_left, input_right) = message[events..events + audio].split_at(frames * 4);
        bytes_to_samples(input_left, &mut left[..frames]);
        bytes_to_samples(input_right, &mut right[..frames]);
        instance.process(&mut left[..frames], &mut right[..frames]);
        reply[0] = u8::from(instance.busy());
        let (output_left, output_right) = reply[1..1 + audio].split_at_mut(frames * 4);
        samples_to_bytes(&left[..frames], output_left);
        samples_to_bytes(&right[..frames], output_right);
        if stream.write_all(&reply[..1 + audio]).is_err() {
            return;
        }
    }
}

/// True after a copy got no answer from its worker. The copies of the
/// worker then leave their signal as it is, and the host ends the worker.
#[derive(Default)]
pub(crate) struct Health(AtomicBool);

impl Health {
    fn ended(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// The host side of one worker process.
pub(crate) struct Worker {
    /// A number no other worker of this process has. The system gives a
    /// process number and a memory address to a new worker again.
    serial: u64,
    /// The process number of the worker.
    id: u32,
    child: Mutex<Child>,
    control: Mutex<(BufReader<TcpStream>, TcpStream)>,
    listener: TcpListener,
    token: [u8; TOKEN_BYTES],
    health: Arc<Health>,
    copies: AtomicU64,
}

impl Worker {
    /// Starts a worker for a bundle and waits for its plugins. `closed`
    /// ends the wait: the host ends.
    pub(crate) fn start(
        program: &WorkerProgram,
        bundle: &Path,
        closed: &AtomicBool,
    ) -> Result<(Arc<Self>, Vec<RemotePlugin>), String> {
        let text = |error: std::io::Error| error.to_string();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(text)?;
        listener.set_nonblocking(true).map_err(text)?;
        let port = listener.local_addr().map_err(text)?.port();
        let token = new_token();
        let hex: String = token.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut command = Command::new(&program.program);
        command
            .args(&program.args)
            .arg(bundle)
            .env(WORKER_ENV, format!("{port}:{hex}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // The worker leads a process group of its own: the processes a
        // plugin starts are in the group, so the system tells them from
        // the processes of the host, and Ctrl-C in the terminal goes to the
        // host only.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command
            .spawn()
            .map_err(|error| format!("the plugin process did not start: {error}"))?;
        let started = accept(&listener, &token, 0, &mut child, closed).and_then(|control| {
            let reader = control.try_clone().map_err(text)?;
            let mut control = (BufReader::new(reader), control);
            match read_answer(&mut control.0, &mut child, closed)? {
                Answer::Loaded(plugins) => Ok((control, plugins)),
                Answer::Failed(reason) => Err(reason),
                Answer::Built => Err("the plugin process gave a wrong answer".into()),
            }
        });
        match started {
            Ok((control, plugins)) => {
                static SERIAL: AtomicU64 = AtomicU64::new(0);
                let worker = Self {
                    serial: SERIAL.fetch_add(1, Ordering::Relaxed),
                    id: child.id(),
                    child: Mutex::new(child),
                    control: Mutex::new(control),
                    listener,
                    token,
                    health: Arc::default(),
                    copies: AtomicU64::new(0),
                };
                Ok((Arc::new(worker), plugins))
            }
            Err(reason) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(reason)
            }
        }
    }

    /// Makes one running copy of a plugin of the bundle. `preset` is the
    /// content of a preset file, `running` counts the copies of the plugin,
    /// and `closed` ends the wait: the host ends. A worker with no answer
    /// counts as ended, so the host starts the bundle again.
    pub(crate) fn build(
        &self,
        plugin: usize,
        preset: Option<Vec<u8>>,
        key: InsertKey,
        sample_rate: u32,
        running: Arc<AtomicUsize>,
        closed: &AtomicBool,
    ) -> Result<RemoteInstance, String> {
        let broken = |reason: String| {
            self.health.0.store(true, Ordering::Relaxed);
            reason
        };
        let text = |error: std::io::Error| error.to_string();
        let copy = self.copies.fetch_add(1, Ordering::Relaxed) + 1;
        let request = Request::Build {
            plugin,
            preset,
            sample_rate,
            copy,
        };
        let mut control = self.control.lock().expect("plugin process");
        let mut child = self.child.lock().expect("plugin process");
        let mut line = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        line.push(b'\n');
        control.1.write_all(&line).map_err(text).map_err(broken)?;
        match read_answer(&mut control.0, &mut child, closed).map_err(broken)? {
            Answer::Built => {}
            // The plugin said no: the worker is in order.
            Answer::Failed(reason) => return Err(reason),
            Answer::Loaded(_) => {
                return Err(broken("the plugin process gave a wrong answer".into()));
            }
        }
        let stream =
            accept(&self.listener, &self.token, copy, &mut child, closed).map_err(broken)?;
        stream
            .set_read_timeout(Some(FIRST_ANSWER_LIMIT))
            .map_err(text)?;
        stream.set_write_timeout(Some(ANSWER_LIMIT)).map_err(text)?;
        running.fetch_add(1, Ordering::Relaxed);
        Ok(RemoteInstance {
            key,
            stream,
            message: [0; 4 + MAX_EVENTS * EVENT_BYTES + BLOCK_BYTES],
            reply: [0; 1 + BLOCK_BYTES],
            event_count: 0,
            busy: false,
            first: true,
            health: Arc::clone(&self.health),
            order: 0,
            held: None,
            running,
        })
    }

    /// The process number of the worker.
    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    /// The number of this worker among the workers of the process.
    pub(crate) fn serial(&self) -> u64 {
        self.serial
    }

    /// Ends the process now. The host calls this for a worker that gave no
    /// answer: such a worker gets no time to end in the ordinary way.
    pub(crate) fn kill(&self) {
        let mut child = self.child.lock().expect("plugin process");
        let _ = child.kill();
        let _ = child.wait();
    }

    /// True after the process ended, or after a copy got no answer. A
    /// build checks the process itself. This probe does not wait for a build.
    pub(crate) fn ended(&self) -> bool {
        let exited = || match self.child.try_lock() {
            Ok(mut child) => !matches!(child.try_wait(), Ok(None)),
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Poisoned(_)) => panic!("plugin process"),
        };
        self.health.ended() || exited()
    }

    /// How the worker ended, for a notice: the exit status of its process,
    /// or no answer. A process that ends closes its sockets a moment before
    /// the system has its status, so the read waits 100 ms at most.
    pub(crate) fn end(&self) -> String {
        let mut child = self.child.lock().expect("plugin process");
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return format!("the plugin process ended ({status})"),
                Ok(None) if start.elapsed() < Duration::from_millis(100) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => return "the plugin process gave no answer".to_owned(),
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let child = self.child.get_mut().expect("plugin process");
        if let Ok(control) = self.control.get_mut() {
            let mut line = serde_json::to_vec(&Request::Quit).unwrap_or_default();
            line.push(b'\n');
            let _ = control.1.write_all(&line);
        }
        let start = Instant::now();
        while matches!(child.try_wait(), Ok(None)) && start.elapsed() < QUIT_LIMIT {
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// 16 bytes no other process knows. The keys of 2 hash states come from the
/// random source of the system.
fn new_token() -> [u8; TOKEN_BYTES] {
    use std::hash::{BuildHasher, Hasher};
    let mut token = [0u8; TOKEN_BYTES];
    for half in token.as_chunks_mut::<8>().0 {
        let state = std::collections::hash_map::RandomState::new();
        *half = state.build_hasher().finish().to_le_bytes();
    }
    token
}

/// Takes the connection of the worker with this token and this copy
/// number. A different process on the machine can reach the port, and has
/// no token.
fn accept(
    listener: &TcpListener,
    token: &[u8; TOKEN_BYTES],
    copy: u64,
    child: &mut Child,
    closed: &AtomicBool,
) -> Result<TcpStream, String> {
    let start = Instant::now();
    loop {
        // A different process can connect again and again with no token,
        // so each turn has these checks.
        if !matches!(child.try_wait(), Ok(None)) {
            return Err("the plugin process ended".into());
        }
        if closed.load(Ordering::Relaxed) || start.elapsed() > CONNECT_LIMIT {
            return Err("the plugin process did not connect".into());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut hello = [0u8; TOKEN_BYTES + 8];
                // On some systems the socket takes the mode of the listener.
                let ready = stream.set_nonblocking(false).is_ok()
                    && stream.set_nodelay(true).is_ok()
                    && stream.set_read_timeout(Some(HELLO_LIMIT)).is_ok()
                    && stream.read_exact(&mut hello).is_ok()
                    && stream.set_read_timeout(None).is_ok();
                let (theirs, number) = hello.split_at(TOKEN_BYTES);
                if ready && theirs == token && number == copy.to_le_bytes() {
                    return Ok(stream);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Reads one answer of the worker, in short waits, so the end of the
/// process and the end of the host end the wait.
fn read_answer(
    control: &mut BufReader<TcpStream>,
    child: &mut Child,
    closed: &AtomicBool,
) -> Result<Answer, String> {
    let ended = || "the plugin process ended".to_owned();
    let slice = Duration::from_millis(100);
    control
        .get_ref()
        .set_read_timeout(Some(slice))
        .map_err(|error| error.to_string())?;
    let start = Instant::now();
    // Bytes, not text: a wait can end in the middle of a character, and the
    // part that is here must stay for the next read.
    let mut line = Vec::new();
    loop {
        match control.read_until(b'\n', &mut line) {
            Ok(0) => return Err(ended()),
            Ok(_) if line.ends_with(b"\n") => {
                return serde_json::from_slice(&line).map_err(|_| ended());
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if !matches!(child.try_wait(), Ok(None)) {
                    return Err(ended());
                }
                if closed.load(Ordering::Relaxed) || start.elapsed() > WORK_LIMIT {
                    return Err("the plugin process did not answer".into());
                }
            }
            Err(_) => return Err(ended()),
        }
    }
}

/// A plugin copy in a worker, as the audio engine holds the copy. Each
/// call that changes the plugin waits in a list for the next block, and
/// goes to the worker with the input of the block.
pub(crate) struct RemoteInstance {
    key: InsertKey,
    stream: TcpStream,
    message: [u8; 4 + MAX_EVENTS * EVENT_BYTES + BLOCK_BYTES],
    reply: [u8; 1 + BLOCK_BYTES],
    event_count: usize,
    /// The answer of the worker to the last block.
    busy: bool,
    /// No block had its answer yet: see [`FIRST_ANSWER_LIMIT`].
    first: bool,
    health: Arc<Health>,
    order: u64,
    held: Option<Arc<()>>,
    running: Arc<AtomicUsize>,
}

impl RemoteInstance {
    pub(crate) fn order(&self) -> u64 {
        self.order
    }

    pub(crate) fn set_order(&mut self, order: u64) {
        self.order = order;
    }

    /// See [`Instance::hold`].
    pub(crate) fn hold(&mut self) -> std::sync::Weak<()> {
        let held = Arc::new(());
        let mark = Arc::downgrade(&held);
        self.held = Some(held);
        mark
    }

    fn push(&mut self, event: Event) {
        if self.event_count < MAX_EVENTS {
            let at = 4 + self.event_count * EVENT_BYTES;
            self.message[at..at + EVENT_BYTES].copy_from_slice(&event.encode());
            self.event_count += 1;
        }
    }

    /// One block to the worker and back. False when the worker gave no
    /// answer: the block stays as it is.
    fn exchange(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let frames = left.len();
        self.message[..2].copy_from_slice(&(frames as u16).to_le_bytes());
        self.message[2..4].copy_from_slice(&(self.event_count as u16).to_le_bytes());
        let at = 4 + self.event_count * EVENT_BYTES;
        samples_to_bytes(left, &mut self.message[at..at + frames * 4]);
        samples_to_bytes(right, &mut self.message[at + frames * 4..at + frames * 8]);
        if self
            .stream
            .write_all(&self.message[..at + frames * 8])
            .is_err()
        {
            return false;
        }
        if self
            .stream
            .read_exact(&mut self.reply[..1 + frames * 8])
            .is_err()
        {
            return false;
        }
        if self.first {
            self.first = false;
            let _ = self.stream.set_read_timeout(Some(ANSWER_LIMIT));
        }
        self.busy = self.reply[0] != 0;
        bytes_to_samples(&self.reply[1..1 + frames * 4], left);
        bytes_to_samples(&self.reply[1 + frames * 4..1 + frames * 8], right);
        true
    }
}

impl OrbitInsert for RemoteInstance {
    fn key(&self) -> InsertKey {
        self.key
    }

    fn set_param(&mut self, param: InsertParam, frames: u32) {
        self.push(Event::Param(param, frames));
    }

    fn restore_params(&mut self, frames: u32) {
        self.push(Event::Restore(frames));
    }

    fn sync(&mut self, beats: f64, tempo: f32, frames: u32) {
        self.push(Event::Sync(beats, tempo, frames));
    }

    fn note(&mut self, note: InsertNote, frames: u32) {
        self.push(Event::Note(note, frames));
    }

    fn cut_notes(&mut self, frames: u32) {
        self.push(Event::Cut(frames));
    }

    fn reset(&mut self) {
        // A stopped callback can reset without processing a block. Keep
        // one reset and discard events from the score that stopped.
        self.event_count = 0;
        self.push(Event::Reset);
    }

    fn busy(&self) -> bool {
        // An event that waits needs a block to reach the plugin.
        !self.health.ended() && (self.busy || self.event_count > 0)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len()).min(MAX_BLOCK);
        if frames > 0 && !self.health.ended() {
            let answered = self.exchange(&mut left[..frames], &mut right[..frames]);
            if !answered {
                self.health.0.store(true, Ordering::Relaxed);
                self.busy = false;
            }
        }
        self.event_count = 0;
    }
}

impl Drop for RemoteInstance {
    fn drop(&mut self) {
        self.running.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checking_a_worker_does_not_wait_for_its_build() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let stream = TcpStream::connect(listener.local_addr().expect("address")).expect("connect");
        let (_peer, _) = listener.accept().expect("accept");
        let mut child = Command::new(std::env::current_exe().expect("test program"))
            .arg("--list")
            .stdout(Stdio::null())
            .spawn()
            .expect("child");
        child.wait().expect("child ends");
        let worker = Arc::new(Worker {
            serial: 0,
            id: child.id(),
            child: Mutex::new(child),
            control: Mutex::new((BufReader::new(stream.try_clone().expect("reader")), stream)),
            listener,
            token: [0; TOKEN_BYTES],
            health: Arc::default(),
            copies: AtomicU64::new(0),
        });
        let build = worker.child.lock().expect("build owns process");
        let checking = Arc::clone(&worker);
        let (send, read) = mpsc::channel();
        let thread = std::thread::spawn(move || send.send(checking.ended()).expect("probe"));
        let result = read.recv_timeout(Duration::from_secs(1));
        drop(build);
        thread.join().expect("probe ends");
        assert_eq!(result, Ok(false), "an active build must not block a probe");
        assert!(worker.ended(), "the next probe finds the exit");

        let _build = worker.child.lock().expect("build owns process");
        worker.health.0.store(true, Ordering::Relaxed);
        assert!(
            worker.ended(),
            "a failed audio exchange is known during a build"
        );
    }

    #[test]
    fn an_event_keeps_its_numbers_on_the_wire() {
        let note = InsertNote {
            pitch: 60.5,
            velocity: 0.75,
            frames: 4_800,
        };
        let param = InsertParam { id: 7, value: 0.25 };
        let events = [
            Event::Param(param, 12),
            Event::Sync(16.5, 120.0, 3),
            Event::Note(note, 64),
            Event::Cut(9),
            Event::Reset,
            Event::Restore(21),
        ];
        /// Writes down each call the events make.
        #[derive(Default)]
        struct Calls(Vec<String>);
        impl OrbitInsert for Calls {
            fn key(&self) -> InsertKey {
                InsertKey::default()
            }
            fn set_param(&mut self, param: InsertParam, frames: u32) {
                self.0.push(format!("param {param:?} {frames}"));
            }
            fn restore_params(&mut self, frames: u32) {
                self.0.push(format!("restore {frames}"));
            }
            fn process(&mut self, _left: &mut [f32], _right: &mut [f32]) {}
            fn sync(&mut self, beats: f64, tempo: f32, frames: u32) {
                self.0.push(format!("sync {beats} {tempo} {frames}"));
            }
            fn note(&mut self, note: InsertNote, frames: u32) {
                self.0.push(format!("note {note:?} {frames}"));
            }
            fn cut_notes(&mut self, frames: u32) {
                self.0.push(format!("cut {frames}"));
            }
            fn reset(&mut self) {
                self.0.push("reset".into());
            }
        }
        let (mut sent, mut read) = (Calls::default(), Calls::default());
        for event in events {
            event.apply(&mut sent);
            let decoded = Event::decode(&event.encode()).expect("an event");
            decoded.apply(&mut read);
        }
        assert_eq!(sent.0.len(), 6);
        assert_eq!(sent.0, read.0);
        assert!(Event::decode(&[0u8; EVENT_BYTES]).is_none());
    }
}
