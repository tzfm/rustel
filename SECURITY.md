# Score execution security

Scores and prebake files are untrusted JavaScript. They execute inside
a bounded QuickJS realm, not as operating-system scripts. This document defines
the capabilities native score code receives.

## Reporting a vulnerability

Report security vulnerabilities privately. Open the **Security** tab of the
[Rustel repository](https://github.com/tzfm/rustel) and choose
**Report a vulnerability**. Include the affected version, impact, and enough
detail for maintainers to reproduce the problem. See
[GitHub's reporting guide](https://docs.github.com/en/code-security/security-advisories/working-with-repository-security-advisories/privately-reporting-a-security-vulnerability).

If the tab does not show **Report a vulnerability**, open
[an issue](https://github.com/tzfm/rustel/issues) that asks for a private
reporting channel. Do not put vulnerability details in a public issue or
discussion.

## Default boundary

- The realm exposes no filesystem, process, socket, HTTP, or environment API.
- Score-level `samples(...)` performs no I/O itself. It asks the host, and the
  host applies the policy in "Sample access" below. The library default is
  fully inert, while the command line defaults to the strudel.cc parity
  policy.
- CPU, JavaScript heap, pending-job, query, and staged-effect limits bound
  score work in the realm and host.

The built-in sample library is host-owned. Enabling audio fetches its fixed
sample manifests and soundfont locations as needed, and a score has no way to
replace those destinations.

## Modules

Native scores cannot load ECMAScript modules. The source validator rejects
static imports, direct `import(...)`, and `import.meta` before execution. A
rejecting QuickJS module resolver is a separate backstop for imports assembled
at runtime through `eval` or `Function`. Neither layer resolves local paths or
remote URLs.

## Sample access

By default the command line applies the strudel.cc parity policy. A score's
`samples(...)` can fetch from any public HTTPS origin whose server consents
to cross-origin reads. The client checks consent on every response, including
each redirect. Every response must carry `Access-Control-Allow-Origin: *` (or
`https://strudel.cc`). Without that header, the same fetch fails on
strudel.cc. The reachable hosts are therefore the same as in the browser: a
host that does not opt in to scripted fetches stays unreachable, and the
address boundary below still applies. This default never grants `file://`,
plain `http`, or URLs that name loopback.

CORS consent identifies servers that accept script traffic. It does not show
that a server is safe. The rest of the boundary limits a hostile server that
consents: the server cannot be on a private address, it learns nothing beyond
the fetch itself, and memory-safe decoders read its bytes.

Consent belongs to a live response, so the client checks it once per fetch.
Bytes already committed to the score cache serve offline reruns without a new
request, as a browser cache does. After you clear the sample cache, every
fetch checks consent again.

`--strict-sample-origins` removes the parity default, restoring the inert
policy in which nothing loads without an explicit grant. Embedders get the
inert policy by default and opt in with
`ScoreSampleAccess::permit_public_cors_origins`.

Explicit grants extend the default:

```sh
rustel song.strudel --allow-local-samples /srv/kits
rustel song.strudel --allow-sample-origin https://samples.example
```

An origin granted with `--allow-sample-origin` is trusted by the operator
directly, so no CORS consent is demanded of it. This is how a personal
server without CORS configuration, a plain-http origin, or a loopback server
is reached at all. Applications embedding `Session` provide the same policy
through `SessionConfig::score_sample_access`.

### Local root

`--allow-local-samples` confines `samples('local:...')` to one canonical root.
An empty suffix selects the root itself, and a relative suffix selects a
directory below it. There is no working-directory fallback. Absolute paths and
parent traversal are refused. The selected directory must remain beneath the
root after canonicalization, and directory scans skip symlink entries. The
runtime pins an open handle to the granted root and opens samples relative to
that handle, so a filesystem rename between validation and use cannot redirect
a read outside the grant.

### Remote origins

Repeat `--allow-sample-origin` once per origin. Each value is one exact
HTTP(S) origin, without credentials, a path, query, or fragment. Every
manifest and audio URL reached through a sample map must individually pass
the policy, either an exact grant or the parity default, so a manifest on one
origin cannot pivot fetches to a host the policy would refuse. Redirects are
free to stay on their origin but cannot move to another one.

Connections resolve only to permitted public addresses, with one exception: explicitly
named loopback. Loopback is available only when the granted URL names
`localhost`, a name beneath `.localhost`, or a loopback IP, and such a name
must resolve exclusively to loopback. Private, link-local, carrier-NAT,
site-local, multicast, benchmarking, documentation, and selected
special-purpose address ranges are refused. This includes IPv4 TEST-NET
ranges, deprecated 6to4 relay anycast, and IPv6 discard-only, ORCHID, ORCHIDv2,
documentation, and SRv6 SID ranges. These special-purpose refusals are a
conservative fetch policy; they are not a claim that every range is unroutable.
The resolver applies this check to every connection and redirect hop.

The score-loading client sends HTTP paths to the named server. It never
converts a path such as `/file/../../secret` into a local filesystem read. A
separately launched `serve-samples` process maps requests only within its
selected root.

These flags are trust decisions stronger than the default: a granted origin
is fetched without asking the server's consent. Grant only the folders and
origins you intend for the score being run.

## Hydra visuals

The default command and runtime builds include the `hydra` feature. Score
JavaScript records Hydra calls as data; the host validates their structure and
composes GLSL from its built-in function table. A renderer thread sends the
shader to wgpu and the platform's GPU driver, then reads pixels for the terminal.
The score does not receive a general GPU API, but untrusted score values still
reach shader compilation and rendering. The program size, argument count,
nesting depth, output dimensions, and frame delivery have fixed limits. A
minimal build can omit Hydra with `--no-default-features`.

Hydra image sources are host fetches. They must pass the session's score sample
access policy, use public HTTPS addresses, remain on the same origin across
redirects, and return CORS permission for `https://strudel.cc` on every
response. A response body is limited to 16 MiB before image decoding, with
separate decoded-image limits. The stricter Hydra URL rules apply even when an
origin has an explicit sample grant; a sample grant does not authorize Hydra
images from plain HTTP or loopback. Camera input is governed separately by
the studio's camera consent setting.

## OSC output

OSC is a host-mediated UDP send, not a JavaScript socket. By default a hap
(one pattern event) with `oscport` reaches loopback only (`127.0.0.1`, `::1`,
and the RFC 6761 `localhost` names). The live loop never performs DNS:
`oschost` must be an IP literal, so a hanging name cannot stall scheduling.

Non-loopback destinations require an explicit grant:

```sh
rustel song.strudel --allow-osc-host 192.168.1.10
```

Applications embedding `Session` use `SessionConfig::score_osc_access`.
Unspecified, multicast, and broadcast addresses are refused even when named.
A hap aimed at an ungranted host is dropped. It does not end the set.

## MIDI and serial devices

The `midi` and `serial` Cargo features are host-level capabilities. The
default build and the release binaries include both features.
With these features compiled in, CLI live playback and Studio open and write
to the MIDI or serial ports a score names. Scores can also select MIDI input
ports. Serial selectors must exactly match a port enumerated by the operating
system; an empty or `default` selector uses the first listed port. There is no
additional per-score command-line grant. When access to those devices is
inappropriate, do not play untrusted scores live, or use a build without these
features.

The score receives only patterns and controls. Device enumeration,
opening, scheduling, and I/O remain in bounded native host code, and no
general device API is installed in QuickJS. MIDI and serial failures are
reported and dropped without ending the set. One-shot query, validation, and
offline render routes do not open these external outputs.

## Remote sample cache

An exact-origin grant authorizes validated manifests and audio from that origin
to be kept in a bounded persistent cache. Authorization is checked before each
cache read or write. Invalid responses are not committed, and an invalid entry
in the dedicated score namespace is removed before one network retry. Local
sample files are read in place and never copied into this cache.

Manifest and audio responses use separate cache identities even when they name
the same URL. A decoder for one kind therefore cannot reject or remove a valid
entry belonging to the other.

Score-selected entries have their own `score` namespace beneath the sample
cache base. The namespace admits at most 4,096 entries and 512 MiB. Each
`SampleLibrary`, normally one Session, adds at most 2,048 entries and 256 MiB.
Cache hits do not consume the per-Session allowance. Admission is charged
before a new entry is opened or written.

Quota admission does not evict valid entries on behalf of a score. When either
ceiling is full, validated bytes remain usable for the current run but are not
persisted. This prevents one score from displacing resources needed by a later
offline render. Entries from the earlier shared cache layout are considered
only after authorization and validation, then copied into the bounded namespace
when its limits permit. The shared copy is retained because that layout also
contains pinned and host-trusted data whose provenance cannot be recovered from
its filename.

`RUSTEL_SAMPLE_CACHE` selects the sample cache base. Otherwise the runtime
uses `cache/samples` under `RUSTEL_CONFIG_DIR`, or `~/.rustel/cache/samples`
by default (with `USERPROFILE` as the Windows home fallback). Without a home
or configuration override it uses the operating-system temporary directory.
Score-selected entries live in the `score` child directory.

Run `rustel clear-score-cache` and confirm, or pass `--force` in a script, to
remove the score namespace and prevent old shared-layout entries from being
imported again. Trusted built-in sample data
is left intact. Applications embedding the runtime get the same location and
operation from `rustel_runtime::score_sample_cache_dir()` and
`rustel_runtime::clear_score_sample_cache()`.

`rustel samples cache` pre-fetches the pinned, hash-verified default pack
files into the trusted cache; `rustel samples clear` empties everything that
was downloaded, score-selected entries included, and is the wider of the two
removals. Neither widens what a score may fetch: both work on data the host
chose to store.

## Local sample server

`serve-samples` is a host-selected browser bridge, not a capability available
to score JavaScript. It listens on loopback by default, and binding it to a
network interface is an explicit host decision. Browser reads require an exact
origin: `https://strudel.cc` by default, or the origins named with
`--allow-origin`. Requests must also carry the listener's permitted `Host`
authority. This policy is separate from the native score capability granted by
`--allow-sample-origin`.

The server exposes only visible `.wav`, `.mp3`, and `.ogg` files beneath its
pinned root. Plain and percent-encoded parent traversal, absolute paths,
escaping symlinks, and Windows alternate data streams are refused. Confined
file symlinks are available only when the platform enforces them atomically,
and other platforms skip them. Request, scan, manifest, handler, file-size,
and in-flight body limits are documented in [Playing your own
samples](docs/local-samples.md#playing-local-samples-in-a-browser).

## Studio remote control

Remote control is a host-selected Studio listener, not a capability available
to score JavaScript. It is off by default. Start it with
`rustel studio --remote-control` or from the Options menu in Studio. It
listens on `127.0.0.1:9247` unless you select another address. Binding it to
a network interface is an explicit host decision. A build without the
`remote-control` Cargo feature has no listener.

The token grants full control of the Studio interface. Studio handles a key
from an authenticated client like a key from the terminal, and `screen`
returns the text and colors on the screen. A client can therefore edit, run,
and save the score, export audio to a file path that it types, paste the
host clipboard into a text field and read it from the screen, reveal the
token in the remote-control panel, and quit Studio. Give the token only to
people and programs that you trust with your keyboard.

The connection is plaintext TCP. The protocol does not encrypt data, and the
client cannot verify the listener. An observer on the network path can read
the token, the keys, and the screen. A different program that listens on the
same address receives the token that the client sends. For a connection from
another machine, keep the listener on loopback and use an encrypted tunnel
such as SSH.

Without a custom token, Studio generates a new random 60-bit code each time
the listener starts. The code stops working when the listener stops. Studio
writes the code to a temporary file that only the current account can read,
and removes the file when the listener stops. A custom token, set with
`--auth-token` or in the remote-control panel, has 4 to 256 ASCII characters
without spaces and does not change when the listener restarts. Use a long
random value. `--auth-token` also exposes the token in the process
arguments, so do not use it on a shared machine.

The listener serves at most 8 connections at a time and answers `err busy` to
any other. A command line is limited to 1024 bytes. A connection must
authenticate within 3 seconds, and a failed attempt closes it. After a
connection ends without authentication, the listener pauses 200 ms before it
serves a new connection. An authenticated connection closes after 60 seconds
without a command.

The limit of 8 includes connections that have not authenticated. A program
that can connect to the listener can use all 8 without the token. It opens a
new connection each time the listener closes one. While it does this, a new
client gets `err busy`, even when it has the correct token. Clients that
authenticated earlier continue to work, and the keyboard in the Studio
terminal is not affected. Select a listener address that only trusted
programs can reach.

## Host inputs

Paths supplied directly as CLI arguments (score files, prebake files, session
files, and render destinations) are selected by the person running the process
and sit outside the score boundary.
