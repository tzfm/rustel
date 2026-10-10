# Embed the engine

Use `rustel-engine` from a Rust host, including the Rust backend of a native
macOS app. The default features are empty:

```toml
[dependencies]
rustel-engine = { git = "https://github.com/tzfm/rustel", default-features = false }
```

The crates are not published on crates.io. Pin a Git `rev` in an application
for reproducible builds, or use `path = "../rustel/crates/engine"` with a local
checkout. The repository's `rust-toolchain.toml` specifies the tested compiler,
which the engine also declares as its `rust-version`.

The DSP is far slower without optimization, and an unoptimized audio crate
can fall behind a real-time callback. Keep it optimized in the host's
development builds too:

```toml
[profile.dev.package.rustel-audio]
opt-level = 3
```

Try the smallest complete path from this checkout:

```sh
cargo run -p rustel-engine --example native_pattern
```

The [example](../crates/engine/examples/native_pattern.rs) parses a pattern,
schedules its notes, resolves voices, and renders stereo PCM. It opens no
device and uses no JavaScript, network, terminal, or graphics stack.

## Choose the layers your host needs

| Feature | Available APIs and dependencies |
| --- | --- |
| None | `core`, `fraction`, `mini`, `scheduler`, `voice`, and `audio` modules; patterns, timing, samples, synths, effects, and PCM rendering |
| `javascript` | `jsruntime` and `transpiler`; QuickJS score evaluation with a C compiler build dependency |
| `session` | `Session`, `SessionConfig`, their errors and diagnostics, and play/render reports; score evaluation, sample loading/cache, scheduling, and file/PCM rendering; includes `javascript`, HTTP/TLS, and sample decoders |
| `extensions` | Additional Rustel score functions; implies `javascript` and also enables extensions in an active `session` |
| `device-audio` | CPAL audio device adapter; works with either the base engine or `session` |
| `opus`, `mp3-export` | Opus sample decoding or MP3 export respectively; each implies `session`; MP3 sample decoding does not require `mp3-export` |

No engine feature enables the CLI or terminal Studio. The `session` feature
retains the runtime's sample and file services; use the base or `javascript`
profile if the host must own those services and omit their dependencies.
All modules re-export existing crate APIs and types, so applications can also
depend directly on individual workspace crates. Other types that `Session`
methods take or return, such as query and schedule reports, stay in
`rustel-runtime`; depend on it with `default-features = false` to name them.

The core EDO helpers (`tonal::edo` and `xen::edo`) accept at most 65,536
divisions. `core::util::sound_index` returns zero for an empty bank; callers
must still check that a sample exists before indexing it.

A browser host builds on the base profile.

Cargo features are additive. Another dependency on `rustel-runtime` with
defaults enabled will add every runtime product feature to the final
dependency graph.
Inspect your application's selected graph with `cargo tree -e normal`.

## Bound JavaScript score evaluation

Direct `JsRuntime` hosts use `evaluate_score_cancellable` for a finite
execution budget and cancellation flag. Its deadline and heap boundary also
cover slider and voicing candidate setup, including JavaScript getters read
while copying the voicing registry. A refused candidate leaves the last good
graph active, and a later score can recover.

The synchronous QuickJS execution budget excludes Rust transpilation.
Ordinary JavaScript side effects already performed before a refusal remain
in the runtime. The legacy `evaluate_score` method keeps its unbounded
execution contract.

## Parse on the stack of the host

The `transpiler` module spawns a thread with 256 MiB of stack for every parse
of a score. A host with no threads gets a refusal diagnostic from every
`transpile` call. A host which already runs on a large stack of its own pays
for a thread the host does not need. Wrap the calls in
`transpiler::on_caller_stack`:

```rust
use rustel_engine::transpiler::{TranspileOptions, on_caller_stack, transpile};

let output = on_caller_stack(|| transpile(source, &TranspileOptions::default()));
```

Inside the closure no parse spawns a thread. This holds for `awaits_in_code`
and for the `JsRuntime` calls which evaluate a score, such as
`evaluate_score_cancellable`, because they transpile first. Calls nest, and
the setting belongs to the calling thread.

The host then owns the stack size. The nesting checks still run first, so
source which nests too deep or spends too many bytes on structure gets a
diagnostic. A source which defeats the lexical scan overflows the stack of the
calling thread. The thread which the module spawns has 256 MiB for this case.
Calls outside `on_caller_stack` keep the spawned thread.

## Reuse scores with a native audio callback

Enable `session` while keeping device ownership in the application:

```toml
[dependencies]
rustel-engine = { git = "https://github.com/tzfm/rustel", default-features = false, features = ["session"] }
```

```sh
cargo run -p rustel-engine --example session_callback --features session
```

The [callback example](../crates/engine/examples/session_callback.rs) creates
a Session on a worker, evaluates a built-in sine score, fills an audio-event
ring, and renders a finite window using a simulated host callback. It needs
no sample downloads or CPAL. A continuous player extends this arrangement:

```text
UI thread          Session worker                 audio callback
---------          --------------                 --------------
commands  ------>  evaluate the score
                   schedule_audio_through()
                   push AudioEvent --> Ring -->   pop in process_block_with()
results   <------  diagnostics                    interleaved stereo f32
```

1. Construct and retain the Session on one worker with
   `QUERY_WORKER_STACK_BYTES` of stack. QuickJS is thread-affine. Send UI
   commands to this worker and return results and diagnostics to the UI.
2. Evaluate scores and call `schedule_audio_through(now, now + horizon, sample_rate)`
   ahead of playback on that worker. Use one consistent time origin and the
   device's sample rate for scheduling and rendering. Sample loading, score
   queries, and voice preparation stay on the worker.
3. Pass the resulting owned `audio::AudioEvent` values through `audio::Ring`.
   Choose its capacity up front and handle overflow on the producer. The
   first thread to push owns the producer role and the first thread to pop
   owns the consumer role. The callback can move to another thread after a
   device reopen, a route change, or an audio-server restart. In that case,
   call the unsafe `release_consumer()` after the old callback has provably
   stopped, for example because its stream was dropped, and before the new
   callback pops. Without this call, the ring refuses the new callback's
   pops: a release build outputs silence and a debug build panics. A
   replacement Session worker needs the unsafe `release_producer()` in the
   same way, after the old worker has been joined. Releasing a role while
   its old thread can still use the ring is a data race.
4. Prepare `audio::LiveScalarBackend` before playback. In the audio callback,
   use `process_block_with` to fill interleaved stereo `f32` output with the
   callback's frame position. The host supplies its device, buffer adaptation,
   and transport/generation atomics. Keep allocation, blocking, I/O, JavaScript,
   and UI work out of the callback.

The example pre-fills one fixed window and has no live reload or transport UI.
A complete player must keep scheduling ahead and coordinate generation flips
and stop/restart with the audio consumer. After you set the stop flag, keep
calling `process_block_with` until `stop_ramp_complete()` returns true, then
close the stream. The stop fades the sound over 10 ms.

Hosts can lower the per-query event limit with
`Session::set_query_hap_budget(limit)`. The limit must be between 1 and
`core::DEFAULT_HAP_BUDGET`, inclusive, and bounds both final and intermediate
hap vectors. It applies to install probes, scheduling, direct queries, and
previews. With a lower limit, a fresh pattern without JavaScript callbacks
is probed over its first cycle before installation; patterns with callbacks
leave their first query to the scheduler. Refused replacements keep the
last good score. Studio uses 65,536; other Sessions retain the core default
unless the host changes it.

If a native settings update unwinds, `core::settings::RuntimeSettings`
retains its last published snapshot. Subsequent reads, updates, and snapshot
replacements recover the publication lock instead of propagating its poison.
The conservative `retained_snapshot_bytes` estimate still returns `None` for
a poisoned publication lock.

A host-owned callback plays synth voices and the bundled `bd` sample only.
`LiveScalarBackend` has no public way to install sample PCM or reverb
impulse responses, and it never generates a reverb inside the callback.
Events for any other sample are dropped, and `.room()` plays dry, with no
diagnostic in either case. When a score needs samples or reverb, use
Rustel's own device output (`device-audio` with `Session::play_on_device`,
which enables the default sample library itself) or offline stereo output:
call `Session::enable_default_samples()`, then `Session::render_pcm`.
Without that call, offline output also plays only the bundled `bd`, unless
the score loads samples itself. The default library fetches sample files
over the network when a score first uses them. See the API docs for the
individual contracts:

```sh
cargo doc -p rustel-engine --features session --open
```

The engine writes nothing to stderr by default. A Session collects score
diagnostics, including `.log()` lines and the voice resolver's notices (kind
`voice-notice`); drain them with `take_diagnostics()` and show them in the
application's UI. `set_direct_diagnostic_logging(true)` prints them as JSON
lines instead. Without a Session, `voice::with_diagnostic_policy(false, ..)`
returns the notices raised while it resolves voices, each a sentence and its
JSON record, and `voice::set_default_direct_diagnostic_logging(true)` prints
them for the whole process. With `device-audio` on Linux, libasound, which
the ALSA device host loads, prints its own error lines to stderr; the engine
leaves them there.

A host with a pointer, such as a window's mouse, creates a
`core::host_value::Pointer` and passes a clone to
`SessionConfig::with_pointer`. As the pointer moves, the host calls
`pointer.x.set(..)` and `pointer.y.set(..)` from any thread. Each value is a
fraction of the host surface, with 0 at the left or top edge. The score's
`mousex` and `mousey` read the latest position when they are queried. Without
a pointer they read 0, so an offline render is repeatable.

For AppKit or SwiftUI, put this Rust API behind an application-owned bridge.
Rustel does not currently provide a stable C ABI, Swift package, or native
window toolkit. A host using Rustel's CPAL output must also install the
`audio::tripwire::TripwireAlloc` at the binary root as documented by that
module; the host-owned callback example does not open that adapter.

## Leave out the scale table

`rustel-core` embeds 3,304 named scales as 244 KB of gzipped JSON and decodes
the table with `flate2` on first use. The `tuning-list` feature holds the table
and the decoder. The feature is on by default. A host with a size limit turns
the feature off:

```toml
[dependencies]
rustel-core = { git = "https://github.com/tzfm/rustel", default-features = false }
```

The `rustel-mini`, `rustel-scheduler`, `rustel-voice` and `rustel-ext` crates
depend on `rustel-core` with no default features, so they keep the feature
off. The `rustel-engine` crate keeps the default features of `rustel-core` and
so keeps the table. Cargo features are additive: any other dependency on
`rustel-core` with defaults enabled turns the feature back on. Run
`cargo tree -e features -i rustel-core` to see the result.

Without the feature, `Tune::scale_names()` is empty and `Tune::scale_count()`
is 0. A score sees every named scale as unknown: `tune("hexany15")` and
`xen("hexany15")` raise the error for an unknown name. A scale given as a
frequency list works as before, as do the `xen` EDO names such as `31edo`.

## Build compatibility

`rustel-runtime` with `default-features = false` omits every product
feature, including the LAME MP3 encoder. The `rustel` binary, package
`rustel` in `crates/cli`, builds the complete product by default. Without
default features it is the minimal command; add `mp3-export` for MP3 export:

```sh
cargo build -p rustel --no-default-features --features mp3-export
```

Without `mp3-export`, WAV/PCM rendering and MP3 sample decoding remain
available; requesting MP3 export returns `RuntimeError::Unsupported`.

CI compiles and runs both examples as a separate downstream workspace and
checks the base profile's normal and build dependencies against a committed
snapshot. Run the same check locally with
`bash .github/scripts/check-engine.sh`.

The engine remains licensed under AGPL-3.0-or-later; see [LICENSE](../LICENSE)
and [NOTICE.md](../NOTICE.md).
