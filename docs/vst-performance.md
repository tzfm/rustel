# Measuring VST3 processing

Build the probe. Then run the same workload through a worker and with no worker:

```sh
cargo build --profile local -p rustel-vst3 --example performance
target/local/examples/performance --frames=64,128,256,512 --automation-hz=30 "kHs Distortion:drive"
target/local/examples/performance --direct --frames=64,128,256,512 --automation-hz=30 "kHs Distortion:drive"
```

The probe reads the standard plugin folders, or `RUSTEL_VST3_PATH` when you set
the variable. The probe loads only the plugins you name and opens no audio
device. The output is JSON Lines. The defaults are 48 kHz, 2,000 measured
callbacks, frame sizes of 16, 64 and 128, and one instance for each plugin.

A `:parameter` suffix adds an automated scenario after the static scenario.
With no `--automation-hz`, the probe updates the parameter on each callback.
`--automation-hz=30` aims for 30 updates per second of simulated audio. Each
update lands on a callback start, one update per callback at most. The sweep
repeats 128 normalized values from 0.1 to about 0.596. `--value=0.5` sends 0.5
on each update of the automated scenario. The static scenario uses the default
settings.

## Stress examples

Run each command alone. Use the names of plugins you have, and parameter keys
those plugins know:

```sh
target/local/examples/performance --rate=96000 --frames=64,128,256,512 --copies=4 --automation-hz=30 "kHs Distortion:drive"
target/local/examples/performance --frames=64,128,256,512 --copies=8 --automation-hz=120 "kHs Distortion:drive"
target/local/examples/performance --frames=64,128,256,512 --copies=16 --automation-hz=30 --value=0.5 "kHs Distortion:drive"
target/local/examples/performance --frames=64,128,256,512 --copies=4 "kHs Distortion:drive"
target/local/examples/performance --poll --frames=64,128,256,512 --automation-hz=30 "Comeback Kid:wetout" "kHs Distortion:drive"
```

Add `--direct` to run with no worker transport. Use `--blocks=4000` for a
longer measurement. Let builds and other benchmarks end first. Then repeat each
case in a process of its own.

Two or more plugin arguments make one serial chain. `--copies=4 "A" "B"` makes
four A instances, then four B instances, each in a slot of its own. This
measures one dependent signal path. The selected parameters take the same value
at the same time.

A score has four effect slots for each orbit, and 16 orbits. A longer probe
chain is synthetic: no score chain has this length. Such a chain does not
measure how independent tracks or orbits scale, though the engine processes
orbit DSP in series today.

The frame list gives the sizes of the simulated device callbacks. The host
gives a plugin 128 frames at most in one call, so a 512-frame callback runs
four chunks. Each chunk goes through the full chain before the next chunk
starts. A larger callback still makes several plugin calls and worker
exchanges, so the probe does not measure a native plugin call of 256 or 512
frames. The JSON has `device_frames`, `process_frames`, `max_plugin_frames` and
`plugin_calls_per_block`.

A command has a fixed callback count and no time limit of its own. For an
unattended run, use an outside supervisor which stops the probe and its workers
within a set time. Each worker leads a process group of its own. Killing the
probe's group does not ensure that the workers stop.

## Reading results

The startup rows show the module load apart from the build of an instance. The
probe makes 100 preparation requests for each slot and checks readiness with no
extra copies. `--poll` adds concurrent readiness and preparation checks, with a
sleep of one millisecond between passes over all slots. The final row of a poll
run reports the check count, the readiness failures, the longest control call
and the running copies.

Each scenario puts the selected parameter back to its default and warms up for
128 callbacks before the measurement. `first_us` holds the time of the first
warmup callback. Instances keep their history and tails from one scenario to
the next. The frame size and the sample rate change how long the warmup and the
measurement last. A short scenario sometimes covers a part of the sweep. Check
`automation_updates_per_parameter` and `measured_automation_hz`.

Each modeled note or automation event restores omitted parameters before
applying its controls. Instrument notes reapply the selected parameter's current
value. Automation counts exclude these note reapplications. The result field
`parameter_restoration_enabled` identifies these runs. The recorded stress run
below predates this behavior.

The timing covers the transport at 90 BPM, the note and parameter queues, the
chain processing and the worker transport. The timing leaves out input
generation, validation and logging. `over_budget` counts the measured callbacks
with a processing time above `device_frames / sample_rate`. Read this count
next to the p99 time and the maximum time.

A good run has exit status 0, one startup row and all processing rows. Each
frame size has a static row and, with a selected parameter, an automated row.
The readiness failures are zero, and the running copies equal plugins times
copies. A poll case also has a final row with one check or more. A timeout, a
host error or partial output is a failed case. Do not count such a case as zero
budget misses.

The probe rejects non-finite samples and reports the peak, the RMS and exact
silence. Silence alone does not fail a run. An effect chain gets identical left
and right channels of a 220 Hz sine at amplitude 0.1. Effects have no source-note
events, so their restoration runs only on automation updates. An instrument
gets repeating notes. The callback size sets their length and spacing. A default
preset, an inactive band, mono input or a missing sidechain can leave costly
plugin code with no work to do. Output above zero does not prove correct sound. Serial
instrument copies do not stand for a mixed polyphonic workload.

## Recorded stress run

On an Apple Silicon Mac with local-profile builds, 151 cases ended with valid
protocol output and no readiness failures. The concurrent polling made 29,947
checks. Atoms gave exact silence in 24 rows, so the totals for usable audio
leave out its 48,000 callbacks. The other 832,000 callbacks had 44,382
over-budget callbacks, some from synthetic chains made to overload. Do not read
this total as a general failure rate.

The single-plugin matrix covered 64, 128, 256 and 512 frames with static
settings and 30 Hz automation. Its 528,000 usable callbacks had four
over-budget callbacks, all with Ozone 12 Vintage Tape in direct mode at 48 kHz
and 128 frames with static settings. The longest took 7.26 ms against a budget
of 2.67 ms.

The table gives, at 128 frames, the first tested serial count with an
over-budget callback. The number in brackets is the over-budget count out of
2,000 callbacks. A count above four is a synthetic chain, longer than the limit
of one score orbit. These numbers are observations and no promise of capacity.

| Plugin | Rate | Worker | Direct |
| --- | --- | --- | --- |
| kHs Gain | 48 kHz | 32 (2) | None through 64 |
| kHs Gain | 96 kHz | 16 (8) | None through 64 |
| ValhallaFutureVerb | 48 kHz | 32 (2,000) | 32 (1,980) |
| ValhallaFutureVerb | 96 kHz | 4 (17) | 8 (1) |
| Ozone 12 Vintage Tape | 48 kHz | 16 (208) | 32 (2,000) |
| Ozone 12 Vintage Tape | 96 kHz | 4 (237) | 8 (2,000) |

At 96 kHz and 128 frames, one Smooth Operator Pro instance had no over-budget
callback at 30 Hz automation, at 120 Hz, or with an update on each callback.
Four instances with 30 Hz automation had 503 over-budget callbacks in worker
mode and 20 in direct mode, out of 2,000. The static rows of the same case had
500 and zero. The readiness checks still passed.

Larger callback sizes helped some synthetic workloads. At 96 kHz, 32 worker
kHs Gain instances had 998, 53 and 10 over-budget callbacks out of 1,000 at 64,
256 and 512 frames. Each plugin call still had 128 frames at most.

The change to the worker buffer went through four trials in alternating order
and showed no consistent speedup. The median of the mean processing time
changed by +3.7%, -0.6% and -0.5% at 1, 16 and 32 kHs Gain instances. At 16
instances, the total over-budget count went from one to 209 with similar median
timings. These runs do not prove better deadline reliability.

Rustel makes one synchronous round trip to the worker for each effect block of
128 frames at most, and processes the effects of an orbit in series. At 48 kHz
with 32 serial kHs Gain instances at unity gain, the probe averaged 1,002.325
microseconds for each block in worker mode and 27.806 microseconds in direct
mode. Worker transport accounts for most of the time in this case. A count above
four is more than the effect slots of one orbit, and stands for a synthetic
stress load.

## Orbit reroute probe

A separate worker-mode probe kept physical insert bus 1 while alternating output
orbits 1 and 2 through seven phases. Each case processed 1,792 blocks at 48 kHz
and 128 frames, with overlapping notes. The simulated block budget was 2.67 ms.

| Plugins | Copies throughout | Output routing | Largest first block after a move | Blocks over budget |
| --- | ---: | --- | ---: | ---: |
| kHs Gain | 1 | Correct | 2.36 ms | 9 |
| kHs Distortion | 1 | Correct | 0.62 ms | 7 |
| Comeback Kid | 1 | Correct | 0.92 ms | 11 |
| Kickstart 2 + Comeback Kid | 2 | Silent | 0.83 ms | 14 |

All cases completed without readiness misses, missing inserts, host errors,
resets or instance destruction during routing. The first block after every move
stayed within budget. Gain, Distortion and Comeback Kid produced finite nonzero
audio only on the selected output. The Kickstart chain remained silent, so that
case establishes instance continuity and readiness only.

The probe exercises the scalar backend's physical-bus and output-route controls
directly. It does not cover Studio mapping, callback install rings or native
room and delay tails. Pacing uses ordinary thread sleeps. Measured processing
excludes sleep time and includes worker exchanges and native voice mixing.
Budget exceedances are simulated deadline comparisons, not device dropouts.

## REAPER offline baseline

REAPER 7.78 rendered 600 seconds of 48 kHz stereo audio through serial kHs Gain
2.4.5 instances at +0 dB. Each output matched the source within 24-bit
quantization. The saved projects held the expected counts of enabled plugins
with no bypass and identical states. Output at unity gain does not prove real
processing in each effect.

| Serial instances | Audio duration | Process wall time |
| ---: | ---: | ---: |
| 1 | 600 s | 15.07 s |
| 8 | 600 s | 16.97 s |
| 16 | 600 s | 19.40 s |
| 32 | 600 s | 16.89 s |

Each configuration ran one time. The wall time includes the startup, the plugin
loads, the WAV write and the exit. The requested render block was 128 frames,
but the actual plugin block size was not verified. These timings do not rise
consistently with the instance count. They do not establish scaling or a speedup
over the Rustel probe, which times the processing alone. No live-device
measurements were obtained.

## Comparing with a DAW

Orbit changes follow graph activation. At 128 frames, a route change can precede
an ordinary note onset by up to 127 frames, or 2.65 ms at 48 kHz. SBD connects its
graph 100 ms before its source onset, and the insert route follows that earlier
activation. Route changes are not sample-exact.

Canceling a prepared plugin or preset replacement can construct another copy of
the restored plugin. Ordinary parameter edits and supported orbit-only moves
keep the same copy. Reusing a canceled replacement safely needs an ownership
handoff between the producer and the audio callback.

The `performance` example measures the host offline, with no pacing. It leaves
out the score evaluation, the sampler, the mixer, Studio, the audio device
callbacks and the scheduling of real-time threads. The automation follows
simulated audio time. The polling follows wall-clock time, so a faster run gets
fewer checks. An over-budget callback is no observed device underrun. Direct
mode is this host with no worker transport, and no DAW benchmark.

A comparison with a DAW needs the same plugin versions, presets, routing,
instance counts, automation, sample rate and real device buffer size. Serial
chains and independent tracks give a DAW different ways to schedule: see
[Ableton's multicore performance guide](https://help.ableton.com/hc/en-us/articles/209067649-Multi-core-performance-in-Ableton-Live-FAQ).
Write down the plugin isolation settings, because a process for each plugin has
a resource cost: see
[Bitwig's hosting options](https://www.bitwig.com/userguide/latest/vst_plug-in_handling_and_options/).
Control the anticipative processing and the live monitoring too.
[Steinberg's ASIO-Guard](https://helpcenter.steinberg.de/hc/en-us/articles/206103564-Details-on-ASIO-Guard-in-Cubase-and-Nuendo)
uses larger buffers for the playback tracks ASIO-Guard accepts than for
monitored or armed tracks. A comparison of dropouts needs repeated runs on a live device.

In a full Rustel score, give different effect chains to different orbits. The
probe gives each place of a chain a slot of its own. The probe does not check
score routing or the performance of a full score.
