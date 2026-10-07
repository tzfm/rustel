# The corpus suite

The suite evaluates and renders 1479 scores, then compares each result with a
committed golden. Run it before you open a pull request:

```sh
cargo test --release -p rustel-runtime --test e2e
```

The run takes about fifteen seconds on a warm machine. `--release` is
required because the goldens come from an optimised build.

The debug run skips full audio rendering. It still checks five nested-callback
scores against their committed event counts and hashes, without downloading
samples:

```sh
cargo test -p rustel-runtime --test e2e nested_callback
```

The first run downloads the sample banks the scores ask for. That takes a
couple of minutes and is cached afterwards.

The default uses automatic engine-kernel selection. Run the same corpus with
the existing portable kernels using:

```sh
RUSTEL_E2E_ACCELERATION=portable cargo test --release -p rustel-runtime --test e2e
```

From PowerShell, keep the override inside a temporary child process:

```powershell
cmd /c "set RUSTEL_E2E_ACCELERATION=portable&& cargo test --release -p rustel-runtime --test e2e"
```

`RUSTEL_E2E_ACCELERATION` accepts only `auto` or `portable`; unset means `auto`.
Invalid or non-Unicode values fail before corpus measurement. The selection is
prepared once and retained by every case's Session. Portable selects only the
engine-owned convolution, supersaw, and wavetable kernels: RustFFT planning
and compiler-generated SIMD remain unchanged. Both modes use the same golden
files, exact-hash rules, and cross-platform metric tolerances. CI runs the
portable suite first, then the automatic suite as a separate sequential step.
Both run only in the heavy tier: on pushes to `main`, scheduled cache-warming
runs, and a manual dispatch for an exact pull request commit (see
CONTRIBUTING.md, "How CI runs, and why"). Ordinary pull request CI does not
run the corpus.
Run it locally or dispatch Heavy CI for the current commit.

## What is in here

| directory | scores | what they are |
|---|---|---|
| `scores/corpus/generated` | 680 | fuzzed combinations of the pattern surface |
| `scores/corpus/regressions` | 100 | one score per bug that has been fixed |
| `scores/corpus/songs` | 186 | complete pieces, credited in `ATTRIBUTION.md` |
| `scores/docs` | 447 | upstream documentation examples, with exact duplicates removed |
| `scores/probes` | 66 | narrow synthesis cases - one oscillator, one filter |

Each score is an ordinary `.strudel` file. Play one, and press Ctrl+C to stop:

```sh
cargo run --release -p rustel -- crates/runtime/tests/e2e/scores/corpus/songs/madeallup.strudel
```

Provenance lives in each file's own header comment, in the same `@title` /
`@by` form the upstream song collections use, and `ATTRIBUTION.md` is
generated from those headers. A score gathered from the community bakery also
carries `@source`, the short link to the pattern it came from.

The bakery scores are the featured patterns this engine can render on its
own. A pattern that fetches a third-party sample bank is not included. Its
golden would depend on an external repository, and an offline run would
record silence.

## Duplicate documentation examples

The following examples share byte-identical source, including duration and
attribution, with the retained case. All golden fields except the case name
also match: event count and hash, source spans, render duration and frames,
peak, RMS, audible range, and audio hash. The runner applies the same setup
and assertions to each case; the path only identifies the result.

Paths below are relative to `scores/docs/`, without `.strudel`. The retained
files keep the original attribution. Their golden rows are unchanged.

| Removed copy | Retained equivalent |
|---|---|
| `audio/pwsweep` | `audio/pwrate` |
| `external_io/nrpv` | `external_io/nrpnn` |
| `external_io/oscport` | `external_io/oschost` |
| `external_io/sysexid` | `external_io/sysexdata` |
| `learn-synths/fm-2` | `fm/fmi-2` |
| `learn-synths/fm` | `fm/fmi` |
| `technical-manual-docs/bandf` | `learn-effects/bpf` |
| `learn-stepwise/polymeter` | `learn-factories/polymeter` |
| `learn-stepwise/stepcat-2` | `learn-factories/stepcat-2` |
| `learn-stepwise/stepcat` | `learn-factories/stepcat` |
| `learn-time-modifiers/clip` | `learn-samples/clip` |
| `tonal/i` | `learn-xen/xen-4` |
| `temporal/chunkInto` | `temporal/chunkBackInto` |

## Reading a failure

Events are compared before audio. An event-count failure prints expected and
actual counts; an event-content failure prints both `haps_sha256` values. If
the events agree, audio is compared next and a failure prints both
`audio_sha256` values plus peak, RMS, and where the sound starts and stops.
Here, **expected** is the committed golden and **actual** is the current engine.

One failure does not indicate a sound change:

- **could not load their samples** - a bank could not be fetched. Offline, a
  missing sample makes a voice silent rather than raising an error, so silence
  compared against a golden looks exactly like a regression. It is not one.

The suite also reports its work against `golden/timing.json`. That number came
from one machine and does not fail on a contributor's different machine.
`RUSTEL_E2E_SPEED_GATE=1` enables the assertion only for a stable runner that
recorded the baseline. Portable runs never compare against this Auto timing
baseline, even when the speed gate is enabled; their elapsed time does not
establish a matched Auto/portable performance comparison. These corpus checks
do not qualify physical-device headroom or browser throughput.

## Changing the sound on purpose

When a set differs, the suite writes the complete result beside its golden as
`golden/<set>.jsonl.actual` and prints a `git diff --no-index` command that
works even though the candidate is ignored by Git. Review that diff. If the
change is intentional, replace the `.jsonl` with the `.actual` file and commit
the one-line-per-score diff; otherwise, the actual file is evidence for the
regression. A changed hash proves that behaviour moved, not that it improved,
so the pull request must explain the intended change and should add a focused
regression score or test when practical.

These goldens show whether this engine changed. They do not show whether it
matches the browser. Browser parity still needs the separate comparison with
strudel.cc in the browser (Firefox is the reference).
