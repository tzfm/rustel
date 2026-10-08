# Changelog

## Unreleased

- Rustel announces new stable releases with a `rustelup` hint. Studio prints the notice after you quit. Disable checks with `rustel config set check_updates false`.
- Theme effects keep `//`, `=>` and other punctuation ligatures whole. The waves themes split them while a wave passed.
- Alt+click and Ctrl+click extend the selection, as Shift+click does. kitty keeps Shift+click and does not send it to Studio.
- Keyboard help says when the terminal does not send Shift+Home and Shift+End. Ghostty on Linux keeps both keys for its scrollback. See [Selecting text](docs/studio.md#selecting-text).
- Settings ▸ Keybinds rebinds the panel shortcuts: show file, rename, delete sample, trim sample, and focus the tape timeline.
- `rustls` moves from 0.23.43 to 0.23.45. Versions 0.23.13 to 0.23.44 accept TLS 1.3 handshake messages at the wrong encryption level ([GHSA-2mjx-qc3c-rqvc](https://github.com/advisories/GHSA-2mjx-qc3c-rqvc)). Sample and image downloads use `rustls`.
- `tremolo(4).tremolodepth(-1)` wrote NaN to the output. The tremolo gain takes 1 for those samples.
- The default sustain of a bus receiver is 1.0. The default was 0.6, so the level fell during each 50 ms decay and returned to full level at the next start. ADSR controls you set stay unchanged.
- Live playback starts tremolo, each filter LFO and `lfo()` with no retrig from the musical time of the note, `cycle / cps`. Live playback read the clock, so the sweep started at a new point on each play. A render does not change.
- A supersaw note waits for the begin gate, as pulse and wavetable notes do. Each supersaw note starts 1 to 128 frames later. With an LFO on the filter, some notes started with a low thump.
- A fractional `curve` in `lfo()` with a negative LFO value gives NaN. The target takes its default for those samples, 350 Hz for a filter frequency. The cutoff went to 20 Hz.
- A ladder filter with a cutoff below 0 or a resonance below -7.69 went to infinity. Both are bounded. Delay feedback and the supersaw spread stay in -1..1 under modulation.
- `rustel render --format scalar-f32` reports the real length. `--duration 8` printed `16.0 s, 8 cycles`.
- CI runs the tests of the hand-written `unsafe` code under [Miri](https://github.com/rust-lang/miri) on each pull request. Miri stops on a memory error or a data race in the audio rings, the AVX2 kernels, the callback host pointers and the QuickJS allocator.

## v0.1.0

First public release. The [README](https://github.com/tzfm/rustel#readme) shows what Rustel does and how to install.
