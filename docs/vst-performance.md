# Measure a plugin

The `performance` example times a plugin chain with no audio device. Build the
example, then give the names of plugins you have:

```sh
cargo build --profile local -p rustel-vst3 --example performance
target/local/examples/performance --frames=128,512 "OTT:depth"
```

| Option | Effect |
| --- | --- |
| `name` | Loads one plugin. More names make one serial chain. |
| `name:parameter` | Adds a run with changes of the parameter. |
| `--copies=4` | Repeats each plugin 4 times in the chain. |
| `--frames=128,512` | The sizes of the callbacks. The default is 16, 64 and 128. |
| `--blocks=4000` | The measured callbacks for each size. The default is 2,000. |
| `--rate=96000` | The sample rate. The default is 48 kHz. |
| `--automation-hz=30` | The parameter changes for each second of audio. |
| `--threads=3` | Runs the process calls of each callback on 3 threads in turn. The chain moves to the next thread through a channel. |
| `--parallel=3` | Runs 3 copies of the chain at the same time, each on a thread of its own. |

The host gives a plugin 128 frames at most in one call. A callback of 512
frames makes 4 calls.

## Read the results

The output is JSON Lines. The `startup` row has the load time and the build
time of each plugin. Each `processing` row is one run at one callback size:

- `mean_us`, `p99_us` and `max_us` are the times of one callback in
  microseconds.
- `budget_us` is the length of the callback in audio time.
- `over_budget` counts the callbacks longer than `budget_us`.
- `silent` is true when the chain gave no sound. Timings of a silent chain
  say little.

Run each case alone on a quiet machine. A case with an error or with missing
rows is a failed case. Do not count such a case as zero misses.

The `live_performance` example runs a chain in the callback of your output
device. The device gets silence:

```sh
cargo build --profile local -p rustel-vst3 --example live_performance
target/local/examples/live_performance --seconds=10 --frames=128 "OTT"
```

## Plugins on several threads

A host that spreads plugins over several cores runs a copy on one thread at a
time, and the thread changes from callback to callback. It also runs two
copies of one plugin at the same time. Two options check that a plugin works
this way:

```sh
target/local/examples/performance --frames=128 --threads=3 "OTT:depth"
target/local/examples/performance --frames=128 --parallel=2 "OTT:depth"
```

With either option, the example also runs the chain on one thread with the
same input. Each `processing` row is that chain, and a `threads` or
`parallel` row follows it:

- `mean_us`, `p99_us` and `max_us` are the times of one callback on the
  threads, from the first start to the last end.
- `equal` is true when each sample of each callback is the same as on one
  thread.
- `max_difference` is the largest difference of one sample.

A plugin with a random part, such as noise or a random start phase, gives a
different output in each copy, also on one thread. For such a plugin a
difference alone shows no fault. A plugin that does not work on several
threads crashes the example, ends it with an error, or gives a large
`max_difference`.

## Compare the plugin workers

A heavy set spreads its plugin chains over plugin workers: see
[Where the audio runs](plugins.md#where-the-audio-runs). To see what the
workers give on your machine, play the same set two times, with the plugin
workers off and with the default:

```sh
RUSTEL_PLUGIN_THREADS=0 rustel play song.strudel --duration 60 --json
rustel play song.strudel --duration 60 --json
```

Each run ends with a `stopped` record. Compare these fields of the 2 records:

- `callback_xruns` counts the underruns of the audio device. On Windows,
  rustel reads them from the clock of the device: gaps that add up to more
  than 2 buffers count one.
- `callback_deadline_misses` counts the callbacks that took longer than their
  buffer.
- `late_events` counts the notes that reached the audio thread after their
  frame.

The `stopped` record of the run with the default also shows what the plugin
workers did:

- `plugin_workers` counts the plugin workers that got their real-time
  priority. It is 0 on a system that gives a plugin worker no real-time
  priority.
- `plugin_worker_busy_nanos` is the time the plugin workers ran plugin
  chains. It stays 0 while the set runs its plugins on the audio thread.
- `plugin_late_jobs` counts the times the audio thread waited for a plugin
  worker that the system did not run. That worker then takes no chain for
  1 second or more. The count stays 0 on Windows on Arm, where the clock of
  a thread runs at no fixed rate.

Read `callback_xruns` first: each underrun is a gap in the sound. A deadline
miss gives a gap only when the device runs out of audio. The plugin workers
help a set when the run with the default gives fewer underruns. A late job
now and then is normal. Many late jobs in a heavy set of one plugin point to
a multicore option of that plugin: see [Limits](plugins.md#limits).

A set that needs less than half of each buffer runs its plugins on the audio
thread in both runs. In Studio, set the metric detail row under Settings ▸
Look to `full`. The header then shows the DSP load: the time of the audio
thread in each buffer.
