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
