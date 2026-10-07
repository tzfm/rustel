# Recording and replaying a set

A session records score changes and their times, not audio. Keep the same
samples available for replay. Use export to create audio.

## Recording

Watched playback and Studio record automatically:

```sh
rustel song.strudel --watch
rustel studio set/
```

Watched playback writes
`~/.rustel/sessions/<song>-<timestamp>.rustel-session`. Studio writes
`sessions/session-<timestamp>.rustel-session` in the set folder. For a single
score file, the set folder is the folder of that file.
Unwatched playback does not record. A recording error is reported without
stopping playback.

```sh
rustel song.strudel --watch --save-session=debug
rustel song.strudel --watch --session-file take2.rustel-session
rustel song.strudel --watch --no-save-session
RUSTEL_SESSION_DIR=/media/usb/sets rustel song.strudel --watch
```

Studio accepts the same three flags. `RUSTEL_SESSION_DIR` does not move Studio
tapes. With no path, Studio opens the last set. On first use, or when that
folder is gone, it creates a new set in the sets folder and names it for the
date. A score path that does not exist opens an unsaved starter score.

| Mode | Contents |
| --- | --- |
| `normal` (default) | Installed saves |
| `debug` | All saves, rejected evaluations, and diagnostics |

## Replaying

```sh
rustel replay tonight.rustel-session
rustel replay tonight.rustel-session --from 600
rustel replay tonight.rustel-session --export tonight.wav
rustel replay tonight.rustel-session --export tonight.mp3
rustel replay tonight.rustel-session --out watch-me.strudel
```

`--from` is seconds from the start, not cycles. Replay starts with the latest
installed save at or before that time. Without one, it stays silent until a
save installs. Studio's first save can occur well after the session starts.
The final state plays until you stop replay.

Live replay evaluates later saves again, including previously rejected saves.
Export uses only saves marked installed. It refuses a session with none.

Export renders offline without a device and keeps voices and tails across
save boundaries. The extension selects WAV or MP3. `--format` overrides the
choice but must match the extension. MP3 uses 320 kbps.
`--duration SECS` sets export length from `--from`.

`--speed 4` makes edits arrive four times faster without changing the score's
tempo. Cycle-dependent patterns can therefore differ. Use `--speed 1` to
reproduce timing. Export ignores `--speed`.

## Watching the code

Use `--follow` to display the active score during live playback or replay.
For a separate terminal renderer:

```sh
rustel song.strudel --watch --score-events 2>&1 >/dev/null | rustel watch-code
rustel replay take.rustel-session --score-events 2>&1 >/dev/null | rustel watch-code
```

`--score-events` writes events for installed scores. Rejected saves do not
appear. `--out` keeps the replay's changing score at a path an editor can open;
otherwise replay uses a temporary file.

## Preloading

The first sounds of a score load automatically. Request later sounds when
needed:

```javascript
await preload("bd sd hh:4 gm_choir_aahs")
$: s("bd*4")
```

`bd` warms the first file of the bank; `bd:3` warms that variant. Initial
playback waits for warmup.
Mid-set requests load while music continues. `await` is optional.
Replay warms sounds named throughout the session before starting.

## Studio log

Studio also writes `studio.log` in `~/.rustel/sessions/`, or in the folder that
`RUSTEL_SESSION_DIR` names. The log is not in the set folder. It contains
diagnostics and save or device errors. Replay ignores this log. See
[Log](studio.md#log) for the record format.

## The file

Sessions use JSON Lines: a header followed by save, log, or control records.
`t` is seconds of audio since the set started. Scores are base64-encoded;
repeated scores can use `{"ref": n}` to refer to an earlier save.

```json
{"version":2,"recorded":"2026-08-19T18:18:04Z","keeps":"all-saves-and-diagnostics"}
{"t":0.0,"kind":"save","status":"installed","source":"JDogcygiYmQqNCIpCg=="}
```

The header describes the format version, recording time, and retained events.
An optional `baseline_cps` records a non-default initial tempo.
A save's `via` can identify a slider update or stop. Control records, including
fader moves and audio-take markers, are recorded but not applied by replay.

Each line is limited to 32 MiB, excluding LF or CRLF. Blank event lines and
an incomplete final JSON record are ignored within that limit, so a truncated
file can replay its complete records.
