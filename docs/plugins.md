# VST3 plugins

A score plays VST3 plugins with two calls. `.vst()` sends a note through an
effect plugin. `.vsti()` plays a note on an instrument plugin.

Plugin support is experimental.

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
and presets. → on a plugin opens the two rows under it, its presets and its
parameters, and → on one of the two makes that half the view of the tab, with
its own search and a trail on the search line. ← and Esc go back, Tab flips
the halves, and a search of the plugin list reads plugin names only.

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
off, and rustel goes on. In a bundle with several plugins, a plugin that
does not start stays off with its reason, and the others stay on. The fault
stays in the cache with the stamp of the bundle, so an unchanged bundle
gets no new test. A rescan tries it again. Each class of a bundle has two
minutes at the scan, so a bundle with many slow classes scans to its end
while a class that hangs still fails.
Studio scans in the background, so you play and edit during the scan.

The scan names the plugin in its test: on the status line of `rustel vst`,
in the Studio background jobs and on the scan row in Settings. A shell
bundle holds many plugins. The name then has the bundle first, as
`Shell › Comp`. A plugin that stops the scan stays on the screen, so you know
which plugin to update or remove. A bundle that ends the scan process, or
gives no answer in 2 minutes, stays off. Its fault names the plugin in its
load, as `the plugin process did not answer while it loaded Comp`. In
Studio, the plugin row on the vst page of Settings shows the fault.

The file `~/.rustel/vst/scan.json` keeps the result. `rustel vst`, or the
scan row in Settings, tests only new or changed bundles, for example after you
install a plugin. `rustel vst --rescan`, or the rescan row in Settings,
empties the cache and scans each bundle again. `rustel play` and
`rustel render` run no scan. They load the plugins the score names.

## Names and values

A name ignores upper case, spaces and punctuation. `"valhalla supermassive"`
finds `ValhallaSupermassive`. A part of a plugin name finds the plugin with
the shortest name: `"serum"` finds `Serum 2`. A part that is a whole word of
a name comes first, then a part at the start of a word, then a part inside a
word. `"tal"` finds `TAL-NoiseMaker`, not `Talk Box` or `Vital`.

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

- The plugin takes the values at the start of the note.
- A slider that is the value of a key, as `feedback` above, moves the
  parameter value at once, also in a note that sounds. During a drag the
  plugin gets the newest value every few milliseconds, and smooths each change
  in its own way. A note that rustel queued before a move plays the moved
  value too. The output keeps the last moves of 64 values for such notes.
- After an edit, a slider moves the parameter value at once again from the
  time rustel queues the first note of the chain after the edit. Before that
  time, a note from before the edit keeps its value. Rustel queues a note up
  to about half a second before it plays.
- A slider in an expression or with a time transform, as `slider(0.5).mul(2)`
  or `slider(0.5).fast(2)`, moves the value with the next note.
- The next move of a slider overrides a value that code of the score wrote
  for its key.
- A score moves 1024 parameter values with sliders at most. A slider past that
  moves its value with the next note.
- A parameter with no value on a later note goes back to its start value.
- A note carries 16 parameter values at most for each plugin.
- Rustel drops each value after the first 16, and each value with a name the
  plugin does not have. The note keeps its plugin.
- Studio refuses an update with a wrong parameter name of a loaded plugin.
  The last score plays on, and Studio marks the name and gives the nearest
  one. A preset name no file of the loaded plugin has is refused and marked
  the same way, read as the engine reads it, upper case, spaces and
  punctuation aside.

A plugin name with no plugin, a preset name with no file, or a dropped value
gives one warning for each evaluation. The warning shows in the Studio log,
and as a `warning:` line of `rustel play`. A muted lane and a lane a solo
drops give the warning of a plugin name or a preset name too.

## Presets

`preset` names a `.vstpreset` file in `~/.rustel/vst/<plugin name>/`. Save the
preset in a DAW, then copy the file to the folder.

```js
note("c2 eb2").vsti("serum 2", { preset: "Dark Bass" })
```

The scan makes a folder for each plugin it finds, so the folder names show
the exact plugin names. The preset name, as the plugin name, ignores upper
case, spaces and punctuation.

Any folder of `.vstpreset` files works too, in any structure: Settings ▸
VST ▸ `add preset folder` adds one. Each file names its plugin in its
header, so rustel finds the presets of a plugin in every folder you added,
and the plugin rows of the page say the count. Enter on a plugin row lists
its presets. A change of the folders reads them again at once, on the
plugin thread: the background-jobs sheet lists the read while it runs, and
no ask waits for it. The folders are watched while Studio runs: a preset
saved or copied in while a score plays is there with no rescan, once the
read of the folders ends, and the count line of the vst tab says where
that read is.

In the vst tab, Enter on a preset writes the call with the preset. With the
caret in a call of the same plugin, Enter on a preset changes only the
preset, and Enter on the row of that plugin goes to the rows under it. The
pick of a plugin name with Ctrl-F leaves the panel open in the presets of
the plugin, so the next Enter picks one.

The knobs column shows the presets of the plugin call under its rails, as
a section closed until → opens it. Enter on a name, and a click on one,
write the preset into the call, and the name the call holds carries the
mark. ← closes the section, and the j k h l of Vim list keys work there
as the arrows do.

A plugin call with no `preset` plays `default.vstpreset` when the folder of
the plugin has one. Save the start sound of a plugin there, and each call
starts with it:

```js
note("c2 eb2").vsti("serum 2")
```

In Studio, drop a `.vstpreset` file on the window. Rustel reads the plugin
class in the file and copies the file to the folder of that plugin. The
status line says which plugin. A file with the name of a preset already in
the folder is not copied. The scan cache of an earlier version has no plugin
classes. After an upgrade, rescan once so a dropped file finds its plugin.

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

Rustel runs 64 chains at most. A Csound instrument takes one of them for each
orbit it plays on, and a plugin chain never takes its place. When the 64 are
in use, a new chain takes the place of a chain in its last 5 seconds, such as
the chain of a lane you removed: see
[Plugin copies and edits](#plugin-copies-and-edits). With no such chain, the
notes of the new chain play with no plugin. An effect note then plays dry,
and an instrument note is silent. A Csound note with a `.vst()` effect is
silent too. The lane of the new chain gives one warning for each
evaluation, with its label and its line:
`the lane 'bass' at line 4 plays with no plugin: 64 chains play at most`.

A chain that ends plays its tail in the output. The output has room for 64
chains and tails. A new chain that finds no room cuts the oldest tail with a
fade of 10 ms to make room, and gives the warning
`the output had no room for a plugin chain: 64 chains play at most`.

## Plugin copies and edits

Each chain has its own copy of each of its plugins. An edit keeps the copies
of each chain the edit does not change:

- A new value of a parameter or of `.orbit()`.
- A lane you add, remove or mute above the lane.
- A plugin call you add or remove above the chain.

A lane is a labeled statement, such as `$:` or `lead:`, or a score with no
label. A lane stays the same lane when you move it, mute it or give it a new
label. A lane you edit stays the same lane in one of these cases:

- It keeps its label. The label `$` does not count.
- It keeps a plugin name.
- Its start and its end together keep half of its text or more.

The text counts more than the label. When one edit copies `lead:` to
`lead2:` and gives `lead:` a new text, `lead2:` is the lane that played.
`lead:` is then a new lane.

A lane keeps its chain through an edit of its plugin calls when the calls
still have one plugin of the chain. An effect keeps its copy when its call
keeps the plugin and the preset, also in a new place in the call order. So
an edit loads only the plugins and presets it adds. An edit that removes an
effect, or puts the effects in a new order, loads nothing.

A chain changes at its first note after the edit. While the chain sounds,
an effect you add fades in, and an effect you remove fades out. A new
preset of an effect, or a new plugin in its place, crossfades with the copy
before. A new preset of the instrument, or a new instrument in a chain that
keeps an effect, fades the copy before out under its first note. Each fade
takes 5 ms. A silent chain changes with no fade.

The copy before ends after its fade, and so does the copy of an effect you
remove. On a lane with a note in the next 16 cycles but no note in 5 seconds
of play, such a copy ends 5 seconds of play after the edit, once the chain
is silent. An instrument you remove ends 5 seconds of play after the edit on
any lane, once it is silent. Time counts only while the score plays, so a
Stop ends no chain. An undo before a copy ends loads nothing, and an undo
after it loads the copy again.

A lane with no note in the next 16 cycles keeps its copies until it has such
a note, also the copy before a new preset. A plugin name the lane loses
still ends its copy 5 seconds of play after the edit. When that is the name
of its instrument, the whole chain ends then, and the next note of the lane
loads each plugin again.

An edit that keeps no plugin of the chain gives the lane a new chain. The
chain before ends 5 seconds of play after the lane loses the name of its
instrument, or the name of each of its plugins. So it stays with the lane
while another pattern of the lane names its plugins.

A plugin with no input and no sound for 30 seconds sleeps and uses no CPU.
The next note wakes the same copy, with no load. A lane keeps its chains
through a long rest.

A lane you remove, or put in a comment, keeps its chains for 5 seconds of
play. A lane that comes back in that time, with its text or its label, keeps
its copies. After that time, the copies play their tail, then end. The tail
has 30 seconds at most.

A muted lane (`_$:`) and a lane a solo (`S$:`) drops play no note, and keep
their plugin copies. Rustel loads the copies of such a lane while it is
silent, so the end of the mute or of the solo loads nothing.

Rustel reads the plugin names in the text of each lane. A plugin call you
put in a comment loses its name. A chain ends 5 seconds of play after its
lane loses the name of each of its plugins. A name that comes back in that
time keeps the chain. So does a note that still plays the plugin, as with a
name in a variable. A chain with a plugin the text of its lane does not name
ends after 10 minutes of play with no note. Such a plugin has its name in a
variable or a function, or comes with a pattern from a prebake or an import.

## Load time and memory

Rustel loads a plugin on a thread of its own when you evaluate a score. The
audio does not stop. Until the plugin is ready, an effect note plays dry and
an instrument note is silent.

While the score plays, Rustel loads the plugin copies of the next 16 cycles.
A lane with its first note later in the score has its copies before that
note.

An edit waits for the plugin copies its notes need. Rustel reads the notes of
the edit near the cycle in play, and loads each copy that is not ready. A
copy the edit keeps is ready, so an edit with no new copy does not wait.
While a copy loads, the last score plays on. When the copies are ready,
the edit plays. If no copy becomes ready in 20 seconds, the wait ends. The
edit then plays, and a note with no copy plays with no plugin until its copy
is ready.

Studio has two load modes under Settings ▸ General ▸ Playback. In each mode,
Studio evaluates the text of a launch on a cycle line during the countdown,
reads its notes, and loads its plugin copies. If this work still runs a short
time before the line, the launch takes the first line after the work. Until a
score runs a plugin call, a launch with no `vst` in its text loads its plugin
copies at its line. In the wait mode, a start waits for the plugin copies of
its first 2 cycles, and an edit waits as above. If a copy of a launch is not
ready at the line, the launch takes the first line after the copy is ready. In
the async mode, the score starts at once, an edit plays at once, and a launch
does not wait for its plugin copies.

`rustel play` waits for the plugin copies of the first 4 cycles at its start.
They load while the samples load. With `--watch`, a save with a plugin copy
that is not ready prints one `waiting:` line, and the last score plays on.
`Updated` follows when the save plays. In a replay of a session tape, no save
waits for its plugin copies.

Memory grows with the number of chains. The Studio memory viewer lists each
loaded plugin with its copies and its load time. The row of a plugin with a
latency above 0 also shows the largest latency of its copies since the plugin
loaded, as `latency 1024 samples`. The last copy of each preset at each
sample rate counts, also after the copy ends. `rustel vst` shows no latency,
because it loads no plugin copy. At the cleanup of unused
samples, Studio unloads a plugin that no chain of the score has and no open
tab names. The chains keep their plugins also after a Stop, so a plugin from
a prebake, an import or a name the score computes stays loaded. A plugin
call in a tab names its plugin, also with a part of the name. Another string
names a plugin only with its whole name, as in `const fx = 'ott'`.

At Stop, Studio keeps its output open with the plugin copies of the score. A
start that reuses that output loads no copy. On Linux, with no output chosen,
the next start after a Stop opens a new output to follow the default device,
and the copies of the score load again. If you open Piano mode or play a note
in it before that start, Piano mode opens the new output. When the stopped
output holds a copy of a plugin the cleanup unloads, the cleanup closes the
output first. In Piano mode, the cleanup keeps the output open with its
copies. The first cleanup after you leave Piano mode closes the output when
it holds a copy of a plugin the cleanup unloads.

## Where the audio runs

The plugin audio runs in the rustel process, as in most DAWs. Each plugin
gets 128 frames at most in one call. A plugin fault during a set ends rustel.
Save your set before you play a plugin you do not trust.

A light set runs each plugin on the audio thread, one after the other. A
heavy set spreads its plugin chains over plugin workers, as live FX
multiprocessing in a DAW. A chain runs its plugins in order, one at a time.
Each chain ends its block inside the audio buffer, so the plugin workers add
no latency. Rustel measures the time each buffer of the set
takes with all of its work on one thread. The chains spread when that time
passes half of the buffer on average, or 80% of one buffer. They go back to
the audio thread when the average falls below 35%. The 80% check also runs
inside a buffer, after each block of 128 frames, so a heavy start spreads
from its second block.

An output starts its plugin workers with its first copy of a plugin that has
its switch `audio thread only` off. By default their number is the number of
cores minus 2, so a machine with 2 cores or fewer gets none. Two hardware
threads of one core count as one core. A CPU with performance and efficiency
cores counts only its performance cores. On Linux, this holds for Intel and
Arm CPUs. On Windows and Linux, the plugin workers also run only on those
cores. An output has 8 plugin workers at most. Each plugin worker asks for a
real-time priority:

| System | Priority |
| --- | --- |
| Windows | The Pro Audio task of the multimedia class scheduler (MMCSS), at critical priority |
| macOS | The time-constraint policy, with the period of the audio buffer |
| Linux | `SCHED_FIFO`, which needs a real-time limit for your user |

A plugin worker that gets no real-time priority stops. With no plugin worker
left, each plugin runs on the audio thread. On Linux, the audio thread takes
the same priority once a plugin worker has it. A user in a group with an
`rtprio` limit of 10 or more in `/etc/security/limits.conf`, such as the
`audio` group on many systems, gets the priority.

When the audio thread waits for a plugin worker that the system did not run,
rustel gives one warning for each evaluation:
`a plugin worker ran late, and the audio waited for it`.

For a plugin that breaks on another thread, turn on its switch
`audio thread only`. In Studio, open Settings, go to the vst page, select the
row of the plugin and press Space. The row then ends in `audio thread only`,
and the copies of the plugin stay on the audio thread. The other plugins keep
the plugin workers. A copy that loads after the change follows the switch. A
copy in play keeps its thread until it loads again, for example when you
restart Studio.

Studio keeps the switches in `~/.rustel/rustel.json`, and `rustel play` reads
them too. With no Studio, write the whole names of the plugins in the file.
Upper case, spaces and punctuation do not count:

```json
{
  "plugin_audio_thread": ["Serum 2", "OTT"]
}
```

A file with a fault gives a warning. Each switch is then off.

| Variable | Effect |
| --- | --- |
| `RUSTEL_PLUGIN_THREADS=2` | Sets the number of plugin workers of each output. `0` turns them off. |
| `RUSTEL_PLUGIN_AUDIO_THREAD="Serum 2;OTT"` | Keeps the plugins it names on the audio thread, in place of the switches. |

`RUSTEL_PLUGIN_AUDIO_THREAD` is for a run with no Studio, for example in a
script. To measure the plugin workers on your machine, see
[Measure a plugin](vst-performance.md#compare-the-plugin-workers).

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
- An edit of the effects of a chain acts at the first note of the chain after
  the edit. Until that note, the tail of the chain plays through the effects
  before the edit.
- An edit that puts the effects of a chain in a new order changes their order
  at once, with no fade. In a chain with 4 effects, so does an edit that
  removes an effect and adds one at another place.
- A chain runs its plugins one at a time. A chain that needs more than one
  core for a block crackles.
- A plugin call longer than the rest of the audio buffer crackles the whole
  output. Rustel does not render a plugin ahead of the audio.
- An export runs its plugins on one thread.
- A plugin that hangs stops the audio.
- On Windows with the multimedia class scheduler service off, each plugin
  runs on the audio thread.
- A plugin with its multicore option on waits for its own threads. Rustel
  reads the plugin worker in that wait as one the system did not run. That
  worker then takes no chain for 1 second or more. Turn the multicore option
  of the plugin off, or turn on its switch `audio thread only`.
- VST2, AU and CLAP plugins are not supported.

A plugin is native code with the rights of your user. See
[Security](../SECURITY.md#plugins). To time a plugin chain, see
[Measure a plugin](vst-performance.md).
