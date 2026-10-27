# Native terminal studio

Studio is an editor, player, and recorder for scores. Start it with a
score or a set folder:

```sh
rustel studio
rustel studio song.strudel
rustel studio set/
```

With no path, Studio opens the last set. On first use, it creates a set. A new
path that ends in `.strudel` opens a starter score. Update writes the file and
plays it. Any other new path, such as `rustel studio set1`, creates a set folder
with a starter scene. File > Open set does the same with a path you type. The
parent folder must exist.

A path given to File > Open set can be a Strudel pattern export: a `.json`
object that holds one pattern of code per entry. Such a file is not a set on
disk, so Studio makes a set of it. The new set folder sits beside the others and
takes its name from the export file. One score is written per entry. A score
that carries a `@title` field takes it as its file name, an entry keyed by a
name takes the key, and the rest are numbered `pattern 1`, `pattern 2` and so on
in the order the patterns were made. The open-set prompt offers these exports
beside the sets, and File > Import Strudel patterns does the same import into
the set already open: one score file an entry lands in the set's folder, and a
name the set already holds is skipped and counted, never overwritten.

Type or paste a score, then press **Ctrl+S** or **F5**. Press **Ctrl+G** or **F8**
to stop. Editing does not change the playing score until you update it. If an
update fails, the last good score keeps playing. An update that changes nothing
leaves the file, and its changed date, alone, so a set you only opened and played
stays as it was.

Use one editor for each score file. Studio does not merge external edits with
unsaved edits. See [External changes](#external-changes) to follow saves from
another program.

## Shortcuts

`^` means Control on every platform, including macOS. `⇧` means Shift. `Alt`
means Option on macOS. Some terminals need an option to send Alt keys.

Enhanced terminals can send **Ctrl+Enter** for Update and **Ctrl+.** for Stop.
The **Ctrl+S / F5** and **Ctrl+G / F8** alternatives also work in legacy terminals.
Use these alternatives if the terminal or operating system takes a shortcut.

| Action | Shortcut |
| --- | --- |
| Menu bar | F1, then the menu letter; Esc closes it |
| Update (write the file and play it) | ^Enter; ^S or F5 |
| Stop | ^.; ^G or F8 |
| Undo / redo | ^Z / ^⇧Z or ^Y |
| Comment / uncomment | ^/ or ^7 |
| Find in the score | ^⇧Y; Ctrl+Shift+F2 where the terminal keeps it |
| Devices | ^P |
| Master volume | ^⇧↑ / ^⇧↓; Ctrl+F12 / Ctrl+F11 |
| Slider under the caret | Alt+↑ / Alt+↓ |
| Quick action at the caret | ^J |
| Knobs column | ^⇧K; View ▸ Knobs |
| Zen mode | ^K or F11 |
| Word wrap | ^U |
| Previous / next scene | ^[ / ^]; F6 / F7 |
| New scene / duplicate | ^N / F3 |
| Rename scene | ^R; Scene > Rename scene |
| Close scene | ^W |
| Record a take | ^⇧R or Shift+F11 |
| Record an input sample | ^H |
| Log | F9; Shift+F9 docks it |
| Mixer | F4 |
| Export | ^⇧X |
| Set panel | ^B |
| Open a set | ^⇧O |
| Theme | ^T |
| Settings | ^O |
| Learn a pad / forget it | ^L / ^⇧L |
| Values for the argument at the caret; the reference | ^F or Ctrl+Space |
| Docs for the function at the caret | ^D |
| Piano mode | F12 |
| Split / close the split | ^E |
| Focus the other pane | F10 or ^⇧E |
| Quit | ^Q twice |

Ctrl+Shift+Y opens a find bar for the score. It starts empty. Type a
regular expression; the bar selects the first match as you type. Enter moves
to the next match and keeps the bar open. Backspace on an empty bar leaves it
open. Esc or a click elsewhere closes it. Some terminals keep this chord for
themselves; Settings > Keybinds shows the fallback.

With Vim editor keys on, Ctrl+Shift+Y and Edit > Find open the Vim search
prompt. Enter searches once and closes it. `n` and `N` walk the matches.

Vim changes Ctrl+R and Ctrl+[ in the score editor. See [Settings](#settings)
for Vim controls.

In a split view, use **Ctrl+Shift+[ / Ctrl+Shift+]** to change scenes. F6 and F7
also work. Each pane has its own caret and scroll position. A scene shows in one
pane at a time. Each pane has its own colour: the first wears the theme's
accent, and the second wears another accent. The pane's tab and its title both
wear it, filled where the caret is and on a dim fill where it is not. Tabs that
are nowhere open stay muted. A click on the tab of a scene in the other pane
moves the caret to that pane, as F10 does. A split does not create a second
player.

F1 opens keyboard help when the menu bar is hidden. Menus show available
actions and shortcuts. Press Esc to return to the editor. To cancel the first
Quit key, press another key. File ▸ Quit exits directly. Normal quit saves edits.

### Selecting text

Hold Shift with a movement key to select. Shift+Home and Shift+End select to
the start and the end of the line. Shift+click moves the end of the selection
to the pointer.

Some terminals keep these keys and do not send them to Studio:

- Ghostty on Linux keeps Shift+Home, Shift+End, Shift+PageUp and
  Shift+PageDown to scroll its history. Add these lines to the Ghostty config
  to free them:

  ```text
  keybind = shift+home=unbind
  keybind = shift+end=unbind
  keybind = shift+page_up=unbind
  keybind = shift+page_down=unbind
  ```

- kitty keeps Shift+click for its own selection. Use Alt+click or Ctrl+click
  in Studio.

Keyboard help (F1) says when the terminal does not send Shift+Home and
Shift+End.

## Scenes

A scene is a `.strudel` score file. A set can have up to 16 open scenes.
Selecting a scene changes the editor; it does not start playback. Update the
selected scene to play it. The scene strip marks the playing scene with `▶`
and unsaved edits with `●`.

Alt with 1 to 9 selects the score that holds that number on the strip. It
works while the editor has the keyboard, so you can switch scores without the
set panel. The panel's rows show the numbers, and a number the strip does not
hold says so in the footer.

Use Ctrl+N for a new scene or F3 to duplicate the current scene. These actions
create files. Scene > Rename scene renames the scene and its file. Rename has
no default shortcut. You can assign one in Settings > Keybinds. Ctrl+W closes
the tab and saves its edits. It does not delete the file. If Studio cannot save the edits,
the tab stays open. Closing a scene also forgets its pad assignment and rewind
setting.

To launch a scene from a MIDI pad, select it, press Ctrl+L, and press the pad.
Ctrl+Shift+L forgets the assignment. Connect the controller before you press
Ctrl+L. Studio listens for pads on every MIDI input that Devices lists. You
do not need to select the controller there.
Pad launches use the current launch setting.

### Sets

A set folder contains scores, `rustel-set.json`, and a `sessions/` folder. The
JSON file stores set options, including open scenes, pad assignments, panes,
the local prebake, and the master limiter.

Use the File menu to create, open, or rename a set. Open Recent lists the last
eight sets. Changing sets saves edits. If Studio cannot save a score, the set
panel asks what to do. Enter retries the save. Del leaves the set without the
edits it could not save. Esc keeps the set open. The current music continues
until you update a score or stop it.

New sets go in `<rustel-config>/sets/` by default. Change the default folder in
Settings. This affects new sets; it does not move existing sets.

In file picker and artwork popups, drag or hold Shift with movement keys to
select text. Double-click selects the whole field. Copy, Cut, Paste, and
Select all use your configured shortcuts.
The selected field shows its Copy shortcut. A terminal's native Copy command
uses the terminal's own selection.

### The set panel

Press Ctrl+B. Use the arrows to select a score or session, then Enter to open
it. Press 1 to 9 to open the score that holds that number on the strip. Space
folds a section. Esc returns to the editor. Use `+` and `-` to change the
panel size.

The panel's foot shows the selected row's details under its own rule: the
name, its strip number and open state, the `@` fields its leading comments
carry, and when it was last saved. The block follows the selection with no
key of its own. A tape's row shows its recording time, and the sessions fold
its count. The hint rows at the panel's bottom name the open, strip and
filter keys.

Press `/` to filter the scores: type, and the panel hides the scores the query
does not answer. A query matches a name or any `@` field value. Enter leaves
the filter standing and moves to the first match. Esc restores the full list.
The sessions fold is never filtered. The box shows its caret while it types.
When the query matches nothing, the row says no match and blinks, so an empty
list cannot read as an empty folder. While a filter stands, Backspace takes the
box back up and deletes from the query. It never starts a delete of a file. In
vim mode the box rests in command mode: press `i` or `a` to type.

Delete asks for confirmation before it removes a file. Confirming also discards
its unsaved edits. Studio does not delete the last score or the active session
tape. Keep a separate backup of sets you want to preserve.

### Prebakes

A prebake runs setup code before scores. Open the global or local prebake from
Options > Global prebake or Options > Local prebake, or from Settings. Learn a
key for either on Settings > Keybinds. The global file is
`<rustel-config>/prebake.strudel`. The local prebake
is stored in the set. Studio runs the global code first, then the local code.

Update checks and saves the prebake, then applies it to the playing score. It
does not start playback when Studio is stopped. Use `globalThis` or `register`
to share helpers with scores. A top-level `const` is local to its setup run.
Sample loading is allowed in a prebake. Transport changes, live MIDI input,
and `initHydra` must stay in scores. A prebake may still define a helper that
wraps them, such as `window.bpm = b => setcpm(b/4)`: the call runs in the
score that calls the helper.

A write that replaces a name the engine owns, such as `window.note = …`, runs
and is kept. The footer names the write once, so you know what every score
after it means by that name.

### Script imports

A score or a prebake loads a helper script with
`await import('https://glossing.dev/scripts.js')`. Studio runs the script as
setup code before the text with the import.

Studio loads a script only from an origin you trust. The first update of such
a score stops with `https://glossing.dev is not trusted`. Press Ctrl+O, go to
the Permissions page, and press Enter on the origin: the refused update runs
again by itself, so the score loads with no second keypress.
Each origin reads `on` or `off`: Space flips a row in place, so you can take
a trust back without the origin leaving the page. Studio keeps the trusted
origins in `studio.json`. Press `d` on an origin to remove the origin.

### Native orchestras

The Permissions page also holds the `native orchestras` switch, which starts
on each run. Csound playback requires the library. Space turns the switch
off for the run, and a score that loads an orchestra while it is off stops
with `Csound is not permitted`. Press Enter on the switch and the refused
update runs again by itself. Studio does not save this switch. See
[Csound](csound.md) for what an orchestra can do.

Turning the switch off blocks later orchestra use. A playing orchestra
continues until you stop playback or accept another score.

The first update after you trust an origin fetches the script. The update
waits for the fetch, 8 seconds at most. Studio fetches each script one time
per launch, so an import needs the network. For a set you play offline, paste
the script into a prebake.

A score plays with the names from its own imports. Suppose two scripts each
define `.sc()` in a different way. A scene with the first import gets the
first `.sc()`, and a scene with the second import gets the second, in any play
order. A name from one script only stays available to every scene. An import
in a prebake shares all its names with every score in the set.

The script runs in the score sandbox, with the limits of a prebake. A helper
that needs a browser page does not work. [KabelSalat graphs](kabelsalat.md)
run through `K` or `worklet`; graph node names are local to the graph. Studio does not
check a score with an import for unknown function names: the script defines
names Studio does not read before the script runs. See
[Script imports](../SECURITY.md#script-imports) for the security rules.

## Checking and playback

Studio checks the source after a short typing pause. The first-error command,
Shift+F3, moves the caret to the first reported problem. Legacy terminals use
the fallback shown in the menu. Open the Log for the full message. A failed
update does not replace the last good score.

The readiness indicator shows whether sounds are ready, loading, or failed.
Click it to open the Log. It cannot predict every sound that code will select
at runtime.

The default playback load mode waits for the first two cycles of sounds. An
old score keeps playing while a new score loads. Stop cancels a pending update.
In asynchronous load mode, notes whose sounds are not ready can be skipped.
Change this option under Settings ▸ General ▸ Playback. The wait includes the
VST3 plugins of the score: see [plugins](plugins.md#load-time-and-memory).

During playback, Studio marks each mini-notation value while its note sounds.
In an `arrange` call, Studio also marks the cycle count of the section in
play, so you see your place in the song:

```js
arrange(
  [8, note("c2*4")],
  [4, note("c2*4, g3*8")]
)
```

The `8` stays marked for 8 cycles, then the `4` for 4 cycles. A section needs
the form `[cycles, pattern]` inside the call to get a mark. A section with no
sound has no mark.

### Launch timing

The launch setting selects immediate playback or the next beat, cycle, two
cycles, four cycles, or eight cycles. It applies to updates and pad launches.
Choose the setting before you launch the next scene.

To start the score at cycle zero, use Ctrl+Shift+S or Shift+F5. To make this the
scene default, enable Rewind on Play with Ctrl+Shift+U. The scene strip marks
this setting with `⟲`.

### Auto evaluate

While the set plays, an edit can apply itself. Turn auto evaluate on under
Settings ▸ Live coding. It is off by default: an update is a deliberate
press.

An edit applies only after you rest. The typing pause sets how long: 250 ms,
500 ms, 1 s, or 2 s. The default is 500 ms.

Only a clean edit applies. Text that fails the check is refused, the last
good score keeps playing, and the footer says why. An applied edit writes
the score's file and never flashes the pane: a flash confirms a keypress,
and nobody pressed anything.

The launch setting names the line an applied edit waits for, the same line
an update waits for. With the launch row off, the edit plays at once. Beat
or cycle holds it for the line the row names, and the header shows the
`» scene` countdown while it waits.

Auto evaluate works while the set plays. A stopped set stays stopped, so
Ctrl+S remains how playback starts. While one score plays, the pause applies
an edit of that score alone: an edit of another score of the set stays in
its file, and the evaluate key is the one that runs it.

### External changes

Another program can edit score files in the set. Choose watch external changes
in Settings ▸ Live coding. Off is the default.

`sync` watches the score in the active tab. It puts the file's text in the
editor and leaves the playing score alone.

`sync & eval` watches every `.strudel` file in the set folder and every open
score. When a file is saved, Studio opens its tab, or selects it if it is
already open, and evaluates the text as an update would. It plays even when the
set is stopped.

Studio checks each watched file's size and modification time ten times a second
and reads it after a change stays put for one check. A save made by Studio
itself is ignored.

A save from another program that changes the text replaces the open editor's
text, including edits that are not saved yet. Studio does not merge the two.

### Sync with score metadata

A score can name its own file. Sync with score metadata, under Settings ▸ Set,
is on by default. An update renames the score's file to its @title field:

```javascript
/*
  @title night drive
  @by you
*/
$: s("bd sd")
```

The characters a name may not hold turn into dashes, and both ends are
trimmed. A name another score or file already holds is not taken. The ribbon
says so, and the file keeps its name. A score with no @title is left
alone.

### Stopping

The first Stop lets envelopes and effect tails finish. The transport shows
STOPPING during this time. Press Stop again to cut the sound. An update during
a tail starts the new score.

## Devices and audio

Press Ctrl+P to select audio output, audio input, or MIDI devices. Choose
`silent` output to edit and run the transport without a sound card. Use
`rustel devices` outside Studio for detailed device information.

### Audio in

Audio input starts with no device selected. Select an input in Devices, or use:

```sh
rustel studio song.strudel --input Scarlett
```

Use `s("in")` for input channel zero or `s("in:1")` for channel one. With no
input selected, these sounds are silent. Use the mixer to adjust input gain.
Select no input again to close the input stream.

Input sounds use live audio. To reverse audio or select a fixed region with
`begin` and `end`, record a sample first. Do not expect a live input to behave
like a stored sample.

### Audio out latency

Open Settings ▸ General ▸ Audio out. The **audio out latency** row sets the
output buffer. Automatic lets the device choose. A smaller buffer reduces
latency but can cause crackles. A larger buffer gives the device more time.
The display shows frames, milliseconds, and the size the device accepted.

You can also start Studio with `--buffer-frames`, from 32 to 16384 frames.
Input buffering adds its own delay. Check the selected device and sample rate
when comparing latency between systems.

### Clock sync and external output

Press Ctrl+P and use Tab to go to the MIDI tab of Devices. The `clock in` and
`clock out` rows are below the ports. Press Left or Right on a row to select
a port. The `clock in` row offers only the ports with a ticked `in` box. The
`clock out` row offers only the ports with a ticked `out` box.

Incoming clock can control tempo, but does not start the score. While it is
active, its tempo takes precedence over the score tempo.

Scores can send MIDI, OSC, and serial output. Device errors appear in the Log.
Stop sends the relevant note-off messages. See [hardware](hardware.md) for
ports, controllers, permissions, and platform setup.

### Link sessions

Press Ctrl+P and use Tab to go to the `link` tab of Devices. It holds one
checkbox. Press Space to join the session on the local network, or to leave
it. The row then shows how many others are in it, and the tempo it runs.

The session is off at each start. Joining is a thing done on purpose, because
it puts this studio on a grid that other machines run. Start Studio with
`rustel studio song.strudel --link` to join before the first evaluation.

While the session is on, its tempo and its play and stop states are shared.
A session that starts playing starts this studio. A session that stops stops
it. When this studio starts or stops, the session is told the same. A new
tempo from an evaluation is given to the session, and a tempo change made
elsewhere is followed here.

The Link session and the MIDI clock in are two outside clocks. Only one of
them leads. Join the Link session, and the clock in is released.

## Reference

Two shortcuts open the reference panel for the caret position:

- **Ctrl+F** shows what you can write at the caret. In an empty argument that
  takes a list of values, such as `warpmode(`, it lists the values. In a
  string, it lists the names that fit, such as sounds, scales, chords, or
  colors. After a `.`, it lists functions. On a word, it searches the
  reference for that word. Elsewhere in a call, it opens the documentation of
  that function, as Ctrl+D does. Outside a call, it opens the reference to
  browse and search.
- **Ctrl+D** opens the documentation of the function at the caret. Inside an
  argument, this is the function that takes the argument. Inside a string, it
  lists the names that fit, as Ctrl+F does.

**Ctrl+Space** does the same as Ctrl+F in a terminal that sends it. macOS
keeps Ctrl+Space for input sources by default. The menus and Settings show
Ctrl+Space only where Studio expects it to arrive.

Use arrows to select an item. Enter uses the selected item as shown in the
footer. Depending on the view, it inserts text or copies an example. Esc goes
back or closes the panel. Search filters such as `tag:vis` and `tag:snippet`
find visualizers and reusable snippets.

A function with more than one name has one row. The row shows the name your
search text matches best, and a line under the row lists the other names after `also:`. Enter
writes the name the row shows: search `cutoff` and Enter writes `cutoff`,
search `lpf` and Enter writes `lpf`. When two names differ only in case, the
row shows the camelCase name. A capital in your search text selects the names
with this capital first: `cycleS` puts `cycleStruct` before `cyclesPer`.

Settings ▸ Reference has one switch for each of six kinds of entry. All six
are on by default. Turn one off to hide the kind from the reference and the
suggestions:

- **OSC / SuperDirt**: controls only SuperDirt or another OSC receiver reads,
  plus `osc()`, `oschost` and `oscport`.
- **serial**: `serial()`.
- **FM routing matrix**: `fmi11` to `fmi88`. The operator controls, such as
  `fmi`, `fmi2` and `fmh`, stay listed.
- **bind & join**: `bind`, `innerJoin`, `appLeft` and the rest of the family.
  You use them to write new pattern functions.
- **pattern internals**: `withHap`, `splitQueries` and other hap plumbing.
- **KabelSalat**: `K()`, `S()`, graph nodes and inputs such as `sFreq` and
  `sGate`. This setting does not change score playback.

A hidden kind stays in reach. `tag:osc`, `tag:serial`, `tag:fm_matrix`,
`tag:bind`, `tag:internals` and `tag:kabelsalat` list the kind anyway, and
Ctrl+D still opens the documentation.

In the panel, Tab goes to the next tab. The examples tab has parts, tracks,
sound design, and Hydra visuals. The generator tab makes new music from a
direction, such as Acid current. In both tabs, Space plays the music and `c`
copies the code. A copy does not change the score. Paste the code into the
score, then update the score.

The vst tab lists the VST3 plugins in the plugin folders. Right arrow loads a
plugin and opens its row onto two rows under it: its presets and its
parameters. Right arrow on one of the two makes that half the view of the
tab, with a trail on the search line - `Serum > presets` - and its own
search. Left arrow and Esc go back to the plugin list. In the parameters
view, right arrow on a group shows its parameters, and left arrow closes the
group first. A parameter row shows the name a score writes, the title and
the default.

In a `.vst()` or `.vsti()` call, Ctrl+F completes from the plugin host. The
first string lists the plugin names: a part of a name finds the plugin, and a
plugin of a known kind shows only for its own call. The object lists the
parameter keys of the plugin, each with its title and default, when the plugin
has loaded.
The string of `preset` lists the preset files of the plugin.
Type to search the plugins by name. In the view of a half, the search finds
its rows: parameters by name, title or group, presets by name. Enter on a
plugin writes
`.vst("name")` for an effect and `.vsti("name")` for an instrument at the
caret. Enter on a parameter writes the parameter with its default, and Enter
on a preset writes `preset: "name"`.

F12 enables Piano mode. Use it to enter notes from the keyboard. Leave Piano
mode before you use those keys to edit ordinary text.

## Samples

Open the Samples tab in Reference to browse banks. Right expands a bank and
Left folds it. Space starts or stops a preview. Enter inserts or copies the
selection; check the footer for the current action.

Alt+A turns on auto preview. The up and down arrows then play each row the
cursor lands on. It works on the Samples, Chords and Scales tabs. Studio keeps
the choice in `studio.json`, so it stays on for the next session. Press Alt+A
again to turn it off. The foot of the list names the shortcut beside the move
keys, and the volume row says `auto on` or `auto off`.

Drag and drop an audio file or folder into the terminal window running Studio
to import samples. You can also add folders and packs in Settings ▸ Samples.
Samples in the set folder are discovered automatically. Use File ▸ Copy
samples into the set when you need a portable set. A dropped `.vstpreset`
file goes to the preset folder of its plugin. See
[Plugins](plugins.md#presets).

The sample view supports alias changes and file deletion. Check the
confirmation before deleting a file. An alias must not replace an existing
bank name. See [local samples](local-samples.md) for folder names, bank aliases,
and stable sample order.

The disk cache stores downloaded files. It is separate from decoded samples
held in memory. Use the Samples settings to fetch imports, cache samples, or
clear the disk cache. Clearing the cache requires confirmation. A later use
can download the files again.

## Sliders

Use `slider(value, min, max, step)` with literal numbers:

```js
$: s("bd sd").gain(slider(0.5, 0, 1, 0.01))
```

Update the score to activate its controls. Drag a slider, scroll over it, or
press Alt+Up / Alt+Down with the caret on it. Keyboard changes use its step.
A live change updates the code and is recorded in the session tape.

A slider placed directly on `gain`, `lpf` or `lpq` also moves notes already
sounding. An exact move takes 5 ms. With slider smoothing on in Settings, a
mouse move takes 35 ms. With a lowpass envelope on the same note, such as
`lpenv` or `lpattack`, an `lpf` slider changes from the next note. A slider
that is the value of a plugin key moves the parameter value at once: see
[plugins](plugins.md#names-and-values). Any other control changes from the
next note.

Ctrl+J opens the quick action at the caret. On a number, it can create a slider.
On a slider, it can change or remove the control. These source edits require
an update before they change playback.

The sliders page in Settings can bind a MIDI knob or controller axis to a
slider. The keybinds page can bind a button to any studio action, Update and
Stop included. See [hardware](hardware.md) for setup. Terminals that report
pixel positions give finer mouse control; other terminals use cell positions.
A drag lands on a whole number when the cell or the pixel under the pointer
holds one that the slider's steps reach.

## Knobs

View ▸ Knobs or ^⇧K opens a column beside the score. It reads the call the
caret is in, and its top row names that call. Its numbers show as sliders:
each plain numeric argument, each `slider(...)` argument, and each numeric
entry of an options object. A plain argument goes by the name the reference
gives it, such as `frequency` for `lpf(800)`. A different call under the
caret moves the rails with it. The column stays while you edit. Esc returns
the keyboard to the score, and ^⇧K closes the column. A terminal that cannot
tell ^⇧K from ^K uses View ▸ Knobs.

Options objects belong to calls that take them, such as
`set({ cutoff: 800, resonance: 5 })` and `vst("Name", { mix: 0.5 })`. An
audio control such as `lpf` takes a number: `lpf(800)` has a rail, and
`lpf({ cutoff: 800 })` has none, because the engine refuses that note.

A rail writes the score in place as you move it. While the set plays and
auto evaluate is on, the change sounds at once, the way a score's own
slider sounds. A stopped set, or auto evaluate off, keeps the typing pause.
If the score already holds a `slider(value, min, max, step)`, the rail moves
its value argument and keeps the call. The score is the master. The column
only edits what is written.

A number with no `slider(...)` gets bounds the studio works out:

- A plugin parameter travels 0 to 1, the numbers a `vst` call takes.
- A function or a control travels the numbers its reference gives it. A
  count moves in whole numbers: `seg(16)`, `ply(3)`, `chop(8)` and
  `euclid(3, 8)` never land between two. A speed such as `fast(1.5)` moves
  in quarters, a place in the cycle such as `off(0.125)` in sixteenths, a
  chance such as `degradeBy(0.3)` in hundredths, and `lpf` from 20 to 20000
  Hz on a log scale. A number past the travel, such as `seg(128)`, widens
  it.
- Any other number up to 1 travels 0 to 1. A larger number travels from 0
  to a round number at least twice its value, in whole steps when it is a
  whole number. A negative number travels the same way below zero.

The bounds stay put while the caret stays in the call, so a value pushed to
the top does not widen its rail. Each rail keeps its row too: an entry that
goes off, comes back or moves in the object does not push the others. Press
Enter to set your own bounds. The studio keeps them for that call name and
parameter.

← and → move a rail one notch, as the score's own sliders move. A notch is
one step on a rail of up to 200 steps. On a longer rail it is about a
hundredth of the travel, in 1, 2 or 5 steps times a power of ten, and on a
log rail a hundredth of the travel. A notch stops on its own grid, so a
value with a fraction, such as 16.3, comes back to 16.5 and 17. Shift+← and
Shift+→ move one step. A drag lands on a whole number when the cell or the
pixel under the pointer holds one that the rail's steps reach. It holds the
rail it started on: the pointer can leave the column, and the rail follows
it until you let go.

The far left of a rail is an off stop. On an object entry it takes the entry
out of the score and leaves its comments where they are. Backspace or
Delete does the same. The column's last two rows name the keys: the first
for the rail under the hand, the second for the call. The rail dims at the off stop, and a move right writes the entry back
as it was. If you edit the call while the entry is off, the entry comes back
at the front of the object. A plain numeric argument has no off stop,
because the call needs it. Home moves it to the bottom of its rail.

Press Enter to type an exact value, with min, max and step in the same form. A
value below min or above max moves that bound to it. The form writes what the
score keeps: a plain number changes the literal, and on a
`slider(...)` argument it rewrites the call's own bounds.

On a `vst(...)` or `vsti(...)` call, press `a` for the plugin's parameter list.
It opens over the score beside the column. Type to search, and press Enter or
click a name to add it as a rail at its default value. The wheel and the
trackpad scroll the list. Plugin values show and write three decimals at
most. The choice is kept in the studio's settings, so the same plugin shows
the same rails in every set. Delete in the list forgets a kept choice, and
Delete on a kept rail that is off takes it off the column.

Before you choose, a plugin call shows up to 10 suggested rails, off until
you move one. A host cannot see a plugin's window, so the studio ranks what
the plugin tells it: the dry/wet mix first when the plugin names one, then
the continuous parameters in the plugin's own order, then its switches and
lists, which are mostly modes and settings. A bypass, a program change and
the MIDI controller stand-ins some plugins add are left out. The first rail
you add or take off starts your own list from the suggestions you kept.
Press `r` twice to drop your list and get the suggestions back. Any other key
between the two keeps your list. Values in the score stay as they are.

A call whose options the reference documents, such as `_pianoroll`,
`scope`, `spectrum`, `lfo` and `env`, shows up to 10 of its documented keys
as rails, in the order of the reference. A key the score does not hold is
off until you move it. It comes on at the value the call already uses, so
the sound and the picture stay the same until the next move. For a painter,
that is the value the terminal uses. A key with no such value, such as
`sync` of `lfo`, is in the `a` list only, and it comes on at the middle of
its travel. A key written by its alias, such as `a` for the `attack` of
`env`, or in another case, is that key. Press `a` for the full list. Enter
or a click adds a key, and a call with nothing in it, such as
`_pianoroll()`, gets the object. A key the score already holds stays as it
is. A key that takes text, such as a colour, a key the terminal ignores,
and the FX stage index `fxi` have no rail.

## Mixer and limiter

Press F4 to open the mixer. Change master volume with Ctrl+Shift+Up / Down.
If the terminal takes those shortcuts, Ctrl+F11 (the left key) lowers the
volume and Ctrl+F12 raises it. If the terminal or the desktop also takes one of
those keys, Studio uses a spare function key; the keyboard help shows it. You
can also drag or scroll the master meter. This changes output volume without
editing the score.

`orbit(n)` sends a track to an effects bus. In the mixer, choose the output pair
for each orbit. If that pair is unavailable, it falls back to the main pair.
A take records the main pair; it is not a recording of all separate outputs.

To protect the master output, use Transport ▸ Add limiter to set. In the
limiter's mixer strip,
change the ceiling with arrows, use `c` to change mode, and use `b` or Enter to
bypass it. Backspace removes it. The set remembers this limiter. The default
limiter setting affects new or unconfigured sets. A score's `.limit(...)` is a
separate effect.

Audio settings on the General page also set the maximum polyphony. More voices can use
more CPU. A score can override the limit with `setMaxPolyphony(...)`.

## Visuals

The View menu opens the two visuals docks. Use `e` to change a dock's edge and
`+` or `-` to resize it. Use its controls to choose a scope, spectrum, events,
mixer, or another visual. Follow the footer for actions in the selected view.

Scores can also contain inline visualizers. Use the visualizers switch in
Settings, or View > Inline visualizers, to show or hide them. Bind a key for
Inline visualizers on the Settings > Keybinds page. Visual settings control
their appearance and opacity. See [Hydra](hydra.md) for shader visuals and
backgrounds. Visuals stop advancing when playback stops.

## Themes

Press Ctrl+T to choose a theme. Type to search, use arrows to select, and press
Enter to keep the selection. Ctrl+N creates a user theme. Ctrl+E edits one; an
edit to a built-in theme creates a user copy. Ctrl+D asks to delete a user theme.

User themes are JSON files in `<rustel-config>/themes/`. `--theme` accepts a
theme name or a path to a theme file, such as `--theme ./midnight.json`. You
can list themes or select one at startup:

```sh
rustel studio --list-themes
rustel studio --theme strudel song.strudel
```

### Minimal user theme

Save this example as `<rustel-config>/themes/my-theme.json`, then select it in
the theme picker:

```json
{
  "background": "#121419",
  "surface": "#181b22",
  "overlay": "#1f2430",
  "foreground": "#e2e7ef",
  "muted": "#5b6371",
  "rule": "#303640",
  "accent": "#55d6e8",
  "ok": "#70d98b",
  "warn": "#ffca28",
  "error": "#ff6b6b",
  "selection": "#37465e",
  "selection_text": "#ffffff",
  "mini": "#70d98b",
  "event": "#ffca28",
  "event_inactive": "#7491d2",
  "playhead": "#ffca28",
  "grid": "#373d46",
  "minimap": "#15181d",
  "minimap_viewport": "#2a3140",
  "syntax": {
    "text": "#e2e7ef",
    "comment": "#5b6371",
    "string": "#70d98b",
    "number": "#ffca28",
    "punctuation": "#d986ff"
  },
  "meter": {
    "low": "#70d98b",
    "mid": "#c8e06a",
    "high": "#ffca28",
    "peak": "#ff6b6b",
    "track": "#242833",
    "fader": "#e2e7ef"
  }
}
```

Themes can also use shader effects. A theme cannot grant camera access.
Webcam input is off by default and needs explicit consent in Settings.

Every colour also accepts `reset` for the terminal default and `ansiN` for
palette slot N, so a theme can wear the palette the terminal already carries.
The built-in `terminal` theme is exactly that: a studio that looks like the
rest of your terminal, Kitty scheme or Noctalia template included. It takes
the bright slot 12 for its accent. Its sibling, `terminal-warm`, moves the
accent and the selection to slot 1, so a palette whose bright slot reads badly
as an accent has another to wear. Two things follow from a colour the studio
cannot measure. A theme that keeps `reset` as its ground should set
`event_mark` to `underline`, as `terminal` does, because a tint has no ground
to tint. And the filled chips write the palette's black, slot 0, so they read
best on a palette whose black is dark. A painter that fades a colour toward
such a ground keeps the colour whole, since a fade with nothing to blend into
would otherwise land on the ground itself.

## Settings

Press Ctrl+O. Use Tab to change pages, arrows to select or change values, and
Space to toggle a switch. The General page covers playback, live coding and
the set. The other pages cover samples, controller mapping, key bindings, the
kinds of entry the reference lists, and the origins trusted for
[script imports](#script-imports).
The About page reports terminal capabilities.

The Appearance page holds everything visual: the chrome, the editor's marks,
where the panels dock, and the drawing tier.

The **beat line** row in the Look group adds a thin strip under the header
that keeps the tempo visible from across the room. It follows the header's
own beat clock and the theme's colours, and `←/→` walks its looks: `pulse`
breathes on each beat, brightest on the downbeat, `travel` sends a bright
segment across once per cycle, and `wave` rolls a swell along the line. It
rests as a dim flat line while nothing plays.

The `evaluation animation` row, under `flashing on evaluation` in the Editor
group, shapes the flash a run of code makes when you evaluate it. `normal`
swaps the lines in and out at once. `fade fast`, `fade medium` and `fade
slow` fade the swap in and out. `swipe` fades the flash away behind a soft
edge that sweeps left to right across each line, `zap` hits hard and
evaporates, `steps` dims through four chunky rungs, `glitch` shatters lines
into fast bands of blocks and flips, and `wave` sends a band of light along
each line. The row above says which evaluations flash at all.

The Terminal row at the top of the Keybinds page selects the list of shortcuts
that Studio avoids because the terminal takes them. Keep it on automatic unless
Studio detects the wrong terminal. If you select another terminal, the row
shows both names, for example `Windows Terminal (this terminal: kitty)`. The
status line says so too, and the Log has a warning when Studio starts with
such a selection. Studio then does not avoid the shortcuts that your terminal
takes, so some of them can stop working. Press Del on the row to return to
automatic.

To change a shortcut, select its row, press Enter, then press the new chord.
Del restores the default. The panel shortcuts Alt+O, Alt+R, Alt+D and Alt+T
are at the end of the list. The panel footers show the chord you set.

In iTerm2, Option set to Normal composes a character. Studio knows the
characters the profile sends and recovers the Option panel shortcut where
the focused surface uses it. Option+T then focuses the tape timeline, and
the composed character types normally everywhere else. Set Option to Esc+
if you want the raw Alt key instead.

The vst page lists the folders the plugin host reads for VST3 plugins: your
folders first, then the standard folders of the system. Press `a` to add a
folder and `d` to remove one of yours. Enter on `scan` reads every folder and
tests each new or changed plugin, for example after you install a plugin.
Enter on `rescan` empties the scan cache and tests each plugin again. While a
scan runs, the `scan` row names the plugin in its test. The page shows the number of plugins found and the preset
folder, `<rustel-config>/vst`, with one folder of `.vstpreset` files for each
plugin. A row for each plugin ends the page, with its state, and with its
fault when it failed. Space on a plugin row turns its switch
`audio thread only` on or off, for a plugin that breaks on another thread.
The row then ends in `audio thread only`. Before the load or the test of its
plugin, a row has the name of its bundle and takes no switch. A plugin copy
that loads after the change follows the switch. Studio keeps the switches in
`<rustel-config>/rustel.json`, so `rustel play` follows them too. See
[plugins](plugins.md#where-the-audio-runs).

Studio saves preferences in `<rustel-config>/studio.json`. The default config
folder is `~/.rustel`, including `%USERPROFILE%\.rustel` on Windows.
`RUSTEL_CONFIG_DIR` selects another folder. Zen mode lasts for the current
session only.

Use Automatic rendering unless you need a specific mode. Available modes
depend on terminal glyphs, colors, image support, and reported dimensions.
An unsupported choice falls back to Automatic. Image rendering can cost more
than text cells. Reduce the frame rate in Appearance settings if visuals use
too much CPU. Check the Log for device or render errors.

### Vim editor keys

The Editor keybindings row selects Default or Vim. Default keeps the usual
editor controls. Vim starts in Normal mode. Press `i` to insert text and Esc,
Ctrl+[ or Ctrl+C to return to Normal mode. The status line shows the mode
beside warnings and errors. Vim uses a steady block in Normal and Visual and
a steady bar in Insert. The caret shape setting applies outside Vim. Studio
saves this choice in `studio.json`.

Vim supports `hjkl`, word motions (`w`, `b`, `e`), `0`, `^`, `$`, `gg`, and `G`.
Use `dd` to delete a line or `:42` then Enter to go to line 42. Counts work
with motions and edits, such as `3j` and `2dd`. Use `a`, `I`, `A`, `o`, and `O`
to enter Insert mode, `d`, `c`, and `y` with a motion to delete, change, or
copy text, and `p` or `P` to put it back. A yank or delete fills the
register and the system clipboard, so `p` pastes the newest copy from any
surface. `r` then a character replaces the character under the cursor, and
a count replaces that many. `D` deletes to the end of the line.
`C` changes that text. `v` selects characters and `V` selects lines.
A selection made with the mouse or with Shift+arrows enters Visual. `d`,
`c`, `y`, `x`, `X`, `D`, `C`, `Y`, `p`, and `P` act on it. Backspace or
Delete deletes it, and `r` then a character replaces every selected
character. Esc returns to Normal. `u` undoes an edit. Ctrl+R in Normal or Visual mode redoes it.
`:redo` then Enter also redoes an edit. Ctrl+D and Ctrl+U move half a page
down and up. A count moves that many lines.

`/` searches forward and `?` backward. Edit > Find opens `/`. Type a regular
expression and press Enter. `n` repeats the search and `N` repeats it in the
other direction; both take counts, such as `2n`. A count on the search also
works: `3/foo` lands on the third match. An empty pattern repeats the last
search. The search wraps and is case-sensitive. It uses the Rust regex syntax
in [regex](https://docs.rs/regex/latest/regex/#syntax), so `\$` finds a literal
dollar sign and `\d+` finds digits. Backreferences and look-around are not
supported. Zero-width matches are skipped.

Text objects select or edit text between separators. `vi(` selects inside
parentheses, and `vi"` selects inside double quotes. Use `a` instead of `i`
to include the separators, such as `va(`. These objects also work with `d`,
`c`, and `y`, such as `ci"` to replace a string. Supported separators are
`()`, `[]`, `{}`, `<>`, single quotes, double quotes, and backticks.

Vim keys take priority while the editor has focus. Unclaimed keys keep their
Studio action. A pending Vim command or mapping owns the next key. While Vim
keys are on, Docs answers to `gd` through a default mapping, and Wrap has no
default chord. Ctrl+D and Ctrl+U page instead. Settings > Keybinds shows
these Vim defaults. A vimrc line for `gd` replaces the Docs mapping. A learnt
chord replaces the withdrawn table chords, and a learnt chord on Ctrl+D or
Ctrl+U wins over the page motion. Settings, prompts, and other panels keep
their usual controls, except the list keys below. Menus show the shortcuts
available in the current context. In Insert mode, Ctrl+R is reserved for
register entry, which is not supported. Pasted text stays literal. On
terminals without bracketed paste, enter Insert mode before pasting.

The Vim list keys row in the Vim section of Settings > Keybinds is on by
default. Plain `j`, `k`, `h`, and `l` move lists, menus, and help as the
arrows do. Ctrl+D and Ctrl+U page every list panel. A panel command keeps
its meaning. The other menu letters do too. While the row is on, these four
letters navigate menus even when they spell a menu mnemonic. Turn the row
off to keep them for text fields and menu mnemonics. The arrows always work.

A search box in the reference column starts in Command mode while the list
keys are on. `j`, `k`, `h`, and `l` walk the list, and the other letters stay
out of the search. Press `i` or `a` to type in the search, then Esc to return
to Command. Space still auditions the selected row. An anchored completion
starts in Insert mode.

The theme picker and the export sheet use the same modes. `j` and `k` move
the list or the form, `i` or `a` type, and Esc returns to Command. On the
export sheet, `h` and `l` change a choice or a number, and Space toggles a
choice. In Command mode, `p` pastes the clipboard into the search or form
and enters Insert. Insert mappings from your vimrc and the shipped defaults
apply in these fields too. The shipped `jk` mapping leaves Insert in the
search.

### Custom Vim mappings

The first time you enable Vim, Studio creates `~/.rustel/vimrc` if it does not
exist. `RUSTEL_CONFIG_DIR` changes that folder. Built-in bindings remain active
until a map line overrides them. Studio also ships default mappings beneath
your file. Your file overrides them line for line. The shipped mappings
include `U` update, `Q` stop, `J` and `K` for `$` labels, `M` for `_`, `jk`
to leave Insert, `gcl` and `gc` to comment, and `gf` for the Reference. The
shipped `<Space>` chords are `V` visualizers, `O` set panel, `H` and `L`
previous and next scene, `P` local prebake, `I` panels, and `1` and `2`
visual set one and two.

Edit the file in your own editor. The Vim mappings path row appears below
Editor keybindings when Vim is enabled. Press Enter to set a full file path,
or Del to restore the default path. Studio saves the path in `studio.json`
and keeps it when you switch between Default and Vim. The file can be a
dedicated Vimscript file or a copy of your Vim or Neovim mappings. The file
name and extension do not matter; Studio reads map lines from the file.

For example, this file searches the score with `J` and `K`, types an
underscore with `M`, toggles the inline visualizers with Space V, and leaves
Insert mode with `jk`:

```vim
nnoremap U <Action:update>
nnoremap Q <Action:stop>
nnoremap <Space>V <Action:visualizers>
nnoremap J /\$<CR>
nnoremap K ?\$<CR>
nnoremap M i_<C-c>l
nnoremap X hx
inoremap jk <Esc>
```

Studio reads these map commands: `noremap`, `map`, `nnoremap`, `inoremap`,
`vnoremap`, `xnoremap`, and `onoremap`. `noremap` and `map` apply to Normal,
Visual, and Operator-pending mode. Before the left-hand side, Studio ignores
the flags `<silent>`, `<nowait>`, `<buffer>`, `<unique>`, `<special>`, and
`<script>`.

Each map takes a key sequence and a replacement. Replacement keys use the
built-in commands and do not expand other mappings. Use uppercase letters for
Shift, `<C-r>` for Ctrl+R, `<A-x>` for Alt+X, and `<S-Tab>` for Shift+Tab.
Named keys include `<Esc>`, `<CR>`, `<Enter>`, `<Tab>`, `<BS>`, `<Del>`,
`<Space>`, `<lt>`, arrow keys, `<Home>`, `<End>`, `<PageUp>`, `<PageDown>`,
and `<F1>` through `<F24>`. Use `<lt>` for a literal `<`. `<Nop>` disables
the input sequence in that mode. Your terminal must send the keys to Studio
for a mapping to work.

Studio skips a line it cannot use: Ex commands (`:...`), plugin commands,
function callbacks, `<leader>`, `<Plug>`, and unknown key notation. Write
your actual leader key, such as `<Space>V`, instead of `<leader>V`.

A replacement can call a Studio action: `<Action:update>`, `<Action:stop>`,
`<Action:visualizers>`, `<Action:find>`, or a label from the Keybinds page
such as `<Action:toggle inline visualizers>`. The action follows its binding,
so a rebind moves the mapping with it. Pointing the map at the action's key
also works, but it stops following if the key changes. An unknown action name
skips the line.

The stable names for `<Action:...>` are:

- Transport: `update`, `stop`, `rewind-update`
- Menus and surfaces: `menu-bar`, `help`, `settings`, `global-prebake`,
  `local-prebake`, `devices`, `theme`,
  `reference`, `docs`, `piano-mode`, `set-panel`, `mixer`, `visuals-one`,
  `visuals-two`, `visualizers`, `log`, `log-sticky`, `focus-panels`, `jobs`,
  `memory`, `export`, `smart-action`, `split`, `hop-pane`
- Editor: `undo`, `redo`, `copy`, `cut`, `paste`, `select-all`, `comment`,
  `find`, `first-error`
- Scenes: `new-scene`, `duplicate-scene`, `record-take`, `record-sample`,
  `rename-scene`, `close-scene`, `learn-pad`, `forget-pad`, `scene-rewind`,
  `previous-scene`, `next-scene`
- View: `word-wrap`, `zen-mode`, `master-up`, `master-down`, `open-set`, `quit`
- Panels: `show-file`, `rename-file`, `delete-sample`, `trim-sample`,
  `focus-timeline`

Names are case-insensitive. The label shown on the Keybinds page also works.

Studio does not run the file as Vimscript; it only reads map lines.
Conditions and function bodies are not evaluated: a map line inside `if` or
`function` is read the same way. Comments start with `"` on their own line.

Studio reloads external edits automatically between commands. An incomplete
key sequence waits up to one second, then uses the matching shorter mapping
or the original keys. A missing file uses built-in bindings. Studio never
overwrites an existing mapping file.

This is a native subset of Vim. It does not load plugins or run Vimscript, and
named registers are not supported.

### On a bare Linux console

The Linux console has limited fonts and colors. Use the legacy shortcuts in
the table above. For a graphical terminal on a local console, one option is:

```sh
sudo apt install cage foot fonts-dejavu-core
cage -- foot -f "DejaVu Sans Mono:size=11" rustel studio song.strudel
```

Run this from the local console, not an SSH session. The compositor needs
access to the seat and video device. Check the distribution's seat setup if it
cannot start. Keep the plain console available when testing a new setup.

## Remote control

Remote control is off by default. Enable it in Options ▸ Remote control or
start Studio with:

```sh
rustel studio --remote-control
```

The default address is `127.0.0.1:9247`. Studio generates a new token each time
you enable remote control, unless you set a token of your own. Use the
remote-control panel to reveal or copy it. A client must authenticate before
it can send keys or request the screen. An authenticated client has full
control of the Studio interface. Keys from a client use normal editing,
even when Vim editor keys are on.

Studio also writes each generated token to a file named
`rustel-studio-remote-<random>.token` in the system temporary directory. The
file contains only the token. File permissions give access only to your user
account. Studio deletes the file when remote control stops. If the Studio
process ends abnormally, the file can stay on disk. A token that you set with
`--auth-token` or in the panel has no file.

The connection is plain text. Keep it on localhost, or use an SSH tunnel for a
remote connection. Run `rustel studio --help` for listener options. See
[Studio remote control](../SECURITY.md#studio-remote-control) for what the
token grants and the limits of the listener.

## Sessions and replay

A session tape records installed scores, their timing, slider changes, and
stops. The first successful update creates a tape in the set's `sessions/`
folder. Replay does not create another tape.

Use `--no-save-session` to disable tapes. `--save-session debug` also records
rejected updates. `--session-file` selects a file explicitly.

Open a session from the set panel. Alt+T focuses its timeline. Select a block
with arrows and press Enter to play it. Ctrl+S updates an edited block.
Ctrl+W returns to the set.

Use `T` to change a block's duration. Tab switches between seconds and cycles.
Delete asks before it removes a block. You cannot rewrite the active live tape
or a debug tape. Use File ▸ New session to finish the current tape first. This
starts a new tape without interrupting the playing score.

## Recording

### Takes

Press Ctrl+Shift+R or Shift+F11 to record a take. Press it again to finish. A
take is a 24-bit stereo WAV of the final master mix at the device sample rate.
Studio saves it in the recordings folder. The default folder is
`<rustel-config>/recordings/`. Use the **recordings folder** row in Settings
to change it. With the default folder, a finished take also appears in the
`recordings` bank.

Recording includes the silence between Stop and the next update. A sample-rate
change or a disk error ends the take and adds a Log message. Normal quit closes
the file. Use Transport ▸ Reveal last export or take to find the output file.

Press Ctrl+H to record an input sample. Press it again to finish. The sample
appears in the recordings bank. It uses the selected input, or the system
default when none is selected. You cannot record an input sample and a take
at the same time.

## Export

Press Ctrl+Shift+X to render the focused score offline. Export uses the text
currently in the editor, including unsaved changes. The global and local
prebakes run first, as they do before a score in play, so a name a setup
defines is there for the bounce. The bounce also uses the same samples as
play: the folders and packs in Settings ▸ Samples, the audio in the set
folder, and your bank renames. Live playback continues.
If the export cannot find or load a sound, or the engine refuses a note, the
export fails and names the refusal. The file is still written. The check covers
every note the export schedules, also notes after an early end.
Use Tab and arrows to choose the length, WAV or MP3 format, stems, and output
path.

Choose a fixed number of cycles or **until silence**. Until-silence export stops
when audio stays below the threshold for the hold time. The defaults are
**−60 dB for 2 seconds**, with a **10-minute maximum**. Adjust the threshold
and hold time for quiet passages or long effect tails.

Studio exports at 48 kHz. The default output folder is `exports/` in the set
folder. When an export has an absolute output path, Studio remembers its
folder. The export dialog then starts in that folder, in every set, while the
folder exists.

The export limiter starts with the current mixer setting. Changing it in
the export dialog affects that export only. CLI exports do not inherit the set
limiter. Match limiter, duration, sample rate, and voice limit when comparing
an export with another render.

Turn stems on to write a `<name> stems` folder beside the take instead of the
one file. It holds `main.wav` with the same bounce, plus one stereo WAV for
each orbit that sounds, named `1.wav` upward. Each stem is tapped after its
orbit's delay, reverb, filter, duck and fader, before the master limiter, so
the stems sum back to an unmastered main and mastering stays in the DAW. Stems
need the WAV format: turning them on takes the format back to WAV, and MP3
turns them off. The status names the folder, because the take lands inside it.

Press Ctrl+Shift+X again to end an export early. Studio adds a fade and keeps
the audio already rendered. Use Transport ▸ Reveal last export or take to
locate it.
See [CLI export](cli.md#export) for command-line options.

## Log

Press F9 to open the Log, or Shift+F9 to dock it. Esc returns to the editor.
End follows the newest entry. Press `v` to show verbose entries. The file
always includes them.

The log file is `<rustel-config>/sessions/studio.log` unless
`RUSTEL_SESSION_DIR` selects another folder. A record looks like this:

`2026-08-25T18:04:11.482Z pid 1234 warn  [check] refused - line 2: unknown sound "abcde"`

Read the source and line number before retrying a failed update. The header's
warning count shows what still stands: a warning whose condition clears, such
as a score that checks clean again, leaves the count by itself, and a warning
nothing clears leaves it after ten seconds. Opening the Log acknowledges the
count right away. Resolved alerts remain visible as history.
For device failures, check Devices and the [hardware guide](hardware.md).
