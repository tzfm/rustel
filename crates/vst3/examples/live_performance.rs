//! Measures a plugin chain in the default output device's real audio callback.
//!
//! Build with `cargo build --profile local -p rustel-vst3 --example live_performance`.
//! Run `live_performance --seconds=10 --frames=128 'Pro-L 2'` under a timeout.
//! `--automation-hz=30 'Plugin:param'` adds parameter edits.
//! The device receives silence. The measured chain receives a continuous sine.
//! Timing misses describe this callback, not confirmed hardware underruns.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the example prints its measurements"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rustel_audio::{InsertKey, InsertNote, InsertParam, InsertSlot, OrbitInsert};
use rustel_vst3::{Host, Resolved};
use serde_json::{Value, json};

const CHUNK: usize = 128;
const MAX_RECORDS: usize = 100_000;

struct Options {
    rate: u32,
    frames: u32,
    seconds: f64,
    blocks: usize,
    automation_hz: Option<f64>,
    requested: Vec<String>,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            rate: 48_000,
            frames: 128,
            seconds: 10.0,
            blocks: MAX_RECORDS,
            automation_hz: None,
            requested: Vec::new(),
        };
        for arg in args {
            if let Some((name, value)) = arg.split_once('=') {
                match name {
                    "--rate" => options.rate = value.parse().map_err(|_| "invalid rate")?,
                    "--frames" => options.frames = value.parse().map_err(|_| "invalid frames")?,
                    "--seconds" => {
                        options.seconds = value.parse().map_err(|_| "invalid seconds")?
                    }
                    "--blocks" => options.blocks = value.parse().map_err(|_| "invalid blocks")?,
                    "--automation-hz" => {
                        options.automation_hz =
                            Some(value.parse().map_err(|_| "invalid edit rate")?);
                    }
                    _ => return Err(format!("unknown option {name}")),
                }
            } else if arg.starts_with("--") {
                return Err(format!("unknown option {arg}, use --name=value"));
            } else {
                options.requested.push(arg);
            }
        }
        if !(8_000..=192_000).contains(&options.rate)
            || !(16..=4096).contains(&options.frames)
            || !(0.1..=120.0).contains(&options.seconds)
            || !(1..=MAX_RECORDS).contains(&options.blocks)
            || options.requested.len() > 4
            || options
                .automation_hz
                .is_some_and(|hz| !(0.1..=1_000.0).contains(&hz))
        {
            return Err("use rate 8000..192000, frames 16..4096, seconds 0.1..120, blocks 1..100000, edits 0.1..1000 Hz and at most four plugins".into());
        }
        Ok(options)
    }
}

struct Loaded {
    name: String,
    instrument: bool,
    param: Option<InsertParam>,
    insert: Box<dyn OrbitInsert>,
}

struct Shutdown(Host);

impl Drop for Shutdown {
    fn drop(&mut self) {
        self.0.shutdown(Duration::from_secs(3));
    }
}

fn load(host: &Host, options: &Options) -> Result<Vec<Loaded>, String> {
    let folders = std::env::var_os("RUSTEL_VST3_PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_else(rustel_vst3::default_folders);
    host.scan(&folders);
    host.wait_idle();
    options
        .requested
        .iter()
        .enumerate()
        .map(|(slot, request)| {
            let (name, param) = request
                .split_once(':')
                .map_or((request.as_str(), None), |(name, param)| {
                    (name, Some(param))
                });
            let (id, plugin) = match host.resolve(name, true) {
                Resolved::Ready(id, plugin) => (id, plugin),
                Resolved::Failed(reason) => return Err(format!("{name}: {reason}")),
                _ => return Err(format!("{name}: missing or still loading")),
            };
            let param = param
                .map(|name| {
                    let id = plugin
                        .param(name)
                        .ok_or_else(|| format!("{} has no parameter {name}", plugin.name()))?;
                    let value = plugin
                        .params()
                        .iter()
                        .find(|param| param.id == id)
                        .expect("the parameter list has the id")
                        .default as f32;
                    Ok::<_, String>(InsertParam { id, value })
                })
                .transpose()?;
            let insert = host
                .insert(
                    InsertKey {
                        plugin: id,
                        preset: 0,
                    },
                    options.rate,
                    if plugin.is_instrument() {
                        InsertSlot::instrument(slot as u64)
                    } else {
                        InsertSlot::effect(slot as u64, 0)
                    },
                    true,
                )
                .ok_or_else(|| {
                    format!("{name}: instance build failed: {:?}", host.take_errors())
                })?;
            Ok(Loaded {
                name: plugin.name().to_owned(),
                instrument: plugin.is_instrument(),
                param,
                insert,
            })
        })
        .collect()
}

struct Chain {
    plugins: Vec<Loaded>,
    rate: u32,
    frame: u64,
    automation_hz: Option<f64>,
    edit: u64,
    value: f32,
    note: u64,
}

impl Chain {
    fn process(&mut self, frames: usize) -> (f32, f64, u64) {
        let update = self.automation_hz.is_some_and(|hz| {
            (self.frame as f64 * hz / f64::from(self.rate)).floor() as u64 >= self.edit
        });
        let note_due = self.frame / u64::from(self.rate) >= self.note;
        if update {
            self.value = 0.1 + (self.edit % 128) as f32 / 256.0;
        }
        let mut peak = 0.0f32;
        let mut squares = 0.0;
        let mut non_finite = 0;
        let (mut left, mut right) = ([0.0; CHUNK], [0.0; CHUNK]);
        for offset in (0..frames).step_by(CHUNK) {
            let count = (frames - offset).min(CHUNK);
            let (left, right) = (&mut left[..count], &mut right[..count]);
            for (index, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
                let phase = (self.frame + offset as u64 + index as u64) as f64
                    * std::f64::consts::TAU
                    * 220.0
                    / f64::from(self.rate);
                *left = phase.sin() as f32 * 0.1;
                *right = *left;
            }
            for plugin in &mut self.plugins {
                if offset == 0
                    && ((note_due && plugin.instrument) || (update && plugin.param.is_some()))
                {
                    plugin.insert.restore_params(0);
                    if let Some(param) = plugin.param {
                        plugin.insert.set_param(
                            InsertParam {
                                id: param.id,
                                value: if self.automation_hz.is_some() {
                                    self.value
                                } else {
                                    param.value
                                },
                            },
                            0,
                        );
                    }
                }
                plugin.insert.sync(
                    (self.frame + offset as u64) as f64 / f64::from(self.rate) * 1.5,
                    90.0,
                    0,
                );
                if offset == 0 && note_due && plugin.instrument {
                    plugin.insert.note(
                        InsertNote {
                            pitch: 60.0,
                            velocity: 0.5,
                            frames: self.rate * 3 / 4,
                        },
                        0,
                    );
                }
                plugin.insert.process(left, right);
            }
            for sample in left.iter().chain(right.iter()) {
                if sample.is_finite() {
                    peak = peak.max(sample.abs());
                    squares += f64::from(*sample).powi(2);
                } else {
                    non_finite += 1;
                }
            }
        }
        if update {
            self.edit = (self.frame as f64
                * self.automation_hz.expect("the automation rate is set")
                / f64::from(self.rate))
            .floor() as u64
                + 1;
        }
        if note_due {
            self.note = self.frame / u64::from(self.rate) + 1;
        }
        self.frame += frames as u64;
        (peak, squares, non_finite)
    }
}

// Each callback writes one fixed record. Main reads it after the stream stops.
// Atomics let the stop path remain safe even if a backend retires callbacks late.
#[derive(Default)]
struct Record {
    entry: AtomicU64,
    callback: AtomicU64,
    playback: AtomicU64,
    elapsed: AtomicU64,
    frames: AtomicU64,
    peak: AtomicU64,
    squares: AtomicU64,
    non_finite: AtomicU64,
}

struct Stats {
    records: Box<[Record]>,
    count: AtomicUsize,
    errors: AtomicUsize,
    error_kinds: [AtomicUsize; ERROR_KINDS.len()],
    fatal_errors: AtomicUsize,
    stop: AtomicBool,
    policy: [AtomicU64; 9],
}

const ERROR_KINDS: [&str; 15] = [
    "device_busy",
    "device_changed",
    "device_not_available",
    "host_unavailable",
    "invalid_input",
    "permission_denied",
    "realtime_denied",
    "resource_exhausted",
    "stream_invalidated",
    "unsupported_config",
    "unsupported_operation",
    "xrun",
    "backend_error",
    "other",
    "unknown",
];

fn error_kind_index(kind: cpal::ErrorKind) -> usize {
    use cpal::ErrorKind;
    match kind {
        ErrorKind::DeviceBusy => 0,
        ErrorKind::DeviceChanged => 1,
        ErrorKind::DeviceNotAvailable => 2,
        ErrorKind::HostUnavailable => 3,
        ErrorKind::InvalidInput => 4,
        ErrorKind::PermissionDenied => 5,
        ErrorKind::RealtimeDenied => 6,
        ErrorKind::ResourceExhausted => 7,
        ErrorKind::StreamInvalidated => 8,
        ErrorKind::UnsupportedConfig => 9,
        ErrorKind::UnsupportedOperation => 10,
        ErrorKind::Xrun => 11,
        ErrorKind::BackendError => 12,
        ErrorKind::Other => 13,
        _ => 14,
    }
}

#[cfg(target_os = "macos")]
fn callback_policy() -> [u64; 9] {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    let mut policy = libc::thread_time_constraint_policy {
        period: 0,
        computation: 0,
        constraint: 0,
        preemptible: 0,
    };
    let mut count = libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT;
    let mut default = 0;
    let mut timebase = Timebase { numer: 0, denom: 0 };
    // SAFETY: These calls inspect this thread and fill stack-owned output buffers.
    let result = unsafe {
        mach_timebase_info(&mut timebase);
        libc::thread_policy_get(
            libc::pthread_mach_thread_np(libc::pthread_self()),
            libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
            (&raw mut policy).cast(),
            &mut count,
            &mut default,
        )
    };
    [
        result as i64 as u64,
        u64::from(count),
        u64::from(default != 0),
        u64::from(policy.period),
        u64::from(policy.computation),
        u64::from(policy.constraint),
        u64::from(policy.preemptible != 0),
        u64::from(timebase.numer),
        u64::from(timebase.denom),
    ]
}

#[cfg(not(target_os = "macos"))]
fn callback_policy() -> [u64; 9] {
    [0; 9]
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn distribution(mut values: Vec<f64>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let last = values.len() - 1;
    json!({
        "mean": values.iter().sum::<f64>() / values.len() as f64,
        "p50": values[values.len() / 2],
        "p99": values[last * 99 / 100],
        "max": values[last],
    })
}

fn report(
    stats: &Stats,
    options: &Options,
    plugins: &[String],
    channels: u16,
    wall: Duration,
) -> Value {
    let count = stats.count.load(Ordering::Acquire).min(stats.records.len());
    let records = &stats.records[..count];
    let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
    let spacing = |field: fn(&Record) -> &AtomicU64| {
        records
            .windows(2)
            .map(|pair| read(field(&pair[1])).saturating_sub(read(field(&pair[0]))) as f64 / 1000.0)
            .collect()
    };
    let frames: u64 = records.iter().map(|record| read(&record.frames)).sum();
    let non_finite: u64 = records.iter().map(|record| read(&record.non_finite)).sum();
    let peak = records
        .iter()
        .map(|record| f32::from_bits(read(&record.peak) as u32))
        .fold(0.0f32, f32::max);
    let squares: f64 = records
        .iter()
        .map(|record| f64::from_bits(read(&record.squares)))
        .sum();
    let mut sizes: Vec<_> = records.iter().map(|record| read(&record.frames)).collect();
    sizes.sort_unstable();
    sizes.dedup();
    let mut slowest: Vec<_> = records.iter().enumerate().collect();
    slowest.sort_unstable_by_key(|(_, record)| std::cmp::Reverse(read(&record.elapsed)));
    let origin = records.first().map_or(0, |record| read(&record.callback));
    let [
        status,
        policy_count,
        default,
        period,
        computation,
        constraint,
        preemptible,
        numer,
        denom,
    ] = std::array::from_fn(|index| read(&stats.policy[index]));
    json!({
        "kind": "live_processing",
        "plugins": plugins,
        "muted_device_output": true,
        "rate": options.rate,
        "channels": channels,
        "requested_frames": options.frames,
        "actual_frames": sizes,
        "max_plugin_frames": CHUNK,
        "callback_policy": if cfg!(target_os = "macos") { json!({
            "valid": count > 0 && status == 0 && policy_count == 4 && default == 0,
            "status": status as i64,
            "is_default": default != 0,
            "period_ticks": period,
            "computation_ticks": computation,
            "constraint_ticks": constraint,
            "preemptible": preemptible != 0,
            "timebase_numer": numer,
            "timebase_denom": denom,
            "captured_on_first_callback": true,
        }) } else { Value::Null },
        "automation_hz": options.automation_hz,
        "blocks": count,
        "wall_seconds": wall.as_secs_f64(),
        "audio_seconds": frames as f64 / f64::from(options.rate),
        "elapsed_us": distribution(records.iter().map(|record| read(&record.elapsed) as f64 / 1000.0).collect()),
        "entry_spacing_us": distribution(spacing(|record| &record.entry)),
        "cpal_callback_spacing_us": distribution(spacing(|record| &record.callback)),
        "cpal_playback_spacing_us": distribution(spacing(|record| &record.playback)),
        "playback_lead_us": distribution(records.iter().map(|record| read(&record.playback).saturating_sub(read(&record.callback)) as f64 / 1000.0).collect()),
        "over_period": records.iter().filter(|record| read(&record.elapsed) as f64 > read(&record.frames) as f64 / f64::from(options.rate) * 1e9).count(),
        "over_playback_lead": records.iter().filter(|record| read(&record.elapsed) > read(&record.playback).saturating_sub(read(&record.callback))).count(),
        "entry_gaps_over_1_5_periods": records.windows(2).filter(|pair| read(&pair[1].entry).saturating_sub(read(&pair[0].entry)) as f64 > read(&pair[0].frames) as f64 / f64::from(options.rate) * 1.5e9).count(),
        "stream_errors": stats.errors.load(Ordering::Relaxed),
        "stream_error_kinds": ERROR_KINDS.iter().zip(&stats.error_kinds).map(|(name, count)| ((*name).to_owned(), json!(count.load(Ordering::Relaxed)))).collect::<serde_json::Map<_, _>>(),
        "fatal_stream_errors": stats.fatal_errors.load(Ordering::Relaxed),
        "non_finite_samples": non_finite,
        "peak": peak,
        "rms": if frames == 0 { 0.0 } else { (squares / (frames * 2) as f64).sqrt() },
        "slowest_callbacks": slowest.into_iter().take(16).map(|(index, record)| json!({
            "block": index,
            "entry_us": read(&record.entry) as f64 / 1000.0,
            "callback_us": read(&record.callback).saturating_sub(origin) as f64 / 1000.0,
            "playback_us": read(&record.playback).saturating_sub(origin) as f64 / 1000.0,
            "elapsed_us": read(&record.elapsed) as f64 / 1000.0,
            "frames": read(&record.frames),
        })).collect::<Vec<_>>(),
    })
}

fn run() -> Result<(), String> {
    let options = Options::parse(std::env::args().skip(1))?;
    let audio = cpal::default_host();
    let device = audio
        .default_output_device()
        .ok_or("no default output device")?;
    let default = device
        .default_output_config()
        .map_err(|error| error.to_string())?;
    if default.sample_format() != cpal::SampleFormat::F32 {
        return Err("this probe requires a float output device".into());
    }
    let mut config = default.config();
    config.sample_rate = options.rate;
    config.buffer_size = cpal::BufferSize::Fixed(options.frames);
    let channels = config.channels;
    let host = Host::new();
    let _shutdown = Shutdown(host.clone());
    let plugins = load(&host, &options)?;
    let names: Vec<_> = plugins.iter().map(|plugin| plugin.name.clone()).collect();
    let mut chain = Chain {
        plugins,
        rate: options.rate,
        frame: 0,
        automation_hz: options.automation_hz,
        edit: 0,
        value: 0.1,
        note: 0,
    };
    // Warm DSP and transport before the device starts. Keep that state for playback.
    for _ in 0..256 {
        let (_, _, non_finite) = chain.process(CHUNK);
        if non_finite != 0 {
            return Err("plugin produced non-finite audio during warmup".into());
        }
    }
    let stats = Arc::new(Stats {
        records: (0..options.blocks).map(|_| Record::default()).collect(),
        count: AtomicUsize::new(0),
        errors: AtomicUsize::new(0),
        error_kinds: std::array::from_fn(|_| AtomicUsize::new(0)),
        fatal_errors: AtomicUsize::new(0),
        stop: AtomicBool::new(false),
        policy: std::array::from_fn(|_| AtomicU64::new(0)),
    });
    let callback_stats = stats.clone();
    let error_stats = stats.clone();
    let (prepared, incoming) = std::sync::mpsc::sync_channel(1);
    let mut live_chain: Option<Chain> = None;
    let origin = Instant::now();
    let stream = device
        .build_output_stream(
            config,
            move |output: &mut [f32], info| {
                let start = Instant::now();
                output.fill(0.0);
                if live_chain.is_none() {
                    live_chain = incoming.try_recv().ok();
                }
                let Some(chain) = live_chain.as_mut() else {
                    return;
                };
                let index = callback_stats.count.load(Ordering::Relaxed);
                if callback_stats.stop.load(Ordering::Relaxed)
                    || index >= callback_stats.records.len()
                {
                    return;
                }
                if index == 0 {
                    for (field, value) in callback_stats.policy.iter().zip(callback_policy()) {
                        field.store(value, Ordering::Relaxed);
                    }
                }
                let frames = output.len() / usize::from(channels);
                let (peak, squares, non_finite) = chain.process(frames);
                let record = &callback_stats.records[index];
                let time = info.timestamp();
                record
                    .entry
                    .store(nanos(start.duration_since(origin)), Ordering::Relaxed);
                record.callback.store(
                    time.callback.as_nanos().min(u128::from(u64::MAX)) as u64,
                    Ordering::Relaxed,
                );
                record.playback.store(
                    time.playback.as_nanos().min(u128::from(u64::MAX)) as u64,
                    Ordering::Relaxed,
                );
                record.frames.store(frames as u64, Ordering::Relaxed);
                record
                    .peak
                    .store(u64::from(peak.to_bits()), Ordering::Relaxed);
                record.squares.store(squares.to_bits(), Ordering::Relaxed);
                record.non_finite.store(non_finite, Ordering::Relaxed);
                record
                    .elapsed
                    .store(nanos(start.elapsed()), Ordering::Relaxed);
                callback_stats.count.store(index + 1, Ordering::Release);
            },
            move |error| {
                let kind = error.kind();
                error_stats.errors.fetch_add(1, Ordering::Relaxed);
                error_stats.error_kinds[error_kind_index(kind)].fetch_add(1, Ordering::Relaxed);
                // These notifications leave the stream running, including after an overload.
                if !matches!(
                    kind,
                    cpal::ErrorKind::Xrun
                        | cpal::ErrorKind::DeviceChanged
                        | cpal::ErrorKind::RealtimeDenied
                ) {
                    error_stats.fatal_errors.fetch_add(1, Ordering::Relaxed);
                    error_stats.stop.store(true, Ordering::Release);
                }
            },
            Some(Duration::from_secs(3)),
        )
        .map_err(|error| error.to_string())?;
    prepared
        .send(chain)
        .map_err(|_| "audio stream ended before preparation")?;
    let start = Instant::now();
    stream.play().map_err(|error| error.to_string())?;
    while start.elapsed().as_secs_f64() < options.seconds
        && stats.count.load(Ordering::Acquire) < options.blocks
        && !stats.stop.load(Ordering::Acquire)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    stats.stop.store(true, Ordering::Release);
    let paused = stream.pause();
    drop(stream);
    let result = report(&stats, &options, &names, channels, start.elapsed());
    println!("{result}");
    paused.map_err(|error| error.to_string())?;
    host.wait_idle();
    let errors = host.take_errors();
    if !errors.is_empty() {
        return Err(format!("plugin host errors: {errors:?}"));
    }
    if stats.fatal_errors.load(Ordering::Relaxed) != 0
        || result["non_finite_samples"] != 0
        || result["blocks"] == 0
    {
        return Err("the stream failed, produced non-finite audio or received no callbacks".into());
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
