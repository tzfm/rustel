# Reference vectors

These files contain results captured from KabelSalat commit
[`d114e806b6fd8ca948f19613db2cf787e4dcd1d6`](https://github.com/felixroos/kabelsalat/tree/d114e806b6fd8ca948f19613db2cf787e4dcd1d6).
The native tests consume the stored data. No reference runtime is required.

## Unit generators

`ugens.json` contains 48 consecutive updates for each fixture. The `rate`,
`inputs`, and `time` fields contain every argument used in the capture.
Each fixture starts with a fresh class from `packages/lib/src/ugens.js`.
The constructor receives node ID 0, an empty input list, the fixture sample
rate, and an empty message callback. The global sample rate matches `rate`.

Pass each input row to `update` in order. For `adsr`, prepend the matching
`time` value. Store each result in `expected` without rounding. Reject
non-finite results.

| Fixture kind | Reference class |
| --- | --- |
| adsr | ADSRNode |
| clock | Clock |
| clockdiv | ClockDiv |
| distort | Distort |
| hold | Hold |
| lcgnoise | LcgNoise |
| noise | NoiseOsc |
| dust | DustOsc |
| brown | BrownNoiseOsc |
| pink | PinkNoise |
| impulse | ImpulseOsc |
| pulse | PulseOsc |
| zaw | ZawOsc |
| saw | SawOsc |
| sine | SineOsc |
| tri | TriOsc |
| lag | Lag |
| slew | Slew |
| slide | Slide |
| filter | Filter |
| bpf | BPF |
| fold | Fold |
| seq | Sequence |
| pick | Pick |
| remap | Remap |
| clip | Clip |
| trig | Trig |
| delay | Delay |
| qf | BiquadFilter |

Before constructing each class, replace its random source with this sequence.
Start the unsigned 32-bit state at 2891336453. Each draw updates the state to
`(1664525 * state + 1013904223) mod 4294967296`, then returns the state divided
by 4294967296. The eight `qf` fixtures cover filter types 0 through 7.

## Primitive operations

`primitives.json` captures the registered compiler functions in
`packages/lib/src/lib.js`, with the JavaScript language backend from
`packages/lib/src/lang/js.js`. Register the library against the matching
`packages/core/src/index.js` from the same commit.

For each row, compile the named `kind` with one variable per `inputs` item,
the result name `result`, and language `js`. Evaluate the resulting operation
with those input values. `expected` contains its numeric conversion, with
non-finite values replaced by zero. `boolean` records whether the result has
Boolean type. `zero` records strict equality with numeric zero before the
conversion. These fields preserve the distinction between numeric zero and
Boolean false.

Empty-input `min`, `max`, `argmin`, and `argmax` cases are absent because the
reference compiler rejects them. All other input rows, including Boolean
values and omitted arguments, remain in the fixture.
