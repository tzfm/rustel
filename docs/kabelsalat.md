# KabelSalat graphs

Use `K(...)` to build a synth or process another sound with a KabelSalat
graph. End each audible branch with `.out()`:

```javascript
$: K(sine(sFreq).mul(0.2).out()).note("c3 e3 g3 b3")
```

`sFreq` is the event's final frequency in Hz, including a `note` or `freq`
control applied after `K`. It is rounded in its last digits, so every
platform binds the same value. `sGate` is the event's gate. `time()` starts
at zero for each graph. Graph state belongs to one voice, so overlapping notes
have independent oscillators, envelopes, noise generators and delay lines.

## Patterns and envelopes

Double-quoted mini-notation inside `K` becomes a graph input. `S(pattern)`
lifts an existing pattern expression into the same input list. Each event
receives numeric values. Ordinary JavaScript arithmetic works on them.

```javascript
$: K(
  saw(sFreq / "1 2 1 4")
    .mul(adsr(sGate, 0.01, 0.1, 0.5, 0.15))
    .mul(S(sine.range(0.1, 0.25).slow(4)))
    .out()
).note("c3 e3 g3 e3").release(0.2)
```

Use the pattern's `release` control to leave time for a graph envelope's
release. Use `sGate` with a graph envelope such as `adsr` to shape a generated
signal. The source's normal amplitude envelope shapes the audio entering
`audioin()`. Generated graph output then passes through the voice's gain and
effect controls. A graph without `.out()` produces silence.

`worklet(source, ...inputs)` is the explicit source form. The source is
score text that calls the native graph API. `pat[0]`, `pat[1]` and later
entries hold the input values in order:

```javascript
$: note("c3 e3").worklet('sine(sFreq).mul(pat[0]).out()', "0.1 0.2")
```

Graph code runs when the final event is queried. Pattern values and pitch
are resolved before the graph is compiled. Node names such as `saw`, `adsr`
and `audioin` are local to this graph. They do not replace score functions.

## Effects and channels

`audioin()` reads the preceding audio at the current source or effect stage.
It takes that signal's first channel. A graph that does not use `audioin()`
generates its own signal instead. `K` effects share the normal limit of three
FX stages per voice.

```javascript
$: s("sawtooth").note("c2 e2 g2 b2")
  .FX(K(audioin().distort(0.4).mul(0.3).out()))
```

Arrays expand a graph into channels. When the array lengths differ, shorter
arrays wrap. `.out()` sends to output channels 0 and 1. `.out(0)` sends to
the left channel and `.out(1)` to the right. Output contributions add.

```javascript
$: K(sine([sFreq, sFreq * 1.005]).mul(0.15).out([0, 1]))
  .note("c3 g3")
```

The graph has 16 internal output channels. Native stereo playback reads
channels 0 and 1. These output indices are local to the graph, not Rustel's
named audio buses.

## Graph construction

The graph API includes chaining, channel expansion, `poly`, `mix`, `split`,
`pick`, `pan`, `apply`, `map`, `channel`, registered functions and modules.
Use a block when several expressions build the graph:

```javascript
$: K(() => {
  const voice = module('voice', frequency => frequency.saw().lpf(0.5).mul(0.1));
  voice(n(sFreq)).out();
  n(sFreq * 2).voice().out();
}).note("c2 e2 g2")
```

`register` and `module` expand into the supported primitive nodes. These
registrations live for this graph construction. They do not alter another
event's graph or the score's pattern registry. Feedback connections retain
the upstream traversal order: an edge to a node evaluated later reads its
previous sample, initially zero.

Graph operations installed through `register()` also have underscore bypass
methods. This convention does not apply to every helper or to methods assigned
directly to `Node.prototype`. For example,
`.abs()` takes the absolute value of a signal, while `._abs()` returns the
signal unchanged. Change `.lpf(.5)` to `._lpf(.5)` to temporarily disable
the filter without removing it from the chain. These are bypasses, not aliases.
Their arguments are ignored, though argument expressions still run while
building the graph. The standalone `_` helper is different: like `n`, it
constructs a constant node.

The native node families include:

| Family | Nodes and helpers |
| --- | --- |
| Sources | `sine`, `saw`, `zaw`, `tri`, `pulse`, `noise`, `rng`, `pink`, `brown`, `dust`, `impulse`, `lfnoise`, `audioin`, `time` |
| Gates and control | `adsr`, `ar`, `ad`, `perc`, `clock`, `clockdiv`, `seq`, `hold`, `trig` |
| Processing | `filter`, `lpf`, `hpf`, `bpf`, `qf` and its filter helpers, `delay`, `distort`, `fold`, `slide`, `lag`, `slew` |
| Arithmetic | Arithmetic and trigonometric functions, `min`, `max`, `argmin`, `argmax`, comparisons, Boolean helpers, `ifelse`, `clamp`, `clip`, `range`, `rangex`, `remap`, `midinote`, `bipolar`, `unipolar` |
| Expressions | `raw`, `bytebeat`, `floatbeat` |

`raw`, `bytebeat` and `floatbeat` compile numeric expressions to native
operations. They accept arithmetic, exponentiation, bitwise operations,
comparisons, logical operations and ternary expressions. Math constants
`Math.PI` and `Math.E` are available, along with the `Math` functions `abs`,
`floor`, `ceil`, `round`, `sign`, `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `log`, `exp`,
`sqrt`, `min`, `max`, `pow` and `atan2`. `Math.min` and `Math.max` take exactly
two arguments. The input is available as `t` or
`$input`, and `time` gives elapsed seconds. Expressions cannot call arbitrary
JavaScript, create objects, assign variables or run loops during audio
processing. Score expressions use the existing JavaScript interpreter and
its execution budget. Graph construction, library helpers, graph and
expression compilation, and audio processing are implemented in Rust.
No KabelSalat JavaScript implementation is loaded or run.

Random sources use a deterministic seed for each node and voice, so their
samples can differ from the upstream process's shared random history.
Nonfinite samples become silence at the graph output. This library version's
`floatbeat` helper expands to `bytebeat`, and that behavior is preserved.

## Limits

- Graph source is limited to 64 KiB and numeric pattern inputs to 1024
  values. Inputs, frequency and numeric constants must be finite.
- A graph has at most 256 native operations and 1024 input connections.
  Expression operations count toward these limits. Each expression string
  is at most 8192 UTF-8 bytes. Expression nesting is limited to 64 levels.
- Graph construction has a separate budget of 8192 temporary nodes,
  32768 builder operations and 64 nested native calls.
- Delay time is at most 10 seconds. The native audio graph pool is bounded
  to 128 graph instances and 64 MiB, including their delay storage. A score
  that exceeds the available capacity reports a preparation error. Live storage
  becomes reusable for a different graph size after all active and queued K
  notes finish or are cancelled. While K notes remain active or scheduled, the
  pool conservatively retains its reservations. Retry after playback drains.
- The producer holds at most 4096 distinct compiled programs. Live graph
  preparation also has a bounded queue of 512 pending transfers.
- Persistent delay identifiers report an error. Use `delay(input, seconds)`
  or `input.delay(seconds)`. Every new voice starts with empty state.
- `midifreq`, `midigate`, `midivel`, `midicc`, `cc`, `mouseX` and `mouseY`
  have no external input connection and report an error. Use `sFreq`,
  `sGate` and patterned numeric inputs for event controls.
- `scope` passes audio through without opening a visualizer. `B` and `_`
  construct constant nodes without interactive widgets.
- Custom JavaScript audio processors registered with `registerUgen` cannot
  run in the native audio engine. Unknown primitive types report an error.
- `compile()` returns native graph JSON. Generated-source `evaluate()` and
  `exportModule()` are unavailable. Node inlet metadata is retained.
  Source generators are omitted.
- Mondo is a separate editor language and is not registered. `K` accepts
  JavaScript graph expressions.

The graph and library API is adapted from KabelSalat core 0.4.0 and library
0.4.1 at revision `d114e806b6fd8ca948f19613db2cf787e4dcd1d6`. See
[NOTICE](../NOTICE.md) for attribution. This version's graph macros and
numeric behavior define the compatibility target.

## Editor reference

Studio documents the graph functions, aliases, node methods and inputs under
`tag:kabelsalat`. Ctrl+D opens help for the call at the caret. In the graph
expression passed to `K(...)`, help and completion use the
graph definitions of names such as `sine`, `seq` and `adsr`. Inside
`S(pattern)`, they use the pattern definitions. Graph inputs such as `sFreq`
and `sGate` insert as values.

Browsing and completion show the regular operations, such as `abs`, without
separate bypass entries. Type `_` or search `tag:bypass` to find bypasses
explicitly. Help on an existing bypass call still explains which operation
it disables.

Settings > Reference > KabelSalat hides these entries from browsing and
completion. The setting persists across launches. An explicit
`tag:kabelsalat` search or Ctrl+D lookup still opens their documentation.
Unsupported external inputs and source generators explain their limits in
the reference and do not appear as graph completion suggestions.

## Implementation plan and verification

The implementation crosses four boundaries. Each boundary has its own
acceptance checks so a graph can be traced from source to rendered samples.

1. Preserve graph syntax and pattern timing. The transpiler extracts
   mini-notation and `S` inputs into the existing pattern combinators.
   Checks cover expressions, blocks, methods, nested calls, source locations
   and graph names that differ from score names.
2. Compile the final event on the score thread. An isolated graph namespace
   calls Rust graph and library functions and exports bounded node data.
   Checks cover final pitch, numeric inputs, modules, multichannel wrapping, feedback,
   independent registrations and resource refusals.
3. Validate and execute native operations. The native program validates
   node names, references, output channels and expressions before
   scheduling. DSP checks cover oscillator phase, envelope and gate timing,
   filters, sequencers, noise, Boolean identity and delay state.
4. Prepare state before audio playback. Source and FX stages receive their
   own prepared graph. Audio processing only evaluates native operations.
   Render checks cover source replacement, `audioin`, FX order, stereo,
   overlapping voices, release tails and live/offline agreement. Callback
   checks cover allocation-free onset, processing and retirement, with
   graph capacity enforced before an onset is accepted.

JavaScript execution and expression parsing stay outside the audio callback.
Invalid source fails with a diagnostic before the graph reaches playback.
