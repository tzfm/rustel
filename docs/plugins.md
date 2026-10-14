# VST3 plugins

A score plays VST3 plugins with two calls. `.vst()` sends a note through an
effect plugin. `.vsti()` plays a note on an instrument plugin.

```js
s("bd*2 sd").vst("valhalla supermassive", { mix: 0.4 })
```

```js
note("c2 eb2 g2").vsti("serum 2").vst("ott", { depth: 0.6 })
```

`vst()` and `vsti()` start a line too, with one note for each cycle. The
plugin name is a text in quotes, not a pattern.

The default build and the release binaries include plugin support. A build
with `--no-default-features` has none. The `vst` feature adds the support
back. See [building](building.md).

## Find your plugins

```sh
rustel vst
rustel vst supermassive
rustel vst serum "osc a"
rustel vst --rescan
```

`rustel vst` lists your plugins. A name prints the parameters and presets of
one plugin. A second word keeps the parameters with the word in their group,
name or key. In Studio, the vst tab of the reference panel lists the plugins,
and Ctrl-F in a `vst()` or `vsti()` call completes plugin names, parameters
and presets.

Rustel reads the standard VST3 folders:

| System | Folders |
| --- | --- |
| Linux | `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3` |
| macOS | `~/Library/Audio/Plug-Ins/VST3`, `/Library/Audio/Plug-Ins/VST3` |
| Windows | `%LOCALAPPDATA%\Programs\Common\VST3`, `%CommonProgramFiles%\VST3` |

`RUSTEL_VST3_PATH` replaces the standard folders and has the form of `PATH`.
Studio adds folders in Settings, vst tab. These folders count in Studio only.

## The plugin scan

Studio at its start, and `rustel vst`, scan each new or changed plugin bundle
one time, in a process of its own. A bundle with a fault at the scan stays
off, and rustel goes on. Studio scans in the background, so you play and edit
during the scan.

The file `~/.rustel/vst/scan.json` keeps the result. `rustel vst --rescan`,
or the rescan row in Settings, scans each bundle again. `rustel play` and
`rustel render` run no scan. They load the plugins the score names.

## Names and values

A name ignores upper case, spaces and punctuation. `"valhalla supermassive"`
finds `ValhallaSupermassive`. A part of a plugin name finds the plugin with
the shortest name: `"serum"` finds `Serum 2`.

A parameter needs its full name: `predelay` finds `Pre-Delay`. The first
column of `rustel vst <name>` is the name to write. Each value goes from 0 to
1, and the plugin maps the value to its own range. The parameter list shows
the number to write, then the display text of the plugin:
`ingain  In Gain  0.5 = 0.0 dB`.

A value is a pattern: a number, a mini-notation string or a slider.

```js
s("hh*8").vst("valhalla supermassive", {
  mix: "0.2 0.8",
  feedback: slider(0.5, 0, 1),
})
```

- The plugin takes the values at the start of the note. A slider move reaches
  the plugin with the next note.
- A parameter with no value on a later note goes back to its start value.
- A note carries 16 parameter values at most for each plugin.
- Rustel drops each value after the first 16, and each value with a name the
  plugin does not have. The note keeps its plugin.
- Studio refuses an update with a wrong parameter name of a loaded plugin.
  The last score plays on, and Studio marks the name and gives the nearest
  one.

A plugin name with no plugin, a preset name with no file, or a dropped value
gives one warning for each evaluation. The warning shows in the Studio log,
and as a `warning:` line of `rustel play`.

## Presets

`preset` names a `.vstpreset` file in `~/.rustel/vst/<plugin name>/`. Save the
preset in a DAW, then copy the file to the folder.

```js
note("c2 eb2").vsti("serum 2", { preset: "Dark Bass" })
```

## Chains

- `.vsti()` gives the plugin the pitch, the `velocity` and the length of each
  note. The `s` of the note makes no sound. One plugin copy holds 128 notes
  at one time, one for each key.
- Each `.vst()` call adds one effect to the chain, in call order. A chain
  holds 4 effects at most.
- `.vst()` with an instrument, or `.vsti()` with an effect, plays the note
  with no plugin.
- Only the notes with the call go through the plugin. Other notes on the same
  orbit stay dry.
- Plugin calls on separate patterns have separate plugin copies, also on the
  same orbit. One pattern used again in the same lane shares its copies.
- `gain` sets the level before the effects. `postgain` sets the level after
  the chain.
- `lpf`, `hpf`, `bpf`, `pan`, `room` and `delay` act on the instrument output,
  before the effects. The last note sets them for the shared instrument.
- A note with a new level or a new pan moves the value in 5 ms while the
  notes before still sound.
- The plugin output joins the orbit before the orbit fader, the DJ filter and
  the duck.
- The plugin gets the tempo of the score. One cycle is one bar of 4 beats, so
  a delay plugin set to 1/8 follows `setcpm`.
- Rustel runs 64 chains at most.

## Plugin copies and edits

Each chain has its own copy of each of its plugins. An edit keeps the copies
of each chain the edit does not change:

- A new value of a parameter or of `.orbit()`.
- A lane you add, remove or mute above the lane.
- One more `.vst()` call at the end of the chain. Only the new effect loads.

A lane is a labeled statement, such as `$:` or `lead:`, or a score with no
label. A lane stays the same lane when you move it, mute it or give it a new
label. A lane you edit stays the same lane in one of these cases:

- It keeps its label. The label `$` does not count.
- It keeps a plugin name.
- Its start and its end together keep half of its text or more.

The text counts more than the label. When one edit copies `lead:` to
`lead2:` and gives `lead:` a new text, `lead2:` is the lane that played.
`lead:` is then a new lane.

A chain with a different plugin, a different preset, or fewer effects gets
new copies. The copies before play their tail, then end.

A plugin with no input and no sound for 30 seconds sleeps and uses no CPU.
The next note wakes the same copy, with no load. A lane you remove, or put in
a comment, gives its copies back after the tail of its sound. The tail has 30
seconds at most.

A muted lane (`_$:`) keeps the copies it had when it played. A new lane with
the same plugins takes these copies. The muted lane then loads new copies at
the end of its mute.

Rustel reads the chains from the text of the score. Each arm of a `stack()`
is a chain. The text does not show a chain from a variable or from a
function. Rustel keeps the copies of such a chain until you remove one of its
plugin calls, or for 10 minutes after its last note.

## Load time and memory

Rustel loads a plugin on a thread of its own when you evaluate a score. The
audio does not stop. Until the plugin is ready, an effect note plays dry and
an instrument note is silent.

Studio has two load modes under Settings ▸ Advanced ▸ Playback. In the wait
mode, a start waits for the plugins of its first 2 cycles, and an edit keeps
the last score in play until its new plugins are ready. The wait ends after
20 seconds. In the async mode, the score starts at once.

In the wait mode, an edit waits only for the plugin copies the new score does
not have. An edit with no new chain waits for no plugin.

Memory grows with the number of chains. The Studio memory viewer lists each
loaded plugin with its copies and its load time. Studio unloads a plugin with
no use in an open tab, at the cleanup of unused samples.

## Where the audio runs

The plugin audio runs in the rustel process, on the audio thread, as in most
DAWs. Each plugin gets 128 frames at most in one call. A plugin fault during
a set ends rustel. Save your set before you play a plugin you do not trust.

Rustel drops the text a plugin prints on standard output.

## Windows plugins on Linux

Rustel loads a plugin with a Linux build directly. For a Windows plugin, a
bridge such as [yabridge](https://github.com/robbert-vdh/yabridge) with Wine
gives the plugin a Linux VST3 bundle:

```sh
yabridgectl add "$HOME/.wine/drive_c/Program Files/Common Files/VST3"
yabridgectl sync
```

The plugins then show in `~/.vst3/yabridge`. `wine` must be on `PATH` when
rustel starts. yabridge 5.1.1 needs Wine Staging 9.21 or earlier.

## Limits

- A plugin shows no window. Activate a plugin with a licence window in a
  different host first.
- Rustel does not compensate plugin latency. A look-ahead effect plays late by
  its look-ahead.
- A side-chain input of a plugin gets silence.
- `cut` does not stop a note of an instrument plugin.
- Native `distort`, `crush`, `coarse` and `compressor` do not process the
  output of an instrument plugin.
- VST2, AU and CLAP plugins are not supported.

A plugin is native code with the rights of your user. See
[Security](../SECURITY.md#plugins). To time a plugin chain, see
[Measure a plugin](vst-performance.md).
