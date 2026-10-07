# rustel-studio-e2e

End-to-end tests for the native studio's terminal interface. A test opens a
real studio through rustel-studio's `harness` feature, presses real keys, and
asserts on what a musician would see: screens, focus, status text, errors.
It never hashes audio. The `hermetic()` fixtures open each studio with an
in-memory clipboard and with temporary configuration and session folders,
so a test does not use the platform clipboard or the settings and session
tapes in the runner's home directory.

The suite can use the network and the sample cache. Each studio loads the
pinned default sample manifests in the background, as the product does. It
reads each manifest from the sample cache. If the cached copy is missing or
does not match its pinned hash, it fetches the manifest from the network
and stores it in the cache. The starter score plays `bd`, and the fixtures
register a local `bd` bank first, so the studio does not download that
sample. If `RUSTEL_SAMPLE_CACHE` is not set, each fixture keeps its cache
in its own temporary folder, so each test starts with an empty cache. The
CI job sets `RUSTEL_SAMPLE_CACHE` to a folder that it restores from the
hosted cache. To reuse the manifests between local runs, set
`RUSTEL_SAMPLE_CACHE` to a folder that persists.

Run:

```sh
cargo test -p rustel-studio-e2e -- --test-threads=1
```

CI runs this suite on Linux, macOS and Windows in the `studio` job of
[`.github/workflows/heavy-checks.yml`](../../.github/workflows/heavy-checks.yml),
called by [`heavy.yml`](../../.github/workflows/heavy.yml). It runs on pushes
to `main`, on a schedule, and on maintainer dispatch for an exact PR commit.
See [How CI runs, and why](../../CONTRIBUTING.md#how-ci-runs-and-why).
The job uses the `--features pty` command below. It runs against the
studio's and the runtime's default feature sets. The check job's workspace
test excludes this crate, though Clippy still type-checks it on all three
platforms.

The `pty` feature adds `pty_smoke.rs` and `pty_alt_chords.rs`, which drive
a default-features `rustel` binary under a pseudo-terminal. Build the
binary first (`cargo build -p rustel`), then:

```sh
cargo test -p rustel-studio-e2e --features pty -- --test-threads=1
```

The PTY smoke tests print `pty_timing` records for startup, paste, and
resize when test output is visible:

```sh
cargo test -p rustel-studio-e2e --features pty --test pty_smoke -- --nocapture --test-threads=1
```

Each record gives the operation, elapsed milliseconds, and terminal size.
The paste record also gives the number of input bytes. These are wall-clock
observations until matching output arrives. They include process startup
where applicable, terminal transport, and the test's polling interval.
They do not measure a complete rendered frame. Timing values are
informational; only the existing functional assertions and hang limits
can fail a test.

## Coverage

`coverage.md` maps every studio surface to the test that covers it, with a
*not yet covered* list a PR is expected to shrink, never grow. If your PR
adds a studio surface, add a row; if it leaves one uncovered, say so there.

## Goldens

Golden screens live in `goldens/<test-name>.txt` as flattened rows, pinned
to LF (`.gitattributes`). A failing comparison prints a unified diff and
writes `<name>.actual` beside the golden, so you can compare the two files.

To regenerate goldens after a deliberate visual change, review the diff of
what changed first, then:

```sh
UPDATE_GOLDENS=1 cargo test -p rustel-studio-e2e
```

Goldens must not differ between operating systems. Where a surface
legitimately differs (the known case is platform shortcut spelling), the
test asserts semantically instead of against a golden.

## Demo video

The suite can record itself. When `RUSTEL_E2E_RECORD_DIR` is set, every
frame every test paints - plus a snapshot after each key, paste, click,
resize and settle - is appended to `recordings/`, one ANSI frame per
line, named for the test that painted it. `src/bin/e2e-demo.rs` turns
the recordings into `demo/e2e-demo.mp4`: the tests play back to back
with no interstitials, and the banner across the top of every frame
names the test that is running and how far through the suite it is.
Git ignores `demo/`, so the repository does not contain a rendered
video. The renderer does not create this folder. Create
`crates/studio-e2e/demo/` before the first render.

```sh
# 1. record - default parallel test threads, which is where the test
#    names come from (libtest names each test's thread after the test;
#    `--test-threads=1` leaves threads unnamed)
RUSTEL_E2E_RECORD_DIR=recordings cargo test -p rustel-studio-e2e

# 2. render - the local ffmpeg encodes the mp4; without it the
#    renderer falls back to an animated GIF, pure Rust, no second tool
cargo run -p rustel-studio-e2e --features demo --bin e2e-demo
```

The renderer is part of this crate and needs no system font. It draws the
banner and the cards in an 8x8 bitmap font, and it renders the terminal
frames from the recorded ANSI. `--recordings <dir>` sets the input folder and
`--out <path>` sets the output file.

After you add a test, run those two commands again. A test that does not
open a studio (a pure table assertion) shows a blank screen with its banner
for a short time, so the recording still includes every test. `pty_smoke`
drives the real binary under a PTY and is not part of the in-process
recording.

CI does not build the video - it is a local-only deliverable, opt-in via
`RUSTEL_E2E_RECORD_DIR`. Re-run the two commands above whenever you want
a fresh demo.
When the `studio` job fails, it uploads the golden `.actual` files. It
uploads no other artifact.

## Layout

- `src/lib.rs` - fixtures: `hermetic()` studios, `assert_golden`, screen
  helpers, the process-global state lock.
- `tests/*.rs` - one file per surface, named for the surface.
- `goldens/*.txt` - flattened screens, LF-pinned.
- `src/bin/e2e-demo.rs` - recordings → demo video (feature `demo`).
- `recordings/` - frame recordings, written only when `RUSTEL_E2E_RECORD_DIR`
  is set, gitignored.
- `demo/` - the rendered demo video, gitignored. The folder is not in the
  repository.
