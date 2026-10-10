# Changelog

## Unreleased

- With an empty search, the reference lists its headings by use across the 186 songs in the score corpus. Temporal, tonal and audio come first. A search still opens on the best match.
- Settings ▸ Reference has five switches, all on by default. Turn one off to hide a kind of entry from the reference and the editor suggestions. The kinds are OSC and SuperDirt-only controls, `serial()`, the FM routing matrix `fmi11` to `fmi88`, the `bind` and `join` family, and pattern internals such as `withHap`. `tag:` search still lists a hidden kind, and Ctrl+D still opens its page. `fadeInTime`, `chorus`, `oschost` and `oscport` now carry the `osc` tag. `dry`, `bus`, `busgain`, `octave`, `unit` and `phaserdepth` play natively, so they lose the `superdirt` tag.
- Pattern queries are faster. Cycle time arithmetic uses 64-bit math when numerators and denominators are below 2^31. Every operation used 128-bit math. `s("[bd hh sd hh]*8").fast(2)` queries 1.7 times faster over whole cycles and 1.65 times faster over a 25 millicycle live window, in a release build on one x86-64 desktop. Events stay the same.
- Scores with several controls query faster. A control such as `.gain(0.8)` built a lookup table for each event, and only `pick` reads the table. Rustel now builds the table when a `pick` asks. `s("[bd hh sd hh]*8").n(3).gain(0.8).pan(0.25).cutoff(800).room(0.3)` queries 1.9 times faster in a release build on one x86-64 desktop. Events stay the same.
- A host which builds `rustel-core` with no default features drops the scale table (244 KB compressed) and its decoder. The new `tuning-list` feature holds both and is on by default. Named scales are unknown in the smaller build, and a scale given as a frequency list works as before. A default build and every Rustel product build stay unchanged.
- A build which never reads the reference text of the controls no longer links the text, about 230 KB uncompressed. The control registry reads a table of names and aliases, and only code which shows the text reads the full table. A product build is unchanged.
- The new `transpiler::on_caller_stack(work)` runs every parse of `work` on the calling thread. Each parse spawned a thread with 256 MiB of stack. A host with no threads got a refusal diagnostic from every `transpile` call, and a host on a large stack of its own paid for a thread the host did not need. The nesting checks still run first, and the caller owns the stack size. Existing callers stay unchanged.
- `rustelup --branch main` and `rustelup --commit <hash>` build rustel from source and install the binary. Use them to test a change before its release. The build needs `git` and `rustup`. Run the install command again to get the new `rustelup`.
- `rustelup` keeps the backup of the previous binary when a restore fails and prints its path. The cleanup deleted the backup with the stage directory.
- `rustel play song.strudel` plays a score, and the playing flags are in `rustel play --help`. `rustel song.strudel` is removed and stops with a tip for `rustel play`. `play` was a hidden alias of `trace`, so `rustel play song.strudel` ran with no sound.
- `rustel export` accepts `--prebake` and is the one command for a bounce to a file. `rustel play` has no `--export`, `--cycles`, `--until-silence`, `--silence-floor`, `--silence-hold` or `--sample-rate`. The old `--export` wrote WAV only, with a default length of 2 seconds.
- `rustel-ext` no longer turns the scale table on. A host which builds `rustel-core` with no default features keeps the table off and still gets the extension names. A default build stays unchanged.
- `rustel studio set1`, with no `set1` on disk, creates the set folder `set1` with a starter scene. File > Open set does the same with a typed path. A missing path that ends in `.strudel` still opens a starter score. If the parent folder is missing or locked, the launch stops and names the path. Before, Studio opened `set1` as a score in the parent folder. The set panel never listed it, and with `/` as the parent each save failed with `Permission denied (os error 13)`.

### Sound changes

- The stop fade runs for the full 10 ms. The fade ended after 128 frames, about 3 ms at 48 kHz, and left a click.

## v0.1.1

- A key pressed at the same moment as a terminal resize reaches Studio at once. The key stayed unread until the next key, and Studio used one full CPU core while it waited.
- Rustel announces new stable releases with a `rustelup` hint. Studio prints the notice after you quit. Disable checks with `rustel config set check_updates false`.
- Theme effects keep `//`, `=>` and other punctuation ligatures whole. The waves themes split them while a wave passed.
- Alt+click and Ctrl+click extend the selection, as Shift+click does. kitty keeps Shift+click and does not send it to Studio.
- Keyboard help says when the terminal does not send Shift+Home and Shift+End. Ghostty on Linux keeps both keys for its scrollback. See [Selecting text](https://github.com/tzfm/rustel/blob/main/docs/studio.md#selecting-text).
- Settings ▸ Keybinds rebinds the panel shortcuts: show file, rename, delete sample, trim sample, and focus the tape timeline.
- `rustls` moves from 0.23.43 to 0.23.45. Versions 0.23.13 to 0.23.44 accept TLS 1.3 handshake messages at the wrong encryption level ([GHSA-2mjx-qc3c-rqvc](https://github.com/advisories/GHSA-2mjx-qc3c-rqvc)). Sample and image downloads use `rustls`.
- `rustel render --format scalar-f32` reports the real length. `--duration 8` printed `16.0 s, 8 cycles`.
- CI runs the tests of the hand-written `unsafe` code under [Miri](https://github.com/rust-lang/miri) on each pull request. Miri stops on a memory error or a data race in the audio rings, the AVX2 kernels, the callback host pointers and the QuickJS allocator.
- The QuickJS allocator reports 0 usable bytes for a null pointer. The size read had no null check.
- Rustel plays through and records from devices with a 24-bit sample format. The output stayed silent with `sample format i24 is not supported`.

### Sound changes

- `tremolo(4).tremolodepth(-1)` wrote NaN to the output. The tremolo gain takes 1 for those samples.
- The default sustain of a bus receiver is 1.0. The default was 0.6, so the level fell during each 50 ms decay and returned to full level at the next start. ADSR controls you set stay unchanged.
- Live playback starts tremolo, each filter LFO and `lfo()` with no retrig from the musical time of the note, `cycle / cps`. Live playback read the clock, so the sweep started at a new point on each play. A render does not change.
- A supersaw note waits for the begin gate, as pulse and wavetable notes do. Each supersaw note starts 1 to 128 frames later. With an LFO on the filter, some notes started with a low thump.
- A fractional `curve` in `lfo()` with a negative LFO value gives NaN. The target takes its default for those samples, 350 Hz for a filter frequency. The cutoff went to 20 Hz.
- A ladder filter with a cutoff below 0 or a resonance below -7.69 went to infinity. Both are bounded. Delay feedback and the supersaw spread stay in -1..1 under modulation.

## v0.1.0

First public release. The [README](https://github.com/tzfm/rustel#readme) shows what Rustel does and how to install.
