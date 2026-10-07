# Playing your own samples

Grant the score a sample folder:

```sh
rustel ~/songs/tune.strudel --watch --allow-local-samples ~/my-samples
```

```javascript
samples('local:')
$: s("kick*2 snare").gain(0.8)
```

No server is needed. Without the grant, `local:` cannot read files.
`rustel check tune.strudel --allow-local-samples ~/my-samples` waits up to
five seconds for imports, then checks sound names from completed libraries.
`--no-samples` omits sound-name checks.

## Source registration history

A sample map above 4 MiB is refused. Source history keeps up to 32 MiB of
unique map text. When old history is removed, the registered banks and audio
stay.

## In the studio: the set's own folder

Audio in the set folder appears under `set` in the Samples browser.
No `samples()` call or flag is needed.

| File | Score |
| --- | --- |
| `deep_bass.wav` | `s("deep_bass")` |
| `kicks/1.wav`, `kicks/2.wav` | `s("kicks")`, `s("kicks:1")` |

Audio files directly in the set folder form one bank. This bank has the name
of the set folder. A file also plays by its filename without the extension
when that name has only letters, digits, `_`, and `-`. A filename does not
replace a bank name or a built-in synth name such as `sine`. If two files
have the same name, that name does not play. Use the bank name and an index.

Studio skips `sessions/` and `exports/`. Press **Ctrl+S** to find added or
removed files. Set samples override imported banks with the same name.
An explicit `samples()` definition in the score keeps its own name override.

The `recordings` bank is Studio-only. For CLI playback, grant the recordings
folder and load it with `samples('local:')`; use the resulting folder bank names.

To delete a local sample, select it and press **Alt+D**, then **Enter**.
Expand a multi-file bank with Right first. **Esc** or another browser action
cancels. Deletion is permanent, including in imported local folders.
Later indices move down: after deleting `kick:0`, the old `kick:1` becomes
`kick:0`. Downloaded packs and score-defined sources cannot be deleted here.

## Imported sources: folders and packs every set sees

Open Settings ▸ Samples:

| Key | Action |
| --- | --- |
| `a` | Add a folder, URL, or shorthand |
| Space | Enable or disable |
| Enter | Refresh the source |
| `r` | Alias one of the source's banks |
| `d` | Remove the source; keep its files |

You can also drop an audio file or folder onto Studio.

| Shorthand | Source |
| --- | --- |
| `github:user/repo` | Repository `strudel.json` |
| `bubo:drum` | `github:Bubobubobubobubo/dough-drum` |
| `shabda:bass,kick` | Samples searched by word |
| `shabda/speech/en-US/m:music,vocode` | Speech; optional language/gender default to `en-GB/f` |

Shabda sample results are random and cached. Refresh to get a new selection.
Source priority is set folder, imported sources, General MIDI fonts, then
bundled banks. The browser shows each bank's source.

In Settings ▸ Samples, `r` aliases a user-imported bank without renaming its
files. Restore the original name to remove the alias. **fetch imports**
downloads used samples before playback. **cache all** downloads all remote
packs; **refresh all** refreshes their lists. **clear cache** needs Enter
twice. Click the `⇣` chip to see download jobs.

Set folders and Studio imports are not available on strudel.cc. To use local
audio there, [serve the sample folder](#playing-local-samples-in-a-browser).

## Why `local:` and not a path

Use `samples('local:')` for the granted folder. `samples('./drums')` is not a
local filesystem grant.

## Selecting a folder safely

```javascript
samples('local:')
samples('local:808')
```

The second form selects a subfolder. Absolute paths, `..`, paths outside the
grant, and symlink entries are refused or skipped. There is no fallback to
the working directory.

## How a folder becomes banks

```text
my-samples/
  kick/   k0.wav  k1.wav  k2.wav
  snare/  s0.wav  s1.wav
```

The parent folder names the bank. Files sort by filename, so `kick:1` is
`k1.wav`. Scans use `.wav`, `.mp3`, and `.ogg`; hidden folders are skipped.
Local files stay on disk and are not copied to the remote cache.

## A score that uses a local sample server

For `samples('http://localhost:5432')`, run a server and grant its exact origin:

```sh
rustel song.strudel --allow-sample-origin http://localhost:5432
```

A failed URL never grants access to local files.

## Playing local samples in a browser

Serve a folder containing only audio intended for browser access:

```sh
cd ~/my-samples
rustel serve-samples
```

Use `samples('http://localhost:5432')` in the browser. The default listener is
`127.0.0.1:5432`. `--port` changes the port. `--host 0.0.0.0` exposes it to
other machines.

Only WAV, MP3, and OGG files are served. The default browser grant is exactly
`https://strudel.cc`. `--allow-origin` replaces that default; repeat it for
each required origin:

```sh
rustel serve-samples \
  --allow-origin https://strudel.cc \
  --allow-origin http://localhost:3000
```

An origin has a scheme, host, and optional port, with no path. Other browser
origins receive HTTP 403. Clients without an `Origin` header can still read
files. Requests must use the listener's host and port; loopback also accepts
`localhost` names.

`--allow-origin` grants browser access to this server. The separate
`--allow-sample-origin` grants native score fetches. Neither implies the other.
Remote fetches restrict private and special-purpose addresses; explicitly
granted loopback servers are allowed. See [Sample access](../SECURITY.md#sample-access).

Files above 128 MiB and scans above 16,384 audio files are refused. Escaping
symlinks are refused; systems without atomic path confinement skip all symlinks.

## Sharing a set built on local samples

Choose **File ▸ Copy samples into the set**, then share the set folder.
This copies banks used by its scores without changing those scores.
It does not download missing files or overwrite samples already in the set.

Incomplete banks and keyed banks such as `piano` are reported and skipped.
For an incomplete bank, play it once or enable **fetch imports**, then retry.
Alternatively, share the sample folder and let the recipient grant it with
`--allow-local-samples`.
