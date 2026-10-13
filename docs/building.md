# Build Rustel

The toolchain is pinned in `rust-toolchain.toml`. On Linux and macOS you
also need a C toolchain with `sh` and `make` on `PATH`. The MP3 encoder is
LAME, and its vendored source builds through its own `configure` script.
Windows needs neither `sh` nor `make`; its build uses only the compiler. The
default build is the complete product, including Studio and Hydra:

```sh
cargo build --profile local -p rustel
```

## Build profiles

Use `local` for an optimized build during development. It inherits the
`release` settings, including optimization level 3 and stripped symbols,
but uses thin link-time optimization (LTO), 16 code-generation units, and
incremental compilation. These settings favor faster rebuilds. The binary
can be larger than a release build; size and runtime speed depend on the
platform and code.

Use `release` for the final optimized binary. It uses fat LTO and one
code-generation unit for optimization across the whole program, at the
cost of longer builds. Both optimized profiles keep panic unwinding for
score recovery.

| Build command | Binary directory |
| --- | --- |
| `cargo build -p rustel` | `target/debug/` - the unoptimized `dev` profile, with debug information |
| `cargo build --profile local -p rustel` | `target/local/` |
| `cargo build --release -p rustel` | `target/release/` |

The executable is `rustel`, or `rustel.exe` on Windows. To build and launch
Studio in one command, use `cargo run --profile local -p rustel -- studio`.
Replace `--profile local` with `--release` to use the release profile.

## Build features and dependencies

A headless build for `query`, `check`, and offline `render` can leave the
default live/visual features out explicitly:

```sh
cargo build --profile local -p rustel --no-default-features --features mp3-export
```

Optional features, combinable as needed:

| Feature | Adds |
| --- | --- |
| `mp3-export` | MP3 export through LAME; included by default, independent of MP3 sample decoding |
| `device-audio` | live playback through CPAL |
| `midi` | MIDI input and output |
| `gamepad` | native gamepad input, included by default; Linux builds require libudev headers |
| `serial` | serial output for microcontrollers, included by default |
| `studio` | the terminal editor, included by default (includes device audio and MIDI discovery) |
| `hydra` | [Hydra visuals](hydra.md), included by default and removable from a lean build |
| `vst` | [VST3 plugins](plugins.md) for `.vst()` and `.vsti()`, included by default |

OSC output is included by default. On Debian and Ubuntu, the default and `--all-features`
builds need the C toolchain named above for the vendored mp3 encoder, ALSA headers
for live audio, libudev headers for gamepads, and `pkg-config` to locate those
libraries:

```sh
sudo apt-get install -y build-essential pkg-config libasound2-dev libudev-dev
```

To keep Studio, Hydra, and the other default features while omitting gamepad input:

```sh
cargo build --profile local -p rustel --no-default-features --features mp3-export,extensions,opus,osc,serial,hydra,studio,remote-control,vst
```

Adding `--all-features` enables `gamepad` again and requires the libudev headers on Linux.

Visuals need no separate system graphics library. Procedural and camera
visuals need no network, while `initImage()`
fetches the public HTTPS image named by the score. Check your build with
`rustel --version` or `rustel doctor`.

See [Contributing](../CONTRIBUTING.md) for development checks and CI.
