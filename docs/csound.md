# Csound instruments

Csound turns an orchestra, a set of instrument definitions, into audio. Use
it to build a sound, then sequence that sound with patterns. Live playback,
Studio, and WAV or MP3 export use the same native library.

## Install and enable

Install double-precision Csound 6.18 or a later 6.x release from the
[Csound downloads page](https://csound.com/download.html). On macOS, run the
package installer, or copy `CsoundLib64.framework` from the downloaded
package into `~/Library/Frameworks` for this user alone, or into
`/Library/Frameworks` for every user; Rustel finds both without further
setup. The Homebrew 6.18 package cannot play named instruments, which shared
orchestras use. On Ubuntu 24.04, `sudo apt install libcsound64-6.0` provides
the library.

The one-shot commands enable native orchestra execution with a flag:

```sh
rustel play song.strudel --allow-csound
```

Studio permits native orchestras while a Csound library is installed: open
the studio and the score plays. Press Ctrl+O and go to the Permissions page
to turn the `native orchestras` switch off for a run; a score refused while
it is off runs again by itself when the switch comes back on.

An orchestra can read files, run processes, and load native opcodes with your
account's permissions. Enable Csound only for scores and orchestras you trust.
See [score execution security](../SECURITY.md#csound-orchestras).

Rustel searches the usual shared-library locations. For another installation,
set `RUSTEL_CSOUND_LIBRARY` to its shared library before starting Rustel.
Plugin opcodes such as fluidsynth or the mixer buss need the framework in
`/Library/Frameworks`; a framework under the home directory plays the core
opcodes, which shared orchestras use. Csound 7 and single-precision builds
are not supported.

## Start with the included instruments

Save this score as `song.strudel`:

```js
note("c3 eb3 g3 bb3").csound("triangle organ").gain(0.45).slow(2)
```

`csound()` selects `triangle` by default. The included instruments are
`triangle`, `organ`, `pad`, and `bow`. The `Meta` instrument selects an
instrument from the event's `s` control. These definitions and the `keymap`
helper come from the upstream orchestra identified in [NOTICE](../NOTICE.md).
They are bundled, so this score needs no orchestra download.

## Make an instrument

`loadCSound` compiles orchestra text. It accepts a string or a tagged template.
`loadCsound` and `loadcsound` are aliases. Define instruments before the pattern
that uses them:

```js
loadCSound`
instr glass
  iBrightness = strtod(keymap("brightness", strget(p6)))
  aEnvelope linseg 0, .005, 1, max(p3 - .01, .001), 0
  aBody poscil p5, p4
  aPartial poscil p5 * iBrightness, p4 * 2.01
  aOut = (aBody + aPartial) * aEnvelope * .5
  outs aOut, aOut
endin
`
register('brightness', (amount, pattern) =>
  pattern.fmap(value => ({ ...value, brightness: amount })))
note("c4 eb4 g4 bb4").csound("glass").brightness("0.1 0.3 0.6 0.9").gain(0.5)
```

The second oscillator sits just above the second harmonic. Raising
`brightness` adds more of that partial. `register` makes the custom control
available as a pattern method. `keymap` reads its value from `p6`, the event's
control map.

An accepted edit keeps its definitions for later evaluations in the same
session. A definition-only evaluation works too. A syntax or compile error
leaves the last accepted score and orchestra playing. Studio export retains
the accepted definitions, including instruments from an earlier evaluation.
An orchestra edit or export starts fresh engines. It does not retain running
notes or the previous engine's global values.

## Score fields

Each onset sends a Csound `i` statement. `p1` selects the instrument and `p2`
starts it at that onset. The remaining fields follow this contract:

| Field | `csound(name)` | `csoundm(name)` |
| --- | --- | --- |
| `p3` | Event duration in seconds | Event duration in seconds |
| `p4` | Frequency rounded to hertz | Fractional MIDI note number |
| `p5` | `gain * 0.2`, with a default gain of `0.8` | `gain * velocity * 127`, with defaults of `1` and `0.9` |
| `p6` | Slash-separated control names and values | Slash-separated control names and values |

Both modes resolve pitch from `freq`, then `note`, `n`, or the event value.
`p6` also contains the resolved `freq` for `csound`, or `frequency` for
`csoundm`. Read it with
`strget(p6)`, then use `keymap` to find a control. A slash inside a control
name or value cannot be represented by this map format.

Use `csoundm` for an orchestra that expects MIDI pitch and velocity. Instrument
names can also be numbers:

```js
loadCSound`
instr 77
  iFrequency = cpsmidinn(p4)
  aTone poscil p5 / 127 * .15, iFrequency
  aEnvelope linseg 0, .005, 1, max(p3 - .01, .001), 0
  outs aTone * aEnvelope, aTone * aEnvelope
endin
`
note("c4 e4 g4 b4").csoundm(77).velocity("0.4 0.7 1 0.7")
```

Csound takes over the sound source when an event has `csound` or `csoundm`.
`gain` reaches the orchestra through `p5`. With `csoundm`, `velocity` also
scales `p5`. Rustel does not apply these controls a second time.

## Load orchestra files

`await loadOrc(...)` loads orchestra text during evaluation. For a local
orchestra saved as `orchestras/glass.orc`, use:

```text
await loadOrc("local:glass.orc")
```

Start playback with its containing directory granted:

```sh
rustel play song.strudel --allow-csound --allow-local-orchestras orchestras
```

The grant confines the host's text loader to that directory. It does not
restrict what the native orchestra can do after compilation.

HTTPS URLs need an exact origin granted with `--allow-import-origin`.
The `github:owner/repository/revision/path` form uses
`https://raw.githubusercontent.com`, which needs that origin granted.
Use a commit ID for the revision so a remote edit cannot break the score.
The loader limits each response to 1 MiB and each request to eight seconds.
Successful loads are cached for the session. Change the URL or local path,
or restart the session, to reload changed text.

## Mix and export

Use `orbit`, `pan`, `postgain`, filters, delay, reverb, and VST effects as with
other instrument sources. Voices from one orchestra share its stereo output
within an insert chain, the instruments and effects assigned to one output.
Use separate orbits to isolate Csound outputs. VST effect chains can create
additional chains on an orbit. Native effects process each chain's combined
signal, so the latest event's effect controls affect its shared output. Put
per-note processing inside the orchestra when each voice needs its own settings.

Export uses the same grants as playback:

```sh
rustel export song.strudel -o take.wav --duration 30s --allow-csound
```

Rustel sets the orchestra's sample rate to the audio device or export rate.
It requires two output channels, `0dbfs = 1`, and `ksmps = 1`. Keep these host
settings out of imported orchestras. One sample per control block preserves
onset and cut timing, but costs more CPU than larger Csound control blocks.

Csound runs on a worker thread that renders into a ring buffer ahead of the
audio callback. The callback posts its commands and reads finished frames
from the ring. It never waits for Csound, so a costly opcode cannot stall
the device output. When an orchestra costs more than the run-ahead allows,
the ring runs dry, that one insert goes silent for the gap, and the rest of
the output plays on.

A scheduled note reaches the worker when the engine admits it, one schedule
lead before the note sounds, so its onset keeps its frame. A cut restarts
the orchestra on the worker thread, which also resets global state. The
compile runs in the rest after the cut, so the first note after it can start
late. Stop clears pending notes and suspends the orchestra until the next
score. Native opcode crashes are not isolated from Rustel.
