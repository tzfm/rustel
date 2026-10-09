# MIDI, OSC, and serial hardware

Live playback and Studio can send a score to local audio and hardware together.
The default build and the release binaries include live audio, MIDI, OSC, and
serial output. To build from source:

```sh
cargo build --profile local -p rustel
```

A build with `--no-default-features` leaves these routes out. The
`device-audio`, `midi`, `osc`, and `serial` features add them back. See
[building](building.md).

The playback flags below use `rustel play`, not `rustel trace`.
`query`, `validate`, and offline rendering do not open output devices.

## Finding MIDI devices

```sh
rustel devices
```

Inputs and outputs have separate lists. Select a port by index or a distinctive,
case-insensitive part of its name. Studio detects newly connected ports.

On Linux, MIDI needs access to `/dev/snd/seq`. If access is denied, add your
user to the `audio` group, then log in again:

```sh
sudo usermod -aG audio "$USER"
```

## MIDI output

`.midi()` adds a MIDI route and keeps the local audio route:

```javascript
$: note("c3 e3 g3 b3").midi('IAC Bus 1')
$: note("c3 e3").midi(0)
$: note("c a f e").midiport("<0 1>").midi()
```

Either quote style works for device names. `.midi()` defaults to output 0.
A blank selector is refused. Per-event `midiport` overrides the `.midi()`
selector. Port errors do not stop local audio.

Supported controls include `midichan`, `velocity`, `ccn`, `ccv`, `progNum`,
`midibend`, `miditouch`, `midicmd`, and CC messages from `midimap`.
SysEx (`sysex`, `sysexid`, `sysexdata`), NRPN (`nrpnn`, `nrpv`), and
`polyTouch` are not sent. Send NRPN through CC 99, 98, 6, and 38 instead.

### Publishing a virtual output

On macOS and Linux:

```sh
rustel play song.strudel --midi-virtual rustel
```

Use `.midi('rustel')` in the score. Repeat the flag for more ports.
Windows cannot publish a virtual WinMM port. Use an installed loopback driver.

## MIDI input

```sh
rustel midi-monitor MiniLab
rustel midi-monitor MiniLab --learn
```

`--learn` prints an expression for the first control moved, then exits.
`--json` prints one object per message. `--timing` includes clock and
active-sensing messages.

```javascript
const cc = await midin('MiniLab')
$: note("c2 c3").s("sawtooth").lpf(cc(74).range(200, 8000))
```

Use `cc(74, 2)` to read channel 2; channels are 1-16. A missing input leaves
controls at their low/default value. Scores can read CC values and note hits.
Pitch bend, aftertouch, program changes, and clock appear in the monitor but
cannot be read by a score. `midikeys` uses its length argument, not note release.

Studio can use learned pads to launch scenes. A MIDI knob or fader can move
a [slider](studio.md#sliders). The mapping page of Settings has 12 slots.
Slot N drives slider N of the playing score. No slot has a default control.
Press Enter on a slot, then move the knob. The slot keeps the CC number and
channel of that knob. Press `t` to change the knob mode: scaled (the
default), jump, or relative. Use relative for binary-offset encoders.

## MIDI clock

```sh
rustel play song.strudel --midi-clock-out MiniLab
rustel play song.strudel --midi-clock-in MiniLab
```

Output sends 24 pulses per beat, plus Start, Continue, and Stop messages.
Input follows the external tempo and overrides `setcps` while clock arrives.
An incoming Start does not start playback. A clock without a Start message
sets tempo without setting song position. A clock silent for half a second
is dropped.

In Studio, press Ctrl+P and use Tab to select the `midi` tab. The `clock in`
and `clock out` rows are below the port list. Left and Right step a clock row
through `none` and the ports ticked for that direction. On a port row, Left
and Right select the `in` or `out` box and Space ticks it.

Use `midi-monitor PORT --timing`
to inspect traffic. To send transport messages from the score instead:

```javascript
$: midicmd("clock*96, <start stop>/2").midi('IAC')
```

At four beats per cycle, `clock*96` sends 24 pulses per beat.

## Gamepad

```javascript
const gp = gamepad(0)
$: note("c a f e").mask(gp.a)
$: s("bd*4").mask(gp.tglA)
$: note("c3").s("sawtooth").lpf(gp.x1.range(100, 4000))
```

Use `gamepad(0)` through `gamepad(3)` in connection order. Reconnecting a pad
with the same name retains its slot.

- Buttons: `a b x y`, `lb rb lt rt`, `up down left right`, `l3 r3`, `start back`.
  They read 1 while held. `tglA`, `tglLB`, and other `tgl` names toggle on press.
- Sticks: `x1 y1 x2 y2` range from 0 to 1, centred at 0.5.
  `x1_2 y1_2 x2_2 y2_2` range from -1 to 1.
- `gp.btnSequence(['down', 'right', 'a'])` reads 1 for two seconds after
  that sequence.

```sh
rustel gamepad-monitor --duration 10
```

Studio lists pads in the devices dock. A stick can also drive a mapping slot
(see [MIDI input](#midi-input)). Press Enter on a slot, then push the stick
fully. The slider moves while you hold the stick. A larger push moves it
faster. Pattern queries run ahead of playback, so short taps
can be missed. Hold a button through the notes it should enable.

With no pad, or without the `gamepad` feature, buttons read up and sticks
read centred. Linux builds with this default feature need `libudev` headers:

```sh
sudo apt-get install -y libudev-dev pkg-config
```

See [building](building.md) to omit gamepad support. `--all-features` enables it.

## Audio in

Select an input in Studio's Devices panel, or name it for live playback:

```sh
rustel play song.strudel --audio-input Scarlett
```

```javascript
$: s("in").lpf(2000).room(.5).delay(.3)
```

`--input` is an alias. Select by device name or a part of it. No input opens by
default. Studio opens a selected input even while stopped. Its mixer `in`
fader adds up to +48 dB. Select `none` to release it.

`in` reads channel 0; `in:1` reads channel 1. Each voice is mono. Use two panned
lanes for stereo. Channels above `in:15` are refused. Studio also checks
literal channels against the open device. CLI playback reads silence for a
channel the device lacks. With no input open, all valid input references are
silent.

Input latency is automatic. Failed streams retry with increasing delays.
`fit`, `begin`, `end`, `speed`, and `loop` do not transform live input.
`note` does not retune it. `seg` and `clip` can gate it; `chop` and `rev`
affect events, not the incoming waveform.

## OSC

`.osc()` sends timed `/dirt/play` bundles to `127.0.0.1:57120`:

```javascript
$: s("bd sd").osc()
```

An argument changes the port. `oschost` and `oscport` also accept patterns.
A non-loopback destination needs an explicit grant:

```sh
rustel play song.strudel --allow-osc-host 192.168.1.10
```

```javascript
$: s("bd sd").osc().oschost("192.168.1.10")
```

Hostnames, multicast, broadcast, and unspecified addresses are refused.

Each bundle is sent about half a second before its time and cannot be taken
back. A saved edit, a moved slider, or a tempo change therefore reaches OSC
up to half a second after it reaches the audio. A tempo change can repeat or
skip notes where the old timing ends.

### Setting up a SuperDirt receiver

Install SuperCollider and its SuperDirt and Dirt-Samples quarks. In
SuperCollider, install once, then start SuperDirt:

```supercollider
Quarks.install("SuperDirt");
Quarks.install("Dirt-Samples");
// After installation:
SuperDirt.start;
```

Run the OSC score after SuperDirt finishes starting. Do not call
`SuperDirt.start` inside your own server boot callback: it reboots the server.

## Serial output

```javascript
$: note("60 64").serial(115200, true, false, '/dev/ttyACM0')
```

Arguments are baud, CRC, short field names, and port. Defaults are 115200,
no CRC, full names, and the first enumerated port. An explicit name must
exactly match an enumerated port, such as `/dev/ttyUSB0` or `COM3`.
Other paths and aliases are refused. Empty or `default` selects the first port.

Ports open on first use. Disconnects, late messages, and full queues are
reported without stopping the set. A port keeps its baud until the set
restarts. Use one consistent selector for each port.

Writes are queued about half a second ahead, as OSC bundles are sent. An edit
reaches the port up to half a second after it reaches the audio.

### Testing serial output without a microcontroller

Use an enumerated USB-serial adapter. On Windows, a com0com pair works if
its ports appear in the system list. Read the peer port at the same baud.
Pseudo-terminal paths from `socat` are not accepted on Linux or macOS.

## Output latency

If sound crackles, increase **audio out latency** in Settings ▸ advanced,
or set a buffer size:

```sh
rustel play song.strudel --buffer-frames 256
```

Studio accepts the same flag. The range is 32-16384 frames. Larger buffers
add latency. Automatic requests 128 frames, or 2048 inside WSL.
Studio's arrows offer automatic and 32-2048; other valid values are retained.
The stream report shows the size the device grants.

A small buffer leaves little time for each callback: 32 frames at 48 kHz is
0.67 ms. The reverb (`room`) does one larger step every 2048 frames. At 32 or
64 frames that step can take longer than one callback, and Studio then shows
the hint to raise the audio out latency. With reverb in the score, use 128
frames, or 64 on a fast machine. On Linux, the `powersave` CPU governor makes
each callback slower; use `performance` for small buffers.

CoreAudio and ALSA adjust the callback buffer. WSL usually needs automatic
or larger. WASAPI shared mode keeps the device period; requests below that
period cannot reduce latency. Larger requests add buffer space.

`RUSTEL_LIVE_BUFFER_FRAMES` overrides automatic.
`RUSTEL_MAX_LIVE_LATENCY_MS` sets the accepted latency ceiling, from 50-5000 ms.

## Trust boundary

The default build and the release binaries include the `midi` and `serial`
features. With these features, live scores can access the MIDI and serial
ports visible to the user without a separate grant. Play only scores that you
trust with those devices, or use a build without these features. Non-loopback
OSC always needs `--allow-osc-host`.
These routes do not provide general JavaScript device, socket, or filesystem
access. See [SECURITY.md](../SECURITY.md).
