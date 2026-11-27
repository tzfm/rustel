# External editors and live integrations

Rustel exposes stdio and JSON interfaces so external editors, visualizers, and control bridges can integrate directly with the engine without linking a custom binary.

```text
Editor / visualizer / bridge
  ├── rustel lsp --stdio                 (LSP completion, hover, diagnostics)
  ├── rustel doc --all --json            (full reference catalogue snapshot)
  ├── rustel samples list --json         (sample packs, sounds, variants, cache state)
  ├── rustel samples audition --stdio    (standalone immediate audition over NDJSON)
  └── rustel play song.strudel --watch --ui-events
        ├── stdout: ui_layout, ui_events, ui_audio, ui_control_ack (NDJSON)
        ├── stderr: live, reload, live_error                       (NDJSON)
        └── stdin:  ui_control commands                            (NDJSON)
```

Rustel stays a single process with no background daemon or network broker. If a setup needs a pub/sub hub or network transport, a bridge process can wrap these stdio streams.

## Language server (LSP)

Start the built-in language server over stdio:

```sh
rustel lsp --stdio
```

The server reads the engine reference catalogue and runs the static linter without opening an audio device or evaluating score JavaScript. It supports these LSP methods:

- `initialize`, `initialized`, `shutdown`, and `exit`
- `textDocument/didOpen`, `textDocument/didChange` (full sync), and `textDocument/didClose`
- `textDocument/publishDiagnostics` (syntax errors, value warnings, and notes from the same linter behind `rustel check`)
- `textDocument/completion` (top-level globals and snippets, chained pattern methods after `.`, sound, scale, and chord names inside strings, parameter choices, and call snippets when the client advertises snippet support)
- `textDocument/hover` (signatures, aliases, descriptions, parameters, and examples matching `rustel doc`)
- `textDocument/signatureHelp` (active parameter hints and choices triggered on `(` and `,`)

Any LSP client that speaks stdio (GNU Emacs with Eglot or lsp-mode, Neovim, Helix, Zed, or a VS Code client) can register `rustel lsp --stdio` for `.strudel` files.

For VS Code, install the [rustel-vscode](https://github.com/tzfm/rustel-vscode) extension. It starts `rustel lsp --stdio` for you and adds live playback with note highlighting. Set `rustel.serverPath` if `rustel` is not on your `PATH`.

Trade-off: static linting checks syntax, mini-notation, and built-in names in milliseconds without starting QuickJS or downloading remote sample manifests. Sample names loaded dynamically by `samples("...")` are checked only when a score runs or through `rustel check`.

## Reference catalogue export

Dump the full documented vocabulary in one command:

```sh
rustel doc --all
rustel doc --all --json
```

Text mode prints one tab-separated `name` and `summary` line per entry. JSON mode emits a versioned snapshot:

```json
{
  "version": 1,
  "revision": "ebc25467c0f117a7e7b9b11378e21cac94d730fd",
  "entries": [
    {
      "name": "lpf",
      "signature": "lpf(frequency)",
      "synonyms": ["cutoff", "ctf", "lp"],
      "summary": "Applies a low-pass filter at the given cutoff frequency.",
      "description": "Applies the cutoff frequency of the low-pass filter.",
      "params": [],
      "examples": ["s(\"bd sd [~ bd] sd,hh*6\").lpf(800)"],
      "tags": ["audio", "filter"],
      "origin": "",
      "callable": true,
      "no_autocomplete": false,
      "deprecated": false
    }
  ]
}
```

Use `rustel doc lpf --json` when a tool needs a single entry. Use `rustel doc --all --json` to generate syntax highlighters, editor completion tables, or offline documentation from one authoritative source.

## Sample catalogue

List shipped packs, sound names, variant counts, categories, import hints, and cache state from the engine's pinned manifests and local cache:

```sh
rustel samples list --json
rustel samples list --pack Dirt-Samples --json
rustel samples list --category drums --cached --json
rustel samples list --source local:kits --allow-local-samples ~/samples --json
```

Text mode groups sounds by pack with variant counts and cache progress. JSON mode emits a versioned snapshot:

```json
{
  "version": 1,
  "complete": true,
  "warnings": [],
  "sample_cache": "/path/to/samples",
  "packs": [
    {
      "pack": "Dirt-Samples",
      "category": "dirt",
      "source": "https://strudel.b-cdn.net/Dirt-Samples/",
      "shortcut": null,
      "import": null,
      "built_in": true,
      "sounds": 10,
      "files": 95,
      "cached_files": 0,
      "cached_bytes": 0,
      "cache_state": "uncached",
      "manifest_state": "ready",
      "manifest_error": null
    }
  ],
  "sounds": [
    {
      "pack": "Dirt-Samples",
      "sound": "casio",
      "variants": 3,
      "files": 3,
      "pitched": false,
      "category": "dirt",
      "origin": "default",
      "source": "https://strudel.b-cdn.net/Dirt-Samples/",
      "shortcut": null,
      "import": null,
      "cached_files": 0,
      "cached_bytes": 0,
      "cache_state": "uncached"
    }
  ]
}
```

Built-in sounds need no import call, so `import` is `null`. External sources passed with `--source` set `shortcut` when a shorthand such as `github:user/repo` or `local:kits` applies, and set `import` to the exact `samples("...")` snippet to paste into a score. Local folder sounds also include `variant_names` with file stems for each variant index. `cache_state` is `"uncached"`, `"partial"`, or `"cached"`.

`complete` is `true` when every requested manifest loaded. When a remote manifest fails or times out, `complete` is `false`, `warnings` lists each affected manifest, and that pack sets `manifest_state` to `"failed"` or `"loading"` with `manifest_error`. Healthy and cached packs still appear in the output.

## Live telemetry and control

Run a watched score with the structured UI event protocol enabled:

```sh
rustel play song.strudel --watch --ui-events
```

Add `--output silent` when testing a client without audio hardware.

Stdout carries only protocol records, one JSON object per line. Stderr carries structured live status, reload, and error records.

### Outbound records on stdout

`ui_layout` (version 2) arrives once per audible generation and carries `generation`, `source_revision` (64-character lowercase SHA-256 of the score bytes), `visuals`, `sliders`, `mini_locations`, and `lanes`. Each entry in `lanes` reports an edit-stable numeric `id`, the lane `label`, and its 1-based `line`:

```json
{"ui_layout":{"version":2,"generation":1,"source_revision":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","visuals":[],"sliders":[],"mini_locations":[[4,9]],"lanes":[{"id":0,"label":"drums","line":1},{"id":1,"label":"bass","line":2}]}}
```

`ui_events` (version 2) arrives in bounded batches as onsets are accepted for playback. When `--ui-events` is active, Rustel enables opt-in lane tracking before evaluating the score. Each event in `events` includes optional `lane` (matching `ui_layout.lanes[].id`) and `sound` (extracted from `s` or `sound` controls) fields alongside `onset_id`, `target_time`, `duration_secs`, `whole_begin`, `whole_end`, `context`, and `value`:

```json
{"ui_events":{"version":2,"device_time":0.1,"cycle":0.05,"cps":0.5,"generation":1,"source_revision":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","events":[{"onset_id":1,"target_time":0.25,"duration_secs":0.5,"whole_begin":"0/1","whole_end":"1/4","lane":0,"sound":"bd","context":[[4,6]],"value":"s:bd"}]}}
```

Trade-off: lane tagging adds one shared context pointer per lane wrapper and stays disabled during normal `rustel play` and `rustel export` runs. Enabling `--ui-events` activates tagging only for that session, so offline exports and standard playback pay zero extra cost.

`ui_audio` (version 1) streams waveform and spectrum frames only when the active score declares an audio visual (`scope`, `tscope`, or `spectrum`).

`ui_control_ack` (version 1) acknowledges any discrete `ui_control` command that sets a non-empty `request` string:

```json
{"ui_control_ack":{"version":1,"request":"solo-1","accepted":true,"generation":1}}
```

### Inbound control commands on stdin

Write one JSON object per line to stdin while `rustel play --watch --ui-events` is running. Every command uses version `1` inside a `ui_control` envelope.

Update a score slider (coalesced to the latest value per slider `id`, gated by active `generation` and `source_revision`):

```json
{"ui_control":{"version":1,"kind":"slider","generation":1,"source_revision":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","id":"10:14","value":0.75}}
```

Solo one lane, or pass `null` to clear solo:

```json
{"ui_control":{"version":1,"kind":"lane_solo","lane":1,"request":"solo-1"}}
{"ui_control":{"version":1,"kind":"lane_solo","lane":null,"request":"solo-clear"}}
```

Mute or unmute a lane without editing the score file:

```json
{"ui_control":{"version":1,"kind":"lane_mute","lane":0,"muted":true,"request":"mute-0"}}
```

Transpose a lane in semitones (range `-48` to `48`, pass `0` to clear):

```json
{"ui_control":{"version":1,"kind":"lane_transpose","lane":1,"semitones":12,"request":"tr-1"}}
```

`lane_solo`, `lane_mute`, and `lane_transpose` also accept optional `generation` and `source_revision` fields when a client wants stale-layout rejection. Lane controls take effect at the scheduler boundary without mutating or rewriting the score file.

Change tempo using either `cps` (cycles per second, up to `100`) or `bpm` (converted at `bpm / 240` cycles per second):

```json
{"ui_control":{"version":1,"kind":"tempo","bpm":128,"request":"tempo-1"}}
```

Control transport (`"stop"` ends playback and `"restart"` rebases cycle zero cleanly after the device schedule lead):

```json
{"ui_control":{"version":1,"kind":"transport","action":"restart","request":"restart-1"}}
```

Audition one sound, variant, note, or chord immediately without waiting for a file reload or cycle boundary:

```json
{"ui_control":{"version":1,"kind":"audition","sound":"bd","n":2,"gain":0.8,"request":"aud-1"}}
{"ui_control":{"version":1,"kind":"audition","sound":"piano","notes":["c4","e4","g4"],"duration":1.5,"request":"aud-2"}}
{"ui_control":{"version":1,"kind":"audition","sound":"kick","source":"local:kits","request":"aud-3"}}
```

Stop any active or pending audition voices without disturbing score playback:

```json
{"ui_control":{"version":1,"kind":"audition_stop","request":"aud-stop"}}
```

`audition` accepts `sound` (with an optional `:index` suffix such as `"bd:2"`), optional `n` (variant index override), optional `note` or `notes` (up to 8 MIDI numbers or note names such as `"c4"`), optional `gain` (`0` to `4`, default `1`), optional `duration` (`0.01` to `30` seconds), and optional `source` (an allowed `samples(...)` specifier). Audition voices run in reserved choke groups so rapid browsing replaces the prior preview without affecting live lanes or transport.

When a requested sample is already decoded on the device, `ui_control_ack` returns `"accepted":true` right away. When the sample or manifest must load first, `ui_control_ack` returns `"accepted":true` with `"message":"loading"` and plays as soon as decoding finishes. If loading fails later, stderr emits a recoverable `live_error` record with `"kind":"audition"`.

### Backpressure and safety bounds

The stdin control reader runs on a dedicated thread with fixed limits: 16 KiB maximum per input line, 64 coalesced pending sliders, and a bounded 64-entry command channel. Commands that arrive when the channel is full are dropped rather than blocking the reader or growing memory.

All control mutations run on the producer thread and trigger a safe scheduler requery cutover. No control parsing, locking, or JSON serialization ever runs on the audio callback thread.

## Standalone sample audition

When no score is playing, keep `rustel lsp --stdio` audio-free and use `rustel samples audition` instead.

Run a persistent audition worker over stdio that speaks the same `ui_control` and `ui_control_ack` NDJSON protocol as `rustel play --ui-events`:

```sh
rustel samples audition --stdio
rustel samples audition --stdio --allow-local-samples ~/samples
rustel samples audition --stdio --output silent
```

Send `audition`, `audition_stop`, or `transport` (`"action":"stop"`) lines on stdin, and read `ui_control_ack` lines on stdout. Closing stdin stops the worker cleanly.

For one-shot previews from scripts or pickers, pass the sound name directly:

```sh
rustel samples audition bd:2
rustel samples audition piano --note c4 --note e4 --note g4 --duration 1.5
rustel samples audition kick --source local:kits --allow-local-samples ~/samples --json
```

## Rust library hosts

Applications embedding `rustel-engine` with the `session` feature can call the same lane controls directly in Rust:

- `Session::set_lane_tracking_enabled(bool)`
- `Session::active_ui_lanes()`
- `Session::set_lane_solo(Option<u32>)`
- `Session::set_lane_mute(u32, bool)`
- `Session::set_lane_transpose(u32, i16)`
- `Session::clear_lane_controls()`

See [Embed the engine](embedding.md) for Cargo features and callback setup, and [Hardware](hardware.md) for MIDI, OSC, serial, and Ableton Link integration.
