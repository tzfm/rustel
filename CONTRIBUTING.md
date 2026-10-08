# Contributing

Rustel is a native Rust audio engine for the [Strudel](https://strudel.cc)
pattern language. Preserve event timing and sound when changing the
implementation; both are part of the engine's compatibility contract.

See [Coding style](CODING_STYLE.md) for guidance on naming, comments, module
boundaries, tests, documentation, and attribution.

## Before opening a pull request

Run:

```sh
cargo fmt --all -- --check
bash .github/scripts/check-panic-unwind.sh
bash .github/scripts/check-engine.sh
bash .github/scripts/tests/release-notes.sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features --exclude rustel-studio-e2e -- --test-threads=1
cargo test --release -p rustel-runtime --test e2e
cargo build -p rustel
cargo test -p rustel-studio-e2e --features pty -- --test-threads=1
```

The last two commands build the product binary and run the studio's end-to-end
suite. Most tests open a real studio headlessly, type into it, and assert on
the screens it paints. The `pty` feature also checks that the product binary
starts, resizes and quits in a real terminal. The suite must run serially, so
it has a command of its own and the workspace test leaves it out.
A PR that changes the studio must keep this suite passing. CI runs it only in
the heavy tier (see "How CI runs, and why"). Run it locally, or ask a
maintainer to dispatch Heavy CI for the PR's current commit.

The `--release` command above it evaluates, queries, and renders all committed
corpus scores. It takes about fifteen seconds with a warm sample cache. The
first run downloads the sample banks used by the scores and takes longer.

`--release` is required. The ordinary debug workspace test compiles the target
but skips its measurements because debug and optimised builds do not produce
the same results for a few scores.

The default product includes [Hydra visuals](docs/hydra.md). Procedural and
camera visuals need no separate system graphics library or network;
`initImage()` fetches the public HTTPS image named by the score. Shaders are
composed in Rust and rendered through wgpu, which can fall back to a
software rasteriser on a machine with no GPU.

On a machine with neither a GPU nor a software rasteriser, the renderer
suites skip their tests and pass.

The explicit `-p rustel --no-default-features --features mp3-export` binary
build has no graphics stack. CI checks that lean build, the complete default
build, and the all-features build. Cargo builds dev-dependencies for every
test target regardless of features. A single ungated `rustel-hydra` under
`[dev-dependencies]` would therefore add its whole dependency tree to a lean
test. Keep it an optional normal dependency, which integration tests can also
use. Gate the tests that use it on the feature, as `tests/semantic/hydra.rs`
does.

## Where code lives

### The studio

`crates/studio/src/app.rs` holds the studio's `App`, what is needed to build
it, and `run`. The App's behaviour is split by feature under
`crates/studio/src/app/`. There is one file for each visible surface
(`mixer.rs`, `reference_panel.rs`, `settings_sheet.rs`, ...) and a few files
for the code that every feature uses (`keyboard.rs`, `mouse.rs`, `render.rs`,
`event_loop.rs`, ...). The module doc at the top of `app.rs` lists them.

Each of those files is an `impl App` block of its own. A method another file
calls is `pub(super)`, which keeps it inside the `app` module; everything else
is private to its file.

To trace a behavior, start at `app/event_loop.rs`, which pumps background
work, dispatches terminal events and decides when to draw. Keys and pointer
events enter through `app/keyboard.rs` and `app/mouse.rs`, then reach the
feature file named in `app.rs`. An update goes through `app/evaluation.rs`
to `worker.rs`, which owns `engine.rs` on its own thread; the reply comes back
through `app/evaluation.rs`. `app/render.rs` builds the frame, while `view.rs`
and `view/layout.rs` decide what appears where.

```text
UI thread                                         engine worker thread

app/event_loop.rs
  |-- app/keyboard.rs, app/mouse.rs
  |     `-- feature file (see app.rs)
  |           `-- app/evaluation.rs -- update --> worker.rs
  |                                 <-- reply ---   `-- engine.rs
  `-- app/render.rs
        `-- view.rs, view/layout.rs
```

The end-to-end harness in `app/harness.rs` drives these same paths; its tests
are grouped by surface in `crates/studio-e2e/tests/`.

For a quick check while working on Studio internals, run the focused library
suite before the end-to-end suite:

```sh
cargo test -p rustel-studio --lib -- --test-threads=1
```

### Tests

Unit tests live at the bottom of the file they test, in a `#[cfg(test)]`
module - the layout the Rust book teaches:

- A unit test uses `use super::*;` and may exercise private behavior
  directly. Keep it small and focused on the unit's logic. Group a large
  suite into named child modules inside the same inline block.
- A crate's own `tests/` folder is for integration tests. Those compile as a
  separate crate and see only the public API.
- Do not extract unit tests into separate files (`#[path]` modules or
  `src/tests/` trees), and do not expose internals just to move a test into
  the integration suite.

### Reviewing a move

To review a change that moves code between files, dim the moved lines. The
other lines are the real changes:

```sh
git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change
```

`git blame -C -C` follows a line's history across the move.

## Comments

A doc comment states the contract in the present tense: what the item
guarantees and under which conditions, usually in one to three sentences.
Describe Rustel directly. Leave implementation history, pull request
narration, chains of reproducers and capitals for emphasis out of every
comment. Keep a Strudel parity note to a short clause, and only where the
behaviour would otherwise look like a bug. Write cross-references as short
pointers, such as "Mirrors [`x`]".

## Reading a corpus failure

- `event count changed` prints the committed expectation and current count.
  `the N events changed` prints the expected and actual `haps_sha256`; the
  count is equal but at least one event's canonical value moved.
- `audio changed` prints the expected and actual `audio_sha256`, followed by
  the peak, RMS, and audible-frame measurements. Samples are hashed on a
  quantised grid, and peak, RMS and audible frames are compared with a small
  tolerance; the event stream, its hash, the span count, the duration and the
  frame count are compared exactly. Set `RUSTEL_E2E_EXACT=1` to gate on the
  audio hash as well - see "What the goldens prove".
- `could not load their samples` is a fetch/cache failure, not a sound
  difference. Run once with network access or point `RUSTEL_SAMPLE_CACHE` at a
  warm cache.
- `failed to run` identifies an evaluation, query, render, or non-finite-audio
  failure.
- `no committed expected result` means a new score needs review and adoption;
  `committed golden row(s) have no score` means a score was removed without
  removing or replacing its expectation.

In every comparison, **expected** means the committed `.jsonl` and **actual**
means what the current engine produced. The failure prints a copy-pasteable
`git diff --no-index` command for each changed set. That command exits with
status 1 when it successfully finds a difference; the status is not another
test failure.

The timing line is informational on ordinary machines. Only a stable machine
that recorded `golden/timing.json` should set `RUSTEL_E2E_SPEED_GATE=1`.

## Reviewing an intentional sound change

For every set that changes, the suite writes the new complete measurement next
to the committed golden:

```text
crates/runtime/tests/e2e/golden/corpus.jsonl.actual
crates/runtime/tests/e2e/golden/docs.jsonl.actual
crates/runtime/tests/e2e/golden/probes.jsonl.actual
```

Review the diff first. If it is the change you intended, replace the matching
golden and commit that diff. If it is surprising, keep the actual file locally
while debugging; it is ignored by Git.

For example, from the repository root:

```sh
git diff --no-index -- crates/runtime/tests/e2e/golden/corpus.jsonl crates/runtime/tests/e2e/golden/corpus.jsonl.actual
```

After reviewing an intentional change, adopt it on PowerShell with:

```powershell
Copy-Item -Force crates/runtime/tests/e2e/golden/corpus.jsonl.actual crates/runtime/tests/e2e/golden/corpus.jsonl
git diff -- crates/runtime/tests/e2e/golden/corpus.jsonl
```

Or in a POSIX shell:

```sh
cp crates/runtime/tests/e2e/golden/corpus.jsonl.actual crates/runtime/tests/e2e/golden/corpus.jsonl
git diff -- crates/runtime/tests/e2e/golden/corpus.jsonl
```

Each golden is JSON Lines, sorted by score, so the review shows one line for
each score that moved. A new hash proves that behaviour changed, not that the
new behaviour is correct. Explain why each score should move in the pull
request and add a focused regression score or test when that is practical.

## Adding a regression score

Add a focused `.strudel` file under
`crates/runtime/tests/e2e/scores/corpus/regressions/` with a provenance header:

```js
/*
  @by your name or handle
  @origin regressions
  @duration 4
*/
```

Run the release suite. It reports the missing golden and writes the complete
candidate `corpus.jsonl.actual`; review and adopt it as above.

## What the goldens prove

The native suite compares the current engine with a committed capture of this
engine. It catches changes quickly and offline once samples are cached. It does
not prove browser parity by itself; maintainers use the separate comparison
with strudel.cc in the browser for that (Firefox is the reference).

Parity includes upstream behavior that may look odd. Crashes, hangs, and unsafe
resource use are exceptions: those are refused safely instead of copied.

Matching the capture's OS and architecture does not guarantee bit-identical
audio. glibc selects its libm implementations per CPU, so two x86_64 Linux
hosts can disagree in the last bits of `sin`, `exp` and `powf`. Quantisation
absorbs most of these differences, but some can still change the audio hash.

Set `RUSTEL_E2E_EXACT=1` on the machine capturing goldens to gate on
`audio_sha256`. Elsewhere, a changed hash triggers a comparison of peak, RMS
and audible frames against their tolerances. Differences below tolerance are
counted and printed. Every platform compares the event count, `haps_sha256`,
source-location span count, duration and frame count exactly.

## How CI runs, and why

The workflows use GitHub-hosted Linux, macOS and Windows runners. `.github/workflows/ci.yml` runs ordinary checks on every pull request and push to `main`. It checks formatting, Clippy, supported feature sets and the serial workspace tests, and runs the tests of the hand-written `unsafe` code under Miri. `.github/workflows/heavy.yml` runs the release-mode corpus in portable and automatic acceleration modes and the Studio end-to-end suite. Heavy CI runs on pushes to `main`, twice weekly for cache warming, and when a maintainer dispatches it for an exact pull request commit.

A pull request with only Markdown changes skips the build and test jobs. Four Markdown files are test inputs, so a change to `SECURITY.md`, `docs/cli.md`, `docs/hardware.md` or `docs/studio.md` runs the full checks. The `main` ruleset requires one check, `CI result`. This job passes when the build and test jobs pass or are skipped.

The CI workflows declare read-only repository tokens. Checkouts do not persist credentials, and the jobs do not receive repository secrets. No CI workflow uses a self-hosted runner.

Heavy CI calls `.github/workflows/heavy-checks.yml` from the same trusted revision for both targets. PR jobs have enforced `cache-mode: read` and restore caches without saving. Main jobs have `cache-mode: write` and can refresh caches. [Cache permissions](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching#controlling-cache-access-with-cache-mode) are separate from the repository token: checking out a PR during a dispatch from `main` does not give that job a PR-scoped cache. The read-only mode keeps PR code from writing caches later consumed by main jobs.

### Running heavy checks for a pull request

Review the proposed code and workflow changes, then record the current full head SHA:

```sh
gh pr view <number> --json headRefOid --jq .headRefOid
gh workflow run heavy.yml --ref main -f pr_number=<number> -f sha=<full-40-character-sha>
```

The Actions page offers the same operation: choose **Heavy CI → Run workflow**, select `main`, then enter the PR number and full SHA. The workflow definition comes from `main`. Its first job checks that the PR is open, targets this repository's `main`, and still has the requested SHA. Each test job checks the SHA again after checkout, before running PR code. The run summary records the SHA. A later push changes the PR head; dispatch a new heavy run for the new SHA. A prior run does not qualify the new commit.

Maintainers should dispatch heavy CI for changes to the engine, scheduler, audio rendering, corpus goldens, Studio behavior or any other release-critical path. The ordinary checks run regardless of this choice. The heavy jobs are optional PR checks, so do not make them unconditional required status checks.

### Unsafe code and Miri

[Miri](https://github.com/rust-lang/miri) interprets Rust code and stops on undefined behavior: a read outside a buffer, a freed pointer, uninitialized memory, a data race or a leak. `.github/scripts/miri.sh` runs the tests of the hand-written `unsafe` code under Miri: the audio rings, the unchecked sample reads, the AVX2 kernels, the callback host pointers, the QuickJS allocator and the bridge frame stack.

```sh
bash .github/scripts/miri.sh
```

The script installs the nightly toolchain named at its top. Miri does not run C code or inline assembly, so a test with QuickJS, an audio device or an OS call stays out of the list. When you add `unsafe` code, write a test with no such call and add the module to the script. Miri is 50 to 300 times slower than a native run, so keep the test small. `cfg!(miri)` cuts a loop count for the Miri run only.

### Build and test behavior

Native build profiles must use `panic = "unwind"` so score panic boundaries
can report failures and rebuild the affected session. The release panic check
asks Cargo for the effective strategy, including configuration, environment,
and Rust flags. Do not override it with `panic = "abort"`. Recovery does not
cover aborts, process crashes, or panics in the real-time audio callback.

In the CI workflows, all `run` steps use Bash, except two steps that run only on Windows and set `shell: powershell`. The first one is the first shell step of the job. It puts Git Bash on `PATH` before a Bash step executes; otherwise the runner can select WSL's `bash.exe`, which cannot read the Windows job script path. The second one checks that the MSVC linker works before the first build. Linux jobs install ALSA, libudev and, where needed, Mesa's software Vulkan driver. The ordinary Linux job frees disk before linking its larger debug test binaries.

The ordinary check builds the default and lean product feature sets, the Studio-without-Hydra combination, standalone engine consumers, and selected WebAssembly and Apple silicon profiles. The corpus runs an optimized build twice so a difference between portable and automatically selected audio kernels is visible. Golden audio hashes may differ in their last bits across hosts; see the corpus suite guide above for the exact/tolerant comparison rules.

Sample downloads use per-platform hosted caches. The loader rechecks cached files against pinned hashes. Twice-weekly heavy runs keep corpus and Studio entries warm; a cold cache can otherwise cause simultaneous CDN fetches. `RUSTEL_SAMPLE_CACHE` is set per step because the `runner` context is unavailable in job-level `env`. `TMPDIR` is isolated per run attempt. Failing corpus and Studio goldens are uploaded as short-lived artifacts.

Concurrency is per job and platform. A newer run cancels the older run for the same ref, including `main`. Heavy PR dispatches use the PR number as their concurrency key, so a newer dispatch supersedes an older one for that PR. A heavy PR run is valid only for the SHA recorded in its summary.

## Public evidence privacy

Do not commit details that identify a contributor's workstation: ownership,
usernames, home-directory paths, hostnames, serial or device identifiers, or
exact personal-workstation specifications. Public benchmark evidence should
use an anonymous hardware class and sanitized paths; retain detailed machine
fingerprints only in private working data.

## Licence

Contributions are AGPL-3.0-or-later, like the rest of the project. See
[LICENSE](LICENSE) and [NOTICE.md](NOTICE.md).
