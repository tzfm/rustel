/*
snippets.rs - Things worth pasting rather than remembering
Rustel snippet table:
Copyright (C) 2026 Rustel contributors

The Hydra example is adapted from the Strudel contributors' documentation,
the Hydra learning page (https://strudel.cc/learn/hydra/, AGPL-3.0-or-later).

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! The reference's snippets: not a function each, but the lines nobody
//! remembers - the `await` in front of `initHydra`, the single quotes a
//! device name needs, the four arguments of `.serial()`. They sit in the
//! same list as the functions, searched the same way, and labelled, so a
//! reader who types `midi` finds `midin` the function and, beside it, the
//! two lines that actually make a knob do something. Enter pastes one at
//! the caret, on lines of its own.

/// One snippet: a name to find it by, what it does, and the code.
pub struct Snippet {
    /// A short phrase, never a function's name: `midi input`, not `midin`,
    /// so the two sit side by side in a search rather than one hiding the
    /// other.
    pub name: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
    pub code: &'static str,
    /// Beside `snippet`, which every one carries.
    pub tags: &'static [&'static str],
    /// What a reader might type looking for it - `microphone` for the
    /// audio input, `arduino` for serial - ranked like another name, but
    /// never shown as one.
    pub keywords: &'static [&'static str],
}

pub const SNIPPETS: &[Snippet] = &[
    Snippet {
        name: "hydra setup",
        summary: "Start Hydra behind the score, with the score's own picture as s0.",
        description: "The `await` is the part everyone forgets: `initHydra` returns a promise, and without the `await` the sketch under it runs before the renderer exists. `feedStrudel: 1` feeds the score's picture in as `s0`, so a sketch can draw on it.",
        code: "await initHydra({ feedStrudel: 1 })\n\nsrc(s0).kaleid(H(\"<4 5 6>\")).diff(osc(1, 0.5, 5)).out()",
        tags: &["visuals"],
        keywords: &[
            "initHydra",
            "hydra",
            "visuals",
            "picture",
            "video",
            "feedStrudel",
        ],
    },
    Snippet {
        name: "gamepad setup",
        summary: "A pad as patterns: a button masks the notes, a stick sweeps the filter.",
        description: "`gamepad(0)` is the first pad plugged in. Buttons are 1 while held (a b x y, lb rb lt rt, up down left right, l3 r3, start back), `tglA` and the rest flip on every press, sticks read x1 y1 x2 y2 from 0 to 1, and `gp.btnSequence('dra')` is 1 for two seconds after that combo.",
        code: "const gp = gamepad(0)\n$: note(\"c a f e\").mask(gp.a)\n$: note(\"c4 d3 a3 e3\").s(\"sawtooth\").lpf(gp.x1.range(100, 4000))",
        tags: &["external_io"],
        keywords: &[
            "gamepad",
            "controller",
            "joystick",
            "pad",
            "buttons",
            "sticks",
        ],
    },
    Snippet {
        name: "samples from github",
        summary: "Load a sample pack from a repository's strudel.json.",
        description: "`github:user/repo` reads the repository's `strudel.json` and registers every bank in it; the pack is fetched once and cached. Settings ▸ Samples does the same for every set without a line in the score.",
        code: "samples('github:tidalcycles/dirt-samples')",
        tags: &["samples"],
        keywords: &[
            "samples",
            "github",
            "import",
            "load",
            "pack",
            "strudel.json",
        ],
    },
    Snippet {
        name: "samples from freesound",
        summary: "A bank a word: freesound searched and packed into a map.",
        description: "`shabda:` searches freesound for each word and packs what it finds into a bank of that name, so `s(\"bass\")` plays a bass. The answer is not the same twice.",
        code: "samples('shabda:bass,kick')",
        tags: &["samples"],
        keywords: &["samples", "shabda", "freesound", "search"],
    },
    Snippet {
        name: "samples from a map",
        summary: "Name your own files, with the folder they live under.",
        description: "A map from bank name to file, and the base every file is under - a URL, or a `github:` spelling with a branch. A bank may be one file or a list of them, played as `s(\"bd:1\")`.",
        code: "samples({ bd: 'bd/BT0A0D0.wav', sd: 'sd/rytm-01-classic.wav' }, 'github:tidalcycles/dirt-samples/master/')",
        tags: &["samples"],
        keywords: &["samples", "map", "url", "folder", "files", "custom"],
    },
    Snippet {
        name: "midi input",
        summary: "A knob on a controller as a pattern, by its CC number.",
        description: "`midin` must be awaited and the name single-quoted - double quotes are mini-notation and the name never arrives. The name is an index or a part of the port's name; `rustel midi-monitor` shows the CC numbers a controller sends. A controller that is not there leaves the control at its low bound and says so without stopping the set.",
        code: "const cc = await midin('MiniLab')\n$: note(\"c2 c3\").s(\"sawtooth\").lpf(cc(74).range(200, 8000))",
        tags: &["external_io", "midi"],
        keywords: &[
            "midin",
            "midi in",
            "controller",
            "knob",
            "cc",
            "control change",
            "hardware",
        ],
    },
    Snippet {
        name: "midi keys",
        summary: "Notes played on a controller, as a pattern of notes.",
        description: "`midikeys` is awaited and single-quoted like `midin`. The function it returns takes the note length, since a key here has no release.",
        code: "const keys = await midikeys('MiniLab')\n$: keys(.5).s(\"piano\")",
        tags: &["external_io", "midi"],
        keywords: &["midikeys", "keyboard", "keys", "notes", "hardware"],
    },
    Snippet {
        name: "midi output",
        summary: "Send a pattern's notes to a MIDI port.",
        description: "The name is an index or a part of the port's name, single-quoted. `.midi()` layers MIDI onto the pattern: the local sound keeps playing unless the pattern has none.",
        code: "$: note(\"c3 e3 g3 b3\").midi('IAC')",
        tags: &["external_io", "midi"],
        keywords: &["midi out", "midi", "synth", "hardware", "port", "IAC"],
    },
    Snippet {
        name: "osc to superdirt",
        summary: "Send a pattern to SuperDirt over OSC.",
        description: "`.osc()` sends `/dirt/play` bundles straight to a SuperDirt on this machine, no bridge process; `.oschost(\"…\")` aims it at another.",
        code: "$: s(\"bd sd\").osc()",
        tags: &["external_io", "osc"],
        keywords: &["osc", "superdirt", "supercollider", "tidal", "network"],
    },
    Snippet {
        name: "serial output",
        summary: "Write each event to a serial port",
        description: "`.serial(baud, sendcrc, singlecharids, port)`: the baud rate, whether a CRC follows each frame, whether ids are one character, and the port. The port is single-quoted.",
        code: "$: note(\"60 64\").serial(115200, true, false, '/dev/ttyACM0')",
        tags: &["external_io", "serial"],
        keywords: &["serial", "arduino", "usb", "port", "microcontroller"],
    },
    Snippet {
        name: "set tempo",
        summary: "Set the tempo in beats per minute, four beats a cycle.",
        description: "`setcpm(bpm/4)` is the convention every genre shelf and the status line assume: one cycle is a bar of four. `setcps(1)` is the same thing said in cycles per second.",
        code: "setcpm(120/4)",
        tags: &["tempo"],
        keywords: &["bpm", "setcpm", "setcps", "speed", "cycles"],
    },
    Snippet {
        name: "audio input",
        summary: "Live input: effects, rhythmic gating, and what needs a recording.",
        description: concat!(
            "Choose an input in the device picker's audio in section; the `none` row at the top of that list turns the input off again. `s(\"in\")` plays channel 0; `in:1` selects channel 1. `n` selects the channel, not a pitch. With no input open, the source is silent.\n\n",
            "Effects: gain, pan, filters, distortion, delay and reverb process the live signal. This example filters it and adds reverb and delay.\n\n",
            "Rhythm: `seg` / `segment` creates listening windows. Add `clip(.5)` for gaps; adjacent windows can sound continuous. See the audio input rhythm snippet.\n\n",
            "Requires recorded audio: sample positions (`begin` / `end`), playback speed and reverse audio need a recorded sample. `note` does not retune the input. `chop` can change timing without slicing recorded audio; `rev` changes event order without reversing the waveform."
        ),
        code: "$: s(\"in\").lpf(2000).room(.5).delay(.3)",
        tags: &["external_io"],
        keywords: &[
            "microphone",
            "mic",
            "line in",
            "audio in",
            "input",
            "record",
            "live",
            "in",
            "segment",
            "note",
            "pitch",
            "speed",
            "slice",
            "reverse",
        ],
    },
    Snippet {
        name: "audio input rhythm",
        summary: "Gate live input eight times per cycle, with audible gaps.",
        description: concat!(
            "`seg(8)` creates eight listening windows per cycle. `clip(.5)` shortens each to half its length, leaving a gap before the next. A short attack and release soften the edges.\n\n",
            "This gates the audio arriving now; it does not restart or slice a recording. `seg(8)` alone can sound continuous because the windows touch. Choose an audio input first; `in` is channel 0."
        ),
        code: "$: s(\"in\").seg(8).clip(.5).attack(.005).release(.01).lpf(1200)",
        tags: &["external_io"],
        keywords: &[
            "audio in", "input", "gate", "gating", "rhythm", "seg", "segment", "clip", "legato",
        ],
    },
    Snippet {
        name: "stereo audio input",
        summary: "Play input channels 0 and 1 as a stereo pair.",
        description: concat!(
            "Choose an input with at least two channels. Each input voice is mono: pan channel 0 left and channel 1 right to keep the pair in stereo.\n\n",
            "`in:1` means the second input channel; it is not a sample variant or a note. Apply the same effects after `stack(...)`, or give each channel its own effects."
        ),
        code: "$: stack(s(\"in\").pan(0), s(\"in:1\").pan(1))",
        tags: &["external_io"],
        keywords: &["audio in", "input", "stereo", "channels", "left", "right"],
    },
    Snippet {
        name: "random scrub",
        summary: "Jump to a random slice of a break, eight times a cycle.",
        description: "`scrub` reads a position in the file from 0 to 1, so a random one is a whole number under sixteen divided by sixteen: `irand(16).div(16)` lands on a sixteenth-note grid, and `seg(8)` picks a new one eight times a cycle. Change the two sixteens together for a finer or coarser grid, and the eight on its own for how often it jumps. `fit` stretches the file to the length of the event it lands in, so the loop keeps time whatever the tempo.",
        code: "samples(\'github:yaxu/clean-breaks/main\')\n$: s(\"amen/4\").fit().scrub(irand(16).div(16).seg(8))",
        tags: &["samples", "random"],
        keywords: &[
            "scrub", "slice", "chop", "stutter", "glitch", "tape", "break", "amen", "irand",
            "random", "position", "jump",
        ],
    },
    Snippet {
        name: "slider control",
        summary: "A number you can drag, in the score.",
        description: "`slider(value, min, max, step)` with literal numbers is drawn as a control in the score after evaluating: drag it with the pointer or nudge it with the wheel, and the value changes without a re-evaluation.",
        code: "$: note(\"c3*4\").s(\"sawtooth\").lpf(slider(800, 200, 4000))",
        tags: &["ui"],
        keywords: &["control", "knob", "widget", "drag", "number"],
    },
];
