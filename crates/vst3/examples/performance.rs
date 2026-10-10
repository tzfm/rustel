//! Measures installed plugins without an audio device or a full plugin scan.
//!
//! Build with `cargo build --profile local -p rustel-vst3 --example performance`.
//! Run `performance "kHs Distortion:drive"` under an external timeout. More
//! plugin arguments make a chain. `--direct` omits worker transport.
//! `--rate=96000` changes the sample rate and `--blocks=4000` the sample count.
//! `--frames=64,128,256,512` selects simulated callback sizes. Each callback
//! processes the full chain in chunks of at most 128 frames, as the host does.
//! `--copies=4` repeats each specified plugin four times in the serial chain.
//! `--automation-hz=30` limits edits to 30 per audio second at callback starts.
//! With no such option, automation sends an edit at every callback start.
//! `--value=0` sends a fixed parameter value instead of the automation sweep.
//! `--poll` also checks readiness and requests preparation during processing.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustel_audio::{InsertKey, InsertNote, InsertParam, OrbitInsert};
use rustel_vst3::{Host, Prepared, Resolved, WorkerProgram};
use serde_json::json;

// The host accepts at most 128 frames in both direct and worker processing.
const MAX_PLUGIN_FRAMES: usize = 128;

struct Options {
    direct: bool,
    rate: u32,
    blocks: usize,
    frames: Vec<usize>,
    copies: usize,
    automation_hz: Option<f64>,
    value: Option<f32>,
    poll: bool,
    requested: Vec<String>,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            direct: false,
            rate: 48_000,
            blocks: 2_000,
            frames: vec![16, 64, 128],
            copies: 1,
            automation_hz: None,
            value: None,
            poll: false,
            requested: Vec::new(),
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--direct" => options.direct = true,
                "--poll" => options.poll = true,
                _ if arg.starts_with("--") => {
                    let (name, inline) = arg
                        .split_once('=')
                        .map_or((arg.as_str(), None), |(name, value)| (name, Some(value)));
                    if !matches!(
                        name,
                        "--rate"
                            | "--blocks"
                            | "--frames"
                            | "--copies"
                            | "--automation-hz"
                            | "--value"
                    ) {
                        return Err(format!("unknown option {name}"));
                    }
                    let value = inline
                        .map(str::to_owned)
                        .or_else(|| args.next())
                        .ok_or_else(|| format!("missing value for {name}"))?;
                    match name {
                        "--rate" => {
                            options.rate = value.parse().map_err(|_| "invalid sample rate")?;
                        }
                        "--blocks" => {
                            options.blocks = value.parse().map_err(|_| "invalid block count")?;
                        }
                        "--frames" => {
                            options.frames = value
                                .split(',')
                                .map(|value| {
                                    value.trim().parse().map_err(|_| "invalid frame count")
                                })
                                .collect::<Result<_, _>>()?;
                        }
                        "--copies" => {
                            options.copies = value.parse().map_err(|_| "invalid copy count")?;
                        }
                        "--automation-hz" => {
                            options.automation_hz =
                                Some(value.parse().map_err(|_| "invalid automation rate")?);
                        }
                        "--value" => {
                            options.value =
                                Some(value.parse().map_err(|_| "invalid parameter value")?);
                        }
                        _ => unreachable!(),
                    }
                }
                _ => options.requested.push(arg),
            }
        }
        if options.requested.is_empty()
            || options.blocks == 0
            || options.rate == 0
            || options.copies == 0
        {
            return Err("usage: performance [--direct] [--rate=48000] [--blocks=2000] [--frames=16,64,128] [--copies=1] [--automation-hz=30] 'Plugin:param' ...".into());
        }
        if options
            .frames
            .iter()
            .any(|frames| !(1..=16_384).contains(frames))
        {
            return Err("frame counts must be from 1 to 16384".into());
        }
        if options
            .automation_hz
            .is_some_and(|hz| !hz.is_finite() || hz <= 0.0)
        {
            return Err("automation rate must be finite and greater than zero".into());
        }
        if options
            .value
            .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            return Err("parameter value must be from 0 to 1".into());
        }
        if options
            .blocks
            .checked_add(128)
            .and_then(|blocks| blocks.checked_mul(options.frames.iter().sum::<usize>()))
            .and_then(|frames| frames.checked_mul(4))
            .is_none()
            || options
                .copies
                .checked_mul(options.requested.len())
                .is_none()
        {
            return Err("requested workload is too large".into());
        }
        Ok(options)
    }
}

fn automation_due(block: usize, frames: usize, rate: u32, hz: Option<f64>) -> bool {
    let Some(hz) = hz else {
        return true;
    };
    if block == 0 {
        return true;
    }
    // Round edit times to the next callback start without accumulating drift.
    let edits_per_block = (hz * frames as f64 / f64::from(rate)).min(1.0);
    (block as f64 * edits_per_block).floor() > ((block - 1) as f64 * edits_per_block).floor()
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

#[derive(Default)]
struct PollStats {
    checks: usize,
    not_ready: usize,
    max_us: f64,
}

struct Poller {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<PollStats>>,
}

impl Poller {
    fn start(host: Host, plugins: &[Loaded], rate: u32) -> Self {
        let targets: Vec<_> = plugins
            .iter()
            .map(|plugin| (plugin.name.clone(), plugin.instrument))
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            let mut stats = PollStats::default();
            while !stopping.load(Ordering::Relaxed) {
                for (slot, (name, instrument)) in targets.iter().enumerate() {
                    let start = Instant::now();
                    stats.not_ready += usize::from(!matches!(
                        host.prepared(name, None, *instrument, rate, slot),
                        Prepared::Ready(_)
                    ));
                    host.prepare(name, None, *instrument, rate, slot);
                    stats.max_us = stats.max_us.max(micros(start.elapsed()));
                    stats.checks += 1;
                }
                thread::sleep(Duration::from_millis(1));
            }
            stats
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> Result<PollStats, String> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| "readiness polling thread panicked".into())
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}

fn run() -> Result<(), String> {
    let Options {
        direct,
        rate,
        blocks,
        frames,
        copies,
        automation_hz,
        value,
        poll,
        requested,
    } = Options::parse(std::env::args().skip(1))?;

    let host = Host::new();
    let _shutdown = Shutdown(host.clone());
    if !direct {
        host.set_worker(Some(WorkerProgram {
            program: std::env::current_exe().map_err(|error| error.to_string())?,
            args: vec!["--worker".into()],
        }));
    }
    let folders = std::env::var_os("RUSTEL_VST3_PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_else(rustel_vst3::default_folders);
    let scan = Instant::now();
    host.scan(&folders);
    host.wait_idle();
    let scan_us = micros(scan.elapsed());
    let mut plugins = Vec::new();
    let mut startup = Vec::new();
    for (slot, request) in requested
        .iter()
        .flat_map(|request| std::iter::repeat_n(request, copies))
        .enumerate()
    {
        let (name, param) = request
            .split_once(':')
            .map_or((request.as_str(), None), |(name, param)| {
                (name, Some(param))
            });
        let start = Instant::now();
        let (id, plugin) = match host.resolve(name, true) {
            Resolved::Ready(id, plugin) => (id, plugin),
            Resolved::Failed(reason) => return Err(format!("{name}: {reason}")),
            _ => return Err(format!("{name}: missing or still loading")),
        };
        let load_us = micros(start.elapsed());
        let param = param
            .map(|name| {
                let id = plugin
                    .param(name)
                    .ok_or_else(|| format!("{} has no parameter {name}", plugin.name()))?;
                let value = plugin
                    .params()
                    .iter()
                    .find(|param| param.id == id)
                    .unwrap()
                    .default;
                Ok::<_, String>(InsertParam {
                    id,
                    value: value as f32,
                })
            })
            .transpose()?;
        let start = Instant::now();
        let key = InsertKey {
            plugin: id,
            preset: 0,
        };
        let insert = host
            .insert(key, rate, slot, true)
            .ok_or_else(|| format!("{name}: instance build failed: {:?}", host.take_errors()))?;
        startup.push(json!({
            "plugin": plugin.name(),
            "slot": slot,
            "copy": slot % copies + 1,
            "parameters": plugin.params().len(),
            "automated_parameter": param.map(|param| param.id),
            "load_us": load_us,
            "build_us": micros(start.elapsed()),
        }));
        plugins.push(Loaded {
            name: plugin.name().to_owned(),
            instrument: plugin.is_instrument(),
            param,
            insert,
        });
    }

    // Readiness checks and prepare requests must reuse each running slot.
    let start = Instant::now();
    let mut not_ready = 0;
    for _ in 0..100 {
        for (slot, plugin) in plugins.iter().enumerate() {
            not_ready += usize::from(!matches!(
                host.prepared(&plugin.name, None, plugin.instrument, rate, slot),
                Prepared::Ready(_)
            ));
            host.prepare(&plugin.name, None, plugin.instrument, rate, slot);
        }
    }
    host.wait_idle();
    let reuse_us = micros(start.elapsed());
    let running: usize = host.plugins().iter().map(|plugin| plugin.running).sum();
    println!(
        "{}",
        json!({
            "kind": "startup",
            "mode": if direct { "direct" } else { "worker" },
            "sample_rate": rate,
            "copies_per_plugin": copies,
            "max_plugin_frames": MAX_PLUGIN_FRAMES,
            "scan_us": scan_us,
            "plugins": startup,
            "prepare_checks": 100 * plugins.len(),
            "prepare_total_us": reuse_us,
            "not_ready": not_ready,
            "running_copies": running,
        })
    );
    if not_ready != 0 || running != plugins.len() {
        return Err("preparing a held insert did not reuse its running copy".into());
    }

    let chain: Vec<_> = plugins.iter().map(|plugin| plugin.name.clone()).collect();
    let poller = poll.then(|| Poller::start(host.clone(), &plugins, rate));
    let mut transport_frames = 0u64;
    for frames in frames {
        for automate in [false, true] {
            if automate && plugins.iter().all(|plugin| plugin.param.is_none()) {
                continue;
            }
            let mut timings = Vec::with_capacity(blocks);
            let mut left = vec![0.0; frames];
            let mut right = vec![0.0; frames];
            let budget_us = frames as f64 / f64::from(rate) * 1_000_000.0;
            let mut late = 0;
            let mut peak = 0.0f32;
            let mut square_sum = 0.0f64;
            let mut first_us = 0.0;
            let mut automation_step = 0usize;
            let mut automation_updates = 0;
            let mut parameter_value = None;
            for plugin in &mut plugins {
                plugin.insert.reset();
                if let Some(param) = plugin.param {
                    plugin.insert.set_param(param, 0);
                }
            }
            // Warm 128 blocks, recording the first separately for deferred DSP setup.
            for block in 0..blocks + 128 {
                let update = automate && automation_due(block, frames, rate, automation_hz);
                if update {
                    parameter_value =
                        Some(value.unwrap_or(0.1 + (automation_step % 128) as f32 / 256.0));
                    automation_step += 1;
                    automation_updates += usize::from(block >= 128);
                }
                for (frame, (left, right)) in left.iter_mut().zip(&mut right).enumerate() {
                    let at = (block * frames + frame) as f32;
                    *left = (at * std::f32::consts::TAU * 220.0 / rate as f32).sin() * 0.1;
                    *right = *left;
                }
                let start = Instant::now();
                for (chunk, (left, right)) in left
                    .chunks_mut(MAX_PLUGIN_FRAMES)
                    .zip(right.chunks_mut(MAX_PLUGIN_FRAMES))
                    .enumerate()
                {
                    let chunk_frame = transport_frames + (chunk * MAX_PLUGIN_FRAMES) as u64;
                    for plugin in &mut plugins {
                        let note_due = chunk == 0 && plugin.instrument && block % 128 == 0;
                        if note_due || (chunk == 0 && update && plugin.param.is_some()) {
                            plugin.insert.restore_params(0);
                            if let Some(param) = plugin.param {
                                plugin.insert.set_param(
                                    InsertParam {
                                        id: param.id,
                                        value: parameter_value.unwrap_or(param.value),
                                    },
                                    0,
                                );
                            }
                        }
                        plugin
                            .insert
                            .sync(chunk_frame as f64 / f64::from(rate) * 1.5, 90.0, 0);
                        if note_due {
                            plugin.insert.note(
                                InsertNote {
                                    pitch: 60.0 + (block / 128 % 5) as f32,
                                    velocity: 0.5,
                                    frames: (frames * 120) as u32,
                                },
                                0,
                            );
                        }
                        plugin.insert.process(left, right);
                    }
                }
                let elapsed = micros(start.elapsed());
                transport_frames += frames as u64;
                if block == 0 {
                    first_us = elapsed;
                }
                for value in left.iter().chain(&right) {
                    if !value.is_finite() {
                        return Err("plugin produced non-finite audio".into());
                    }
                    if block >= 128 {
                        peak = peak.max(value.abs());
                        square_sum += f64::from(*value).powi(2);
                    }
                }
                if block >= 128 {
                    late += usize::from(elapsed > budget_us);
                    timings.push(elapsed);
                }
            }
            timings.sort_by(f64::total_cmp);
            println!(
                "{}",
                json!({
                    "kind": "processing",
                    "mode": if direct { "direct" } else { "worker" },
                    "plugins": chain,
                    "sample_rate": rate,
                    "frames": frames,
                    "device_frames": frames,
                    "process_frames": frames.min(MAX_PLUGIN_FRAMES),
                    "max_plugin_frames": MAX_PLUGIN_FRAMES,
                    "plugin_calls_per_block": frames.div_ceil(MAX_PLUGIN_FRAMES) * plugins.len(),
                    "copies_per_plugin": copies,
                    "automation_enabled": automate,
                    "parameter_restoration_enabled": true,
                    "automation_hz": automation_hz,
                    "automation_updates_per_parameter": automation_updates,
                    "measured_automation_hz": automation_updates as f64 * f64::from(rate) / (blocks * frames) as f64,
                    "parameter_each_block": automate && automation_updates == blocks,
                    "fixed_value": value,
                    "concurrent_readiness_polling": poll,
                    "blocks": blocks,
                    "budget_us": budget_us,
                    "first_us": first_us,
                    "mean_us": timings.iter().sum::<f64>() / blocks as f64,
                    "p50_us": timings[blocks / 2],
                    "p99_us": timings[(blocks - 1) * 99 / 100],
                    "max_us": timings[blocks - 1],
                    "over_budget": late,
                    "peak": peak,
                    "silent": peak == 0.0,
                    "rms": (square_sum / (blocks * frames * 2) as f64).sqrt(),
                })
            );
        }
    }
    if let Some(poller) = poller {
        let stats = poller.finish()?;
        host.wait_idle();
        let running: usize = host.plugins().iter().map(|plugin| plugin.running).sum();
        println!(
            "{}",
            json!({
                "kind": "polling",
                "checks": stats.checks,
                "not_ready": stats.not_ready,
                "max_us": stats.max_us,
                "running_copies": running,
            })
        );
        if stats.checks == 0 || stats.not_ready != 0 || running != plugins.len() {
            return Err("concurrent preparation did not reuse each running copy".into());
        }
    }
    host.check_workers();
    drop(plugins);
    host.wait_idle();
    let errors = host.take_errors();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("plugin host errors: {errors:?}"))
    }
}

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "--worker") {
        let code = args
            .get(2)
            .map_or(2, |path| rustel_vst3::serve(Path::new(path)));
        std::process::exit(code);
    }
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
    fn defaults_preserve_the_original_workload() {
        let options = options(&["Plugin:mix"]).unwrap();
        assert_eq!(options.rate, 48_000);
        assert_eq!(options.blocks, 2_000);
        assert_eq!(options.frames, [16, 64, 128]);
        assert_eq!(options.copies, 1);
        assert_eq!(options.automation_hz, None);
        assert!(!options.direct);
        assert!(!options.poll);
    }

    #[test]
    fn accepts_spaced_and_inline_options_with_callback_sizes_above_the_host_limit() {
        let options = options(&[
            "--direct",
            "--poll",
            "--rate=96000",
            "--blocks",
            "4000",
            "--frames=64,128,256,512",
            "--copies",
            "16",
            "--automation-hz",
            "30",
            "--value=0.5",
            "First:mix",
            "Second:drive",
        ])
        .unwrap();
        assert_eq!(options.rate, 96_000);
        assert_eq!(options.blocks, 4_000);
        assert_eq!(options.frames, [64, 128, 256, 512]);
        assert_eq!(options.copies, 16);
        assert_eq!(options.automation_hz, Some(30.0));
        assert_eq!(options.value, Some(0.5));
        assert_eq!(options.requested, ["First:mix", "Second:drive"]);
        assert!(options.direct);
        assert!(options.poll);
    }

    #[test]
    fn rejects_invalid_workloads_before_loading_plugins() {
        for arg in [
            "--rate=0",
            "--blocks=0",
            "--frames=",
            "--frames=0,128",
            "--frames=16385",
            "--copies=0",
            "--copies=-1",
            "--automation-hz=0",
            "--automation-hz=-1",
            "--automation-hz=NaN",
            "--automation-hz=inf",
            "--value=NaN",
            "--unknown=1",
        ] {
            assert!(options(&[arg, "Plugin"]).is_err(), "{arg}");
        }
        assert!(options(&["Plugin", "--frames"]).is_err());
        assert!(options(&[]).is_err());
    }

    #[test]
    fn automation_rounds_to_callback_starts_without_accumulating_drift() {
        let updates: Vec<_> = (0..50)
            .filter(|block| automation_due(*block, 256, 48_000, Some(20.0)))
            .collect();
        assert_eq!(updates, [0, 10, 19, 29, 38, 47]);
        for rate in [48_000u32, 96_000] {
            for frames in [64, 128, 256, 512] {
                let blocks = (rate as usize).div_ceil(frames);
                let updates = (0..blocks)
                    .filter(|block| automation_due(*block, frames, rate, Some(30.0)))
                    .count();
                assert_eq!(updates, 30, "rate={rate}, frames={frames}");
            }
        }
    }

    #[test]
    fn automation_sends_at_most_one_update_per_callback() {
        for block in 0..1_000 {
            assert!(automation_due(block, 128, 48_000, None));
            assert!(automation_due(block, 128, 48_000, Some(1_000.0)));
        }
    }
}
