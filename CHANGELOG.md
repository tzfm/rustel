# Changelog

## Unreleased

- Release archives and installers include signed build provenance for independent verification with GitHub CLI.
- A video file plays as a sample: `mp4`, `m4v`, `mov`, `mkv` and `webm` play their audio track.
- `_video()` and `video()` draw a video sample's picture in sync with its sound. H.264, HEVC, AV1, VP9, VP8 and Motion JPEG decode natively.
- The visuals docks show the clips of `_video()` and `video()` lanes. Enter opens a clip in a desktop window.
- Settings ▸ General ▸ biggest video sets the largest video file that loads. The default is 256 MiB.
- `s0.initVideo()` plays WebM or MKV video and animated GIF, PNG or WebP. `initImage()` and `initVideo()` read files from the set folder.
- Hydra sketches read the mouse, `width` and `height`, and set `speed`, `bpm`, `fps` and `time`. Patterns read the mouse as `mousex` and `mousey`.
- The reference knows the Hydra names, and a shared sketch with calls Rustel does not support still runs.
- Picking a plugin from the reference opens its presets and closes the call.
- Each open scene keeps its plugins loaded, and a scene you close frees them.
- Plugins stay loaded when the output device or buffer size changes, and an undo within 5 seconds reuses the previous plugin copy.
- A plugin chain with up to 1 second of latency plays in time, also in exports, and edits reach it that much later.
- A plugin that fails or sends invalid audio goes silent with one warning, and the next evaluation loads it again.
- A Studio export names a plugin that does not load, and Ctrl+C stops `rustel export` to MP3.
- Samples, images and videos load only from allowed sources. `--allow-sample-origin` adds a source, and Settings ▸ Permissions allows a refused folder.
- Scores render faster on small machines and 64-bit ARM, queries of repeating patterns run up to 180 times faster, a query for a span a table answered before gets the kept answer, and decoded samples load faster the next time.
- On a slow machine, an edit of a playing score arrives late instead of being refused. A dense synth score no longer refuses its edits.
- On Linux, the audio keeps its priority over the studio, and rustelup offers to install rtkit when needed.
- Vim mode follows Vim more closely in motions, operators, text objects, Visual mode, search, paste, undo and vimrc files.
- The studio themes, except rustel-dark, live in [rustel-studio-themes](https://github.com/tzfm/rustel-studio-themes). A build without the `themes` feature keeps rustel-dark.
- Fixed: a studio.json that another rustel version wrote keeps its settings.
- Fixed: the first chord or scale preview sounds, a scale preview keeps its tempo on a slow machine, and a sample preview says when it waits for its sample.
- Fixed: the first notes of a plugin with a named preset go through it, and a parameter pick works with a partial plugin name.
- Fixed: a terminal that answers Kitty pictures no longer types its answers into the score, and Edit ▸ Find chosen with a click takes typing.
- Fixed: in Vim mode, a key in a search box cancels a pending delete, `p` handles a Unicode line separator, and a pick on an opening quote writes inside the string.
- Fixed: a Hydra callback with a round number such as `mouse.x / 3000` compiles.
- Fixed: an edit of a playing Csound pattern no longer plays the old notes it replaces, unless Csound already started them.

### Sound changes

- The room and the delay of a note through `.vst()` go through the plugin, as its dry sound does.

## v0.1.6

- rustls 0.23.46 fixes four rustls advisories.
- File ▸ Export patterns and File ▸ Import patterns write and read Strudel pattern exports. File ▸ Open set opens an export as a new set.
- In the set panel, `/` filters the scores, 1 to 9 opens a strip score, and the foot shows the selected file's details.
- Alt or Ctrl+Shift with a digit selects that strip score from the editor.
- Settings ▸ Set ▸ sync with score metadata renames a score's file to its `@title`.
- The export sheet shows the bounce's progress and can write stems. `rustel render --stems` writes one WAV per orbit.
- `talkit('hello')` speaks text in a score, and the `talkit` command speaks or writes a WAV. Talkit is experimental. See [Talkit](https://github.com/tzfm/rustel/blob/main/docs/talkit.md).
- View ▸ Knobs opens sliders for the numbers of the call under the caret, and the presets of a plugin call.
- The vst tab of the reference is a tree of presets and parameters. Settings ▸ VST filters the plugin list.
- An unknown preset name refuses the update and gives the nearest name.
- A `slider()` on `fm` moves the sounding voice.
- `oneshot(grid)` plays a pattern once on the next grid line.
- Auto evaluate applies an edit only to the playing score, and skips an edit that adds no instruction.
- Studio reloads scores when another program saves them.
- Vim keys work in lists, menus, search boxes, find, paste and replace. Default Vim mappings ship, and the wheel scrolls in Vim mode.
- Mac Option keys type their character.
- `register()` can replace a built-in name.
- Settings has an Appearance page, and the first tab is renamed General.
- A plugin edit loads only the copy that changed, and heavy sets spread plugin chains over several cores.
- Windows reports underruns, and Csound reports when it falls behind.
- `getTime()` reads 0 when a score starts from its first cycle.
- talkit and the Hydra synth live in their own repositories, pinned by tag.
- Fixed: long renders keep every note, a mono device plays orbits sent to other pairs, `talkit` speaks from a live score, Hydra `fps` works, a comment-only score stops Hydra, and two memory leaks.
- Fixed: stray colours in Windows Terminal, wobbling Kitty pictures, and startup leftovers in Apple Terminal.
- Sample copies that could write outside the set folder are refused. Fetches retry a 429 or 5xx response.

### Sound changes

- FM modulators run from the start of the render, as in strudel.
- LFO rates above the sample clock, and negative rates, keep moving.
- A sound under an explicit bank plays the bank's asset when the bank has one.
- `s("in")` takes the FM controls.
- `partials` plays up to 2047 partials with Firefox's tables, and renders much faster.
- Sawtooth, square and triangle tables have 4096 samples at every sample rate.

## v0.1.5

- `.csound()` and `.csoundm()` play Csound instruments. They need Csound 6.18 or a later 6.x. See [Csound](https://github.com/tzfm/rustel/blob/main/docs/csound.md).
- Studio has optional Vim keys and regular expression find. See [Vim editor keys](https://github.com/tzfm/rustel/blob/main/docs/studio.md#vim-editor-keys).
- View ▸ Inline visualizers turns the inline visualizers on and off.
- A Vim mapping can name a Studio action, as `<Action:visualizers>`.
- A score that stretches every voice no longer overruns the audio device.
- A MIDI press right after a release is no longer missed.
- File picker and artwork popups have Copy and Cut.

## v0.1.4

- Any Studio action can run from a MIDI button, learnt on Settings ▸ Keybinds. See [hardware](https://github.com/tzfm/rustel/blob/main/docs/hardware.md#midi-input).
- A Studio export uses the same samples as play, and fails when a sound does not load. `rustel export --json` lists the refusals.
- Studio shows a hint when audio struggles in a CPU power-saving mode. See [hardware](https://github.com/tzfm/rustel/blob/main/docs/hardware.md#output-latency).
- Presets come from any folder you add. A dropped `.vstpreset` goes to its plugin's folder, and a call with no preset plays `default.vstpreset`.
- Preset folders are watched, so a new preset shows with no rescan.
- Settings ▸ VST lists each plugin's state, and `scan` tests only new plugins.
- The plugin scan names the plugin it tests and limits the time per plugin.
- On macOS, plugins are created and released on the main thread. A bundle keeps its working plugins when one fails.
- A plugin search prefers whole words, so `vsti("TAL")` plays TAL-NoiseMaker.
- Rustel joins a running Link session at its tempo, and two sessions that joined each other merge.
- New: `stackLeft`, `stackRight`, `stackCenter`, `stackRepeat`, `degradeByWith`, `open` and `close`. Custom `xen` scales take ratio strings and patterns.
- Voices with distortion, `crush`, FM or a supersaw render faster.
- Stop stops the set when auto evaluate is on.
- A pipe into a reader that stops early ends quietly.
- The header's warning count lets go of a warning that nothing resolves.

### Sound changes

- Euclidean rhythms take negative steps and more pulses than steps.
- Distortion curves compute `tanh` more exactly and faster.
- `randL` uses the score's random seed.

## v0.1.3

- `.vst()` and `.vsti()` play VST3 effects and instruments, with parameters and presets. Plugin support is experimental. See [Plugins](https://github.com/tzfm/rustel/blob/main/docs/plugins.md).
- Plugins are scanned in a separate process, so a plugin fault does not stop Rustel.
- `K(...)` runs KabelSalat synths and effects. See [KabelSalat graphs](https://github.com/tzfm/rustel/blob/main/docs/kabelsalat.md).
- `--link` and the Devices panel join Rustel to an Ableton Link session. See [hardware](https://github.com/tzfm/rustel/blob/main/docs/hardware.md#link).
- Settings ▸ Live coding turns on auto evaluate, which applies an edit after a typing pause.
- Studio and the CLI report device underruns apart from DSP deadline misses.
- Studio's export runs the prebakes before the score.
- A set you only open and play keeps its files unchanged.
- The bank check knows the General MIDI names and ignores case. A sound only some machines hold is a note, not an error.
- A refused voice is reported once per score text.
- `rustelup` runs from PowerShell 7 and under the default execution policy.

### Sound changes

- Stop fades a routed orbit with the main pair.
- Master volume and limiter changes ramp without clicks.
- On macOS, Studio keeps its output stream open across Stop, so there is no click.

## v0.1.2

- `rustel play song.strudel` plays a score, and `rustel export` bounces one to a file.
- `rustelup --branch main` and `rustelup --commit <hash>` build and install from source.
- A score can `import()` a helper script from a trusted origin. See [Script imports](https://github.com/tzfm/rustel/blob/main/SECURITY.md#script-imports).
- `rustel studio my_folder` creates a missing set folder.
- Settings ▸ Reference hides kinds of entries, and an empty search lists the headings by use.
- A reference search shows each function under the name you type.
- `isobite` and `cycleStruct` play the next cycle of a pattern on each event of a structure.
- `bus` takes a name.
- `getTime()`, `getCps()`, `getRandsAtTime()`, `getFrequencyFromValue()` and `.onSchedule()` are available.
- Studio marks the playing section of `arrange`.
- Studio asks what to do when it cannot save a score as you open another set.
- Pattern queries are faster.
- Library builds can leave out the scale table and the control reference text.
- Fixed: a slider move no longer silences a set with a refused voice, Space stops a sample preview the first time, and helper scripts no longer share names between scores.

### Sound changes

- The stop fade runs for the full 10 ms.
- `.pg()` and `.accent()` multiply the note's level.
- `s("one")` plays a constant signal for bus modulators.
- An `lpq` slider moves the resonance of a sounding note.

## v0.1.1

- Rustel tells you about a new release. `rustel config set check_updates false` turns this off.
- Settings ▸ Keybinds rebinds the panel shortcuts.
- Alt+click and Ctrl+click extend the selection.
- `rustls` 0.23.45 fixes [GHSA-2mjx-qc3c-rqvc](https://github.com/advisories/GHSA-2mjx-qc3c-rqvc).
- Devices with a 24-bit sample format play and record.
- Fixed: a key pressed during a resize is read at once, and `rustel render --format scalar-f32` reports the real length.

### Sound changes

- `tremolodepth(-1)` no longer writes NaN.
- A bus receiver's default sustain is 1.0.
- Live tremolo and LFOs start from the note's musical time.
- A supersaw note waits for the begin gate.
- LFO curves, ladder filters, delay feedback and supersaw spread stay in range.

## v0.1.0

First public release. The [README](https://github.com/tzfm/rustel#readme) shows what Rustel does and how to install.
