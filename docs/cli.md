# Command line

Use `rustel --help` or `rustel <command> --help` for all options.
Use `rustel studio` for the [terminal editor](studio.md).

## Play

```sh
rustel play song.strudel
rustel play song.strudel --watch
```

Press Ctrl+C to stop. With `--watch`, each saved change updates playback.
If evaluation or the first scheduling window fails, the last valid score
continues.

Add `--prebake setup.js` to load helpers before the score. Put shared functions
on `globalThis`. A prebake cannot set tempo, select MIDI input, or open visuals.
Studio has its own prebakes in Settings.

Use `--buffer-frames 128` to request an audio buffer of 32-16384 frames.
Increase it if sound crackles. Studio calls this setting `audio out latency`.
The device can use a different size. See [Hardware](hardware.md).

## Export

```sh
rustel export song.strudel -o take.wav --duration 30s
rustel export song.strudel -o take.mp3 --cycles 32
rustel export song.strudel -o take.wav --duration 30s \
  --until-silence --silence-floor -60 --silence-hold 2
```

The last command renders for about 30 seconds, then waits for audio to stay
below **−60 dBFS for 2 seconds**. Silence detection starts only after audio
has reached the threshold. The extra time is limited to 60 seconds, including
for a score that never reaches the threshold. These are the default floor
and hold values.

`export` and `render` are the same command. Export uses no audio device.
The defaults are 8 cycles and a 16-bit stereo WAV at 48 kHz, saved beside the
score. `--duration` accepts `30s`, `2m`, `1:30`, `1h`, or `16b`. Durations of
at least one cycle round to the nearest whole cycle. `--prebake setup.js`
loads helpers before the score, as for `play`.

| Format | Output |
| --- | --- |
| `scalar-wav` | 16-bit WAV; default |
| `scalar-f32` | Unclamped float WAV |
| `mp3` | 320 kbps MP3 |
| `onset-json` | Scheduled events |
| `wav` | Silent timing file |

The output extension selects MP3 or JSON. Use `--format` to choose explicitly.
Ctrl+C stops a WAV export and finalizes a partial file if writing has started.

CLI export has no master limiter. Its voice limit is 128 unless the score
calls `setMaxPolyphony`. Score-level `.limit()` still applies.
[Studio export](studio.md#export) can use the live set's limiter and voice limit.

## Check and query

```sh
rustel check song.strudel
rustel query -e 'note("c e g").s("piano")' --begin 0 --end 2 --json
rustel doc inspire
rustel validate song.strudel
rustel bench song.strudel
```

`check` checks syntax, mini-notation, names, and arguments without playback.
`query` prints pattern events; JSON uses exact fractions. Query errors and
refused sample imports return a failure status. A score must end on a pattern
or collect patterns with `$:`. A definition-only `register(...)` is valid setup.

`validate`, `bench`, and `trace` provide validation, timing, and scheduling
reports. Use their `--help` for options. `trace` gives no sound without
`--device-audio`. Use `rustel play song.strudel` to hear a score. Hydra
pictures are visible in Studio; CLI playback and query do not render them.

## Sessions

Watch mode records score changes in `~/.rustel/sessions/` by default.

```sh
rustel play song.strudel --watch --save-session debug
rustel play song.strudel --watch --no-save-session
rustel replay take.rustel-session
rustel replay take.rustel-session --export set.wav
```

See [Sessions](sessions.md) for recording and replay limits.
To show the active code in the terminal that plays the score, use `--follow`.
To render it in a separate process, pipe the score events to `watch-code`:

```sh
rustel play song.strudel --watch --score-events 2>&1 >/dev/null | rustel watch-code
```

## Hardware

```sh
rustel devices
rustel midi-list
rustel midi-monitor
rustel gamepad-monitor
rustel play song.strudel --midi-clock-out IAC
rustel play song.strudel --audio-input Scarlett
```

See [Hardware](hardware.md) for MIDI, OSC, serial, gamepads, and audio input.
For remote keypresses and screen access, use `rustel studio --remote-control`.
It binds to `127.0.0.1:9247` by default. Clients must authenticate. The protocol
is plaintext TCP; use a trusted network or an encrypted tunnel.
See [Remote control](studio.md#remote-control).

## Samples

```sh
rustel play song.strudel --allow-local-samples ~/kits
rustel samples cache --list
rustel samples cache piano
rustel samples cache
rustel samples clear --dry-run
```

Public HTTPS sample packs with CORS enabled load by default. Other access needs
an explicit grant. See [Local samples](local-samples.md) and
[Security](../SECURITY.md).

`samples cache` downloads the shipped packs for offline use. Repeat it to
resume an interrupted download. `samples clear` removes all downloaded samples;
`clear-score-cache` removes only score-selected samples. Both ask for confirmation.
Use `--dry-run` to preview, or `--force` for a script.

## Output and exit status

Results go to stdout; notices and errors go to stderr. `--json` selects JSON
where supported. `-v` and `-vv` add live diagnostics; `-vvv` streams live events
as newline-delimited JSON on stderr. It does not select JSON for other commands.

Use `--plain` to remove animation and styling, `--no-color` to remove styling,
and `--quiet` to hide notices and progress. `--no-input` prevents prompts;
destructive commands then require `--force`.

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Evaluation, validation, or runtime failure |
| 2 | Invalid arguments |
| 3 | Resource limit |
| 4 | I/O failure |
| 5 | Audio failure |
| 128 + signal | Interrupted: 130 for Ctrl+C, 143 for SIGTERM |

Recoverable MIDI or serial failures can leave playback running and return 0
on normal completion. Argument errors use text diagnostics, even with `--json`.

## Configuration

Data is stored in `~/.rustel`. Override it with `RUSTEL_CONFIG_DIR`, or use
`RUSTEL_SAMPLE_CACHE` and `RUSTEL_SESSION_DIR` for those directories alone.

On startup, interactive commands check for a new stable release in the
background, at most once every 24 hours. A newer version prints a notice on
stderr. Studio prints the notice only after you quit and return to the
terminal. Run `rustelup` to update.
On Windows, use `rustelup.cmd` if PowerShell blocks scripts.

Checks use GitHub's public release API. Network failures stay silent and do
not delay startup or exit. Results are cached in `~/.rustel/cache/update-check.json`.
The worker ends when Rustel exits. If a command exits before its request
finishes, the next eligible launch tries again.
Pipes, CI, JSON output, completion scripts, the `samples` commands, `--quiet`,
and `--no-input` skip checks and notices.

Read or change the preference with:

```sh
rustel config get check_updates
rustel config set check_updates false
rustel config set check_updates true
```

Settings always apply to your user account, so there is no `--global` flag.
`get` prints the saved value, or `true` when unset. `set` writes
`~/.rustel/rustel.json`, preserving other settings:

```json
{
  "check_updates": false
}
```

Disable checks for one command with `--no-update-check`, or set
`RUSTEL_NO_UPDATE_CHECK` to any nonempty value. These overrides take precedence
over the saved setting. `get` still reports the saved value.

`RUSTEL_CONFIG_DIR` moves this file and the update cache too. The settings file
is optional. Remove the setting or set it to `true` to restore checks.

```sh
rustel completions --install
rustel --version
```

Completion installation writes a shell script and prints any required setup;
it does not edit your shell configuration. See [rustelup](../rustelup/README.md)
to install, update, or uninstall.
