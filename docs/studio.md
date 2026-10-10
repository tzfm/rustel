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

Type or paste a score, then press **Ctrl+S** or **F5**. Press **Ctrl+G** or **F8**
to stop. Editing does not change the playing score until you update it. If an
update fails, the last good score keeps playing.

Use one editor for each score file. Studio does not detect external file
changes or merge them with unsaved edits.

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
| Devices | ^P |
| Master volume | ^⇧↑ / ^⇧↓; Ctrl+F12 / Ctrl+F11 |
| Slider under the caret | Alt+↑ / Alt+↓ |
| Smart action at the caret | ^J |
| Zen mode | ^K or F11 |
| Word wrap | ^U |
| Previous / next scene | ^[ / ^]; F6 / F7 |
| New scene / duplicate | ^N / F3 |
| Rename / close scene | ^R / ^W |
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

In a split view, use **Ctrl+Shift+[ / Ctrl+Shift+]** to change scenes. F6 and F7
also work. Each pane has its own caret and scroll position. Selecting a scene
already in the other pane moves focus there. A split does not create a second
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

Use Ctrl+N for a new scene or F3 to duplicate the current scene. These actions
create files. Ctrl+R renames the scene and its file. Ctrl+W closes the tab and
saves its edits. It does not delete the file. If Studio cannot save the edits,
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
stays open. The current music continues until you update a score or stop it.

New sets go in `<rustel-config>/sets/` by default. Change the default folder in
Settings. This affects new sets; it does not move existing sets.

### The set panel

Press Ctrl+B. Use the arrows to select a score or session, then Enter to open
it. Space folds a section. Esc returns to the editor. Use `+` and `-` to change
the panel size.

Delete asks for confirmation before it removes a file. Confirming also discards
its unsaved edits. Studio does not delete the last score or the active session
tape. Keep a separate backup of sets you want to preserve.

### Prebakes

A prebake runs setup code before scores. Open the global or local prebake from
Settings. The global file is `<rustel-config>/prebake.strudel`. The local prebake
is stored in the set. Studio runs the global code first, then the local code.

Update checks and saves the prebake, then applies it to the playing score. It
does not start playback when Studio is stopped. Use `globalThis` or `register`
to share helpers with scores. A top-level `const` is local to its setup run.
Sample loading is allowed in a prebake. Transport changes, live MIDI input,
and `initHydra` must stay in scores.

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
Change this option under Settings ▸ Advanced ▸ Playback.

### Launch timing

The launch setting selects immediate playback or the next beat, cycle, two
cycles, four cycles, or eight cycles. It applies to updates and pad launches.
Choose the setting before you launch the next scene.

To start the score at cycle zero, use Ctrl+Shift+S or Shift+F5. To make this the
scene default, enable Rewind on Play with Ctrl+Shift+U. The scene strip marks
this setting with `⟲`.

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

Open Settings ▸ Advanced ▸ Audio out. The **audio out latency** row sets the
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

Settings ▸ Reference has one switch for each of five kinds of entry. All five
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

A hidden kind stays in reach. `tag:osc`, `tag:serial`, `tag:fm_matrix`,
`tag:bind` and `tag:internals` list the kind anyway, and Ctrl+D still opens
the documentation.

In the panel, Tab goes to the next tab. The examples tab has parts, tracks,
sound design, and Hydra visuals. The generator tab makes new music from a
direction, such as Acid current. In both tabs, Space plays the music and `c`
copies the code. A copy does not change the score. Paste the code into the
score, then update the score.

F12 enables Piano mode. Use it to enter notes from the keyboard. Leave Piano
mode before you use those keys to edit ordinary text.

## Samples

Open the Samples tab in Reference to browse banks. Right expands a bank and
Left folds it. Space starts or stops a preview. Enter inserts or copies the
selection; check the footer for the current action.

Drag and drop an audio file or folder into the terminal window running Studio
to import samples. You can also add folders and packs in Settings ▸ Samples.
Samples in the set folder are discovered automatically. Use File ▸ Copy
samples into the set when you need a portable set.

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

Ctrl+J opens the smart action at the caret. On a number, it can create a slider.
On a slider, it can change or remove the control. These source edits require
an update before they change playback.

Controller mapping in Settings can bind a MIDI knob or controller axis to a
slider. See [hardware](hardware.md) for setup. Terminals that report pixel
positions give finer mouse control; other terminals use cell positions.

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

Advanced audio settings also set the maximum polyphony. More voices can use
more CPU. A score can override the limit with `setMaxPolyphony(...)`.

## Visuals

The View menu opens the two visuals docks. Use `e` to change a dock's edge and
`+` or `-` to resize it. Use its controls to choose a scope, spectrum, events,
mixer, or another visual. Follow the footer for actions in the selected view.

Scores can also contain inline visualizers. Visual settings control their
appearance and opacity. See [Hydra](hydra.md) for shader visuals and backgrounds.
Visuals stop advancing when playback stops.

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

## Settings

Press Ctrl+O. Use Tab to change pages, arrows to select or change values, and
Space to toggle a switch. Settings include playback, rendering, samples,
controller mapping, key bindings, and the kinds of entry the reference lists.
The About page reports terminal capabilities.

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

Studio saves preferences in `<rustel-config>/studio.json`. The default config
folder is `~/.rustel`, including `%USERPROFILE%\.rustel` on Windows.
`RUSTEL_CONFIG_DIR` selects another folder. Zen mode lasts for the current
session only.

Use Automatic rendering unless you need a specific mode. Available modes
depend on terminal glyphs, colors, image support, and reported dimensions.
An unsupported choice falls back to Automatic. Image rendering can cost more
than text cells. Reduce the frame rate in Advanced settings if visuals use
too much CPU. Check the Log for device or render errors.

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
control of the Studio interface.

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
currently in the editor, including unsaved changes. Live playback continues.
Use Tab and arrows to choose the length, WAV or MP3 format, and output path.

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

Read the source and line number before retrying a failed update. Opening the
Log acknowledges its warning badge. Resolved alerts remain visible as history.
For device failures, check Devices and the [hardware guide](hardware.md).
