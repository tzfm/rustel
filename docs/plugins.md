# VST3 plugins

A score plays through VST3 plugins with two calls. `.vst()` sends a note
through an effect plugin. `.vsti()` plays a note on an instrument plugin.

```js
s("bd*2 sd").vst("valhalla supermassive", { mix: 0.4 })
```

```js
note("c2 eb2 g2").vsti("serum 2").vst("ott", { depth: 0.6 })
```

A plugin call starts a line too, with one note for each cycle. Each `.vst()`
call after the first adds one more effect to the chain.

```js
vsti("serum 2").seg(8).note("<c2 eb2>")
  .vst("ott", { depth: 0.3 }).vst("valhalla supermassive")
```

The default build and the release binaries include plugin support. A build with
`--no-default-features` leaves plugin support out. The `vst` feature adds the
support back. See [building](building.md).

## Find your plugins

```sh
rustel vst
rustel vst supermassive
rustel vst serum "osc a"
rustel vst --rescan
```

`rustel vst` lists the plugins in the plugin folders. A name loads one plugin
and prints its parameters and presets. A plugin with more than 40 parameters
prints its top level and the names of its groups. A second word prints the
parameters with the word in their group, name or key.

In Studio, the vst tab of the reference panel shows the same list. Expand a
plugin to load the plugin and see its parameters. Press Ctrl-F with the caret
in a `vst()` or `vsti()` call to browse the parameters of the plugin. With the
caret in the plugin name, Ctrl-F suggests names, and the vst tab shows the
parameters of the plugin. Enter puts the selected parameter and its default in
the options of the call, and adds the `{}` when the call has none. Enter keeps
a value the call has already and adds no key a second time. A key you typed in
part gets the same completion.

The plugin folders are the standard VST3 folders of the system:

| System | Folders |
| --- | --- |
| Linux | `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3` |
| macOS | `~/Library/Audio/Plug-Ins/VST3`, `/Library/Audio/Plug-Ins/VST3` |
| Windows | `%LOCALAPPDATA%\Programs\Common\VST3`, `%CommonProgramFiles%\VST3` |

Studio adds folders in Settings, vst tab. With `RUSTEL_VST3_PATH` set, the
folders in the variable take the place of the standard folders. The variable has
the form of `PATH`.

## The plugin scan

Rustel scans plugins as a DAW does. The scan loads each plugin bundle one time,
in a process of its own, and reads the plugins of the bundle. A bundle with a
fault is reported and stays off, and rustel goes on.

The scan runs in the background when Studio starts, and when you run
`rustel vst`. Studio is ready at once: play and edit while the scan runs. The
list of background jobs has the row `scan plugins` with its progress.

The scan cache keeps the plugins of each bundle, in the file
`~/.rustel/vst/scan.json`. A bundle with the same files as at its scan is not
loaded again, so the next start has each plugin by name and kind at once. A
bundle with a changed file, a new bundle, and a bundle with a fault are in the
scan of the next start.

`rustel vst --rescan`, and the rescan row of the vst tab in Settings, empty the
cache and scan each bundle again.

## Names

A plugin name ignores upper case, spaces and punctuation:
`"valhalla supermassive"` finds `ValhallaSupermassive`. A part of a name finds
the plugin with the shortest name with the part: `"serum"` finds `Serum 2`.

A parameter name follows the same rule: `predelay` finds `Pre-Delay`. The first
column of `rustel vst <name>` is the name to write. A parameter with an empty or
repeated title takes its number as the name.

## Values

Each value goes from 0 to 1. The plugin maps the value to its own range. A value
is a pattern: a number, a mini-notation string or a slider.

The parameter list shows the default as a number from 0 to 1 for the score.
When the plugin's display text differs, it appears beside the number:
`ingain  In Gain  0.5 = 0.0 dB`. Use the number in the score.

```js
s("hh*8").vst("valhalla supermassive", {
  mix: "0.2 0.8",
  feedback: slider(0.5, 0, 1),
})
```

A note carries 8 parameter values at most for each plugin. The plugin takes the
values when the note starts, so a slider move reaches the plugin with the next
note.

A parameter omitted by a later note returns to its value when the plugin copy
was created, after loading its preset. Removing a key from the score takes
effect on that later note. Notes at the same onset share their controls.
Patterns on one orbit share a copy when they use the same plugin and preset
at the same chain position. Its parameter values affect all sound in that copy.
Each copy tracks up to 32 changed parameters for restoration. A parameter beyond
this limit can keep its last value.

Studio marks a key the plugin has no parameter for, and gives the nearest
name: `OTT has no parameter "dpth" - did you mean "depth"?`. A note with such a
key plays with no plugin.

## Presets

`preset` names a `.vstpreset` file in `~/.rustel/vst/<plugin name>/`. Save the
preset in a DAW, then copy the file to the folder. The preset name ignores upper
case, spaces and punctuation, as a plugin name does.

```js
note("c2 eb2").vsti("serum 2", { preset: "Dark Bass" })
```

## Effects and instruments

- `.vst()` is for an effect. Only the notes with the call go through the
  plugin. A note on the same orbit with no `.vst()` stays dry.
- `.vsti()` is for an instrument. The plugin gets the pitch, the level (`gain`
  times `velocity`) and the length of each note, and makes the sound. The engine
  voice of the note is silent, so `s` is not heard. The controls of the engine
  voice, such as `lpf`, `decay`, `unison` and `detune`, do not reach the plugin.
  Set the sound with the parameters of the plugin or with a preset.
- With both calls on a note, the instrument goes through the effects.
- A second `.vst()` call on a note adds a second effect. The note goes through
  the effects in call order. A chain holds 4 effects at most.
- One orbit holds one effect chain and one instrument plugin. All patterns on
  the orbit share them, and the chain of the last note is the chain in use. A
  different plugin at the same place of the chain takes the place of the first.
  Use `.orbit(2)` for a second chain or a second copy.
- Changing only one literal `.orbit(1)` to `.orbit(2)` can reuse the plugin
  chain. The chain must be written directly, used only once, and move to an
  orbit with no plugin. Bindings, indirect reads, nested plugin calls and shared
  chains in the source prevent reuse.
  The plugin state and its tail follow the new output. The old orbit's native
  `room` and `delay` tails stay on the old orbit. Other orbit edits can need a
  separate copy.
- The plugin output joins the orbit before the orbit fader, the DJ filter and
  the duck. `delay` and `room` take the engine voice, not the plugin output.
- The plugin gets the tempo and the bar position of the score. One cycle is one
  bar of 4 beats, so a delay plugin set to 1/8 follows `setcpm`.
- An effect with no input and no output for 30 seconds sleeps until the next
  note. An instrument does the same.
- A rewind ends the notes an instrument holds, at the frame the engine voices
  stop.

## Load time and memory

Rustel loads a plugin when you evaluate a score with the plugin name, on a
thread of its own. The audio does not stop. Until the plugin is ready, an effect
note plays dry and an instrument note is silent. The first load of a bridged
plugin takes 1 to 2 seconds. A score change later keeps the loaded plugin.

In Studio, the list of background jobs has a row for each plugin in its load,
and one for the [plugin scan](#the-plugin-scan).

In Studio, the load mode sets what the first notes do. In the wait mode, a
start holds its downbeat until each plugin of its first 2 cycles is ready for
its orbit. An edit keeps the last score in play until its new plugins are
ready. The header shows `waiting for plugins`. A plugin not ready after 20
seconds holds the score no longer. In the async mode, the score starts at once
and the first notes play as above. The load mode is under Settings ▸ Advanced ▸
Playback.

For an edit, Studio reads the plugin calls from the score text: the name and
the `preset` as quoted strings, the place of each `.vst()` call in its chain,
and the number of the one `.orbit()` call of the statement. With the name in a
variable, the edit does not wait. A change of parameter values, or a new
control on the same plugin, starts no new wait. When a new copy is needed,
Studio prepares it in the background. The chain in play stays in place until
the notes of the new score need the new copy.

Each orbit with a plugin holds one copy of the plugin. Using the same plugin
on two orbits holds two copies, and so does a second place in a chain. The
Studio memory viewer lists each loaded plugin with its copies, its load time,
and on Linux the memory of its process.

See [VST performance measurements](vst-performance.md) for a repeatable test
of plugin startup, processing, parameter changes and prepared-copy reuse.

Studio unloads a plugin when no open tab names the plugin, the reference panel
does not show the plugin, and no output plays through the plugin. The unload
runs with the cleanup of unused samples: after a stop, a new score or a closed
tab, when the `drop unused` time of Settings has passed since the last sample
preview. With `drop unused` at `never`, a loaded plugin stays loaded. The next
use loads the plugin again.

## The plugin process

Each plugin bundle runs in a process of its own. Rustel starts the process at
the first use of a plugin of the bundle, and each copy of the plugins of the
bundle runs there. The audio goes to the process and back for each block of 128
frames at most, over a local socket.

A plugin fault ends the plugin process and not rustel, at the load and in the
middle of a set:

- The set plays on. An effect passes its input, and an instrument is silent.
- Rustel says `the plugin process ended`, with the exit status of the process.
  On Windows, the code `0xc0000005` is a memory fault in the plugin.
- The next note starts the plugin again, in a new process. The first notes play
  as at a first load.
- A bundle with 3 such ends stays off until the next rescan.

A plugin process with no answer for 1 second counts as ended, and rustel says
`the plugin process gave no answer`. The sound of the set stops for this
second, one time. The first block of a copy has 5 seconds.

In a render to a file, a plugin with a fault stays off to the end of the
render.

## Windows plugins on Linux

Rustel loads each VST3 plugin with a Linux build directly, and needs no more
for these. Rustel has no code for Wine or for a bridge.

Most commercial plugins have no Linux build. A bridge gives such a plugin a
Linux VST3 bundle, and rustel loads the bundle as one more plugin. One such
bridge is [yabridge](https://github.com/robbert-vdh/yabridge) with Wine:

1. Install Wine Staging 9.21. yabridge 5.1.1 does not work with Wine 9.22 or a
   later version.
2. Run the Windows installer of the plugin with Wine.
3. Add the Wine VST3 folder to yabridge and sync:

```sh
yabridgectl add "$HOME/.wine/drive_c/Program Files/Common Files/VST3"
yabridgectl sync
```

The plugins then show in `~/.vst3/yabridge`, and `rustel vst` lists them.
`wine` must be on `PATH` when rustel starts. Each bridged plugin also has a Wine
process: about 45 MB for a small effect and 435 MB for one copy of a large
synth.

## Limits

- A plugin shows no window. A plugin with a licence or sign-in window needs one
  activation in a different host.
- Rustel does not compensate plugin latency. A look-ahead effect plays late by
  its look-ahead.
- A plugin with no answer stops the sound for 1 second before rustel takes the
  plugin out: see [The plugin process](#the-plugin-process).
- `cut` does not stop a note of an instrument plugin.
- VST2, AU and CLAP plugins are not supported.

A plugin is native code with the rights of your user. See
[Security](../SECURITY.md#plugins).
