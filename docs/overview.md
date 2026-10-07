# Engine overview

Rustel runs the Strudel pattern language in a native engine.
For a first pattern, see the [quick start](../README.md#quick-start).

## How it works

QuickJS evaluates the score. The pattern engine uses exact fractions to
schedule events. Rust audio code renders synths, samples, filters, envelopes,
delay, and reverb. MIDI, OSC, and serial output can send events to other devices.

```text
score source
     |
  QuickJS             evaluates the score to a pattern
     |
pattern engine        schedules events with exact fractions
     |
     +--------------> MIDI, OSC, and serial output
     |
Rust audio code       synths, samples, filters, envelopes, delay, reverb
     |
audio device or exported file
```

Rustel also provides live input through `s("in")`, local sample folders,
offline audio export, and session tapes for recording and replaying edits.

## Sound fidelity

Native golden files detect renderer regressions. They do not prove browser
parity. Firefox is the target for live audio comparison. Agreement for sample
rate conversion and pitch playback still needs validation. An offline WAV
comparison does not establish live timing or output gain. Live comparisons
also need to check device and system mixer gain.

See [compatibility limits](compatibility.md) for missing or inactive features.

## Intentional differences

- **First beat:** Startup gives opening sounds a bounded preparation period
  before cycle zero. Unbanked native synths do not wait for sample manifests.
- **Random audio:** Noise, generated reverb, and randomized oscillator phases
  use reproducible seeds. Exact random samples can differ from the browser.
- **Refused edits:** Evaluation errors and invalid native controls in a
  replacement's first converted window leave the last good score playing.
  Valid silence remains a valid edit. Later windows still report and skip
  individual invalid voices.
- **Empty calls:** Methods with required arguments, such as `.fast()`, report
  an error. An empty control on an existing control pattern leaves it unchanged;
  `"0.5 1".gain()` still names numeric values as gain.
- **MIDI selectors:** Empty or blank port names are refused. An omitted selector
  or an index can still select a port.
- **Soundfont names:** The GM map keeps corrected bass filenames and drops
  one empty filename from the `gm_gunshot` list.
- **Registration:** Scores can replace extension names. Core names are reserved.
- **Resource limits:** Source complexity, copied values, and pattern arithmetic
  have bounds. Diagnostics identify refused operations.

## Extensions

Standard builds include native extensions from
[Switch Angel](https://github.com/switchangel/strudel-scripts/blob/706dd24814ac86193ceec6d0b263f432cba6e5ff/prebake.strudel),
with contributions from [Glossing](https://codeberg.org/glossing/Strudel_Scripts).
They also include [`inspire`](../crates/ext/src/undefined_aeon/inspire.rs),
by undefined_aeon (tzfm). It makes a repeating random melody in a scale:

```js
s("piano").seg(8).inspire("<ab:major>", 0.4, 2, 10, 4)
```

Studio's reference lists each extension's source. Extensions are not part of
upstream Strudel, and `rustel check` does not report their use. See
[credits and licenses](../NOTICE.md).

## Visuals

Default builds include terminal visualizers and native
[Hydra](hydra.md) sketches behind the editor. Hydra renders offscreen on a GPU
or a software renderer. Terminal capabilities determine how frames appear.
See [Hydra's design](hydra-design.md) for the rendering path and its tests.

## Embedding and performance

`rustel-engine` provides the pattern core, scheduler, voice preparation, and
DSP. JavaScript, sessions, and device I/O are optional. See the
[embedding guide](embedding.md).

DSP kernels follow CPU capabilities. `--acceleration portable` selects scalar
convolution, supersaw, and wavetable kernels for playback and export. RustFFT
and compiler SIMD remain independent. `rustel doctor` and Studio's About panel
report the selected kernels.
