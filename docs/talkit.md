# Talkit

Speak text. `talkit` is a speech synthesizer built on the parameter model of
the old Talk It! program, with a formant engine written in Rust. It has no GUI
and no external libraries, just text in and sound out.

Talkit is experimental.

## In a score

`talkit(text, options)` speaks text, the way `s("...")` plays a sample. The
quotes decide how the text splits into events:

- Single quotes speak one phrase. `talkit('hello there, how are you?')` keeps
  its spaces and punctuation and is one event per cycle.
- Double quotes are mini-notation, as everywhere else.
  `talkit("my name is skrillex")` speaks one word per event.

```javascript
$: talkit('hello there').slow(2)
$: talkit("my name is skrillex")
$: talkit('hola mundo', { personality: "little-robot", language: "spanish" })
$: talkit(['question?', 'answer.'], { effort: "whispered" })
```

In double quotes, `?` and `,` are mini-notation operators, so write
punctuation in single quotes. An array plays its phrases one after another in
the cycle. Any pattern works as the text too: `s("bd hh").talkit('hello')`
speaks on the events of another pattern.

Each field of the options object is itself a pattern, so
`quality: "sung monotone"` gives one quality to each event, and
`quality: "<natural sung>"` alternates by cycle. The keys are the parameter
table below, with the short forms `v`, `p`, `s`, `q`, `e` and `l`.

Write a voice name with hyphens: `{ v: "little-robot" }`. An option value is
mini-notation, where a space separates two events, so `"little robot"` asks
for a voice called `little`.

### Melodies

A note sets the pitch the voice starts on:

```javascript
$: note("c3 e3 g3 c4").talkit('la')
$: n("0 2 4 7").scale("C3:major").talkit('do', { v: "singing-boy", q: "monotone" })
```

`.freq(220)` sets the pitch in Hz, and `.octave(1)` moves a note or a
frequency up one octave. A note can be from C1 to C7. An explicit `pitch`
option outranks the note.

Start from the notes, as with a synth. `talkit('la')` makes one event per
cycle, so `talkit('la').note("c3 e3 g3")` keeps that one event and speaks
only at C3.

The quality decides what the voice does with the note. Monotone holds it.
Sung sings a tune of its own around the note, one step of a scale per
syllable, from a fourth below to a fifth above. The syllable's first
consonant picks the step, the way the original engine sings. Natural moves
like speech: it starts just under the note, a stressed syllable rises above
it, the voice sinks a little after each rise and starts again after a pause,
a final `.` falls and a final `?` rises. The voice also grows softer where
the pitch falls. For a melody that stays on its notes, use
`quality: "monotone"`.

A whispered voice has no pitch: `effort: "whispered"` replaces the voice with
breath, so a note or a `pitch` setting changes nothing. A breathy voice keeps
its pitch.

### Controls

The `speed` option is how fast the synthesizer speaks. The hap control
`.speed` stays playback rate, so `.speed(2)` plays the rendering twice as
fast. The other controls apply as to a sample: `.gain`, `.pan`, `.cut`,
`.release` and effects. A synth named by `s` on the same event does not
sound.

Each phrase, voice and pitch is rendered once, the first time an event asks
for it, and then plays from memory. A second of speech renders in about two
milliseconds, so the first event sounds. Studio lets go of renderings that no
playing score uses and renders them again when a score asks.

## From the command line

Build the command from a copy of this repository:

```sh
cargo install --path crates/talkit
```

Then:

```sh
talkit hello world
talkit personality=little-robot "I am a robot"
talkit lang=spanish "hola mundo"
talkit effort=whispered out=secret.wav "can you keep a secret"
```

Bare words are the text to speak, joined with spaces. A `text=` pair does the
same for text that itself contains an `=` sign.

## Parameters

Command line pairs change one setting each, in the order given. In a score the
same keys are fields of the options object, and a personality applies before
the other keys in any order.

| Key | Values | Notes |
| --- | --- | --- |
| `personality`, `voice` or `v` | see voices below | applies a whole preset |
| `pitch` or `p` | 50 to 500 | the pitch the voice starts on in Hz, default 100 |
| `speed`, `rate` or `s` | 50 to 400 | whole numbers, 150 is nominal, higher is faster |
| `quality` or `q` | natural, monotone, sung | monotone holds the pitch, sung sings a tune around it |
| `effort` or `e` | normal, breathy, whispered | whispered drops voicing entirely, and with it the pitch |
| `language`, `lang` or `l` | english, spanish | letter rules and number words per language |
| `out` or `o` | a `.wav` path | command line only, writes the file instead of playing |

On the command line a personality sets pitch, speed, quality and effort in
one step, so put single overrides after it.

## Voices

The 20 voices of the original program, in its button order, with the
settings each one starts from:

| Voice | Pitch | Speed | Quality | Effort |
|---|---|---|---|---|
| `man` | 100 | 150 | natural | normal |
| `woman` | 200 | 150 | natural | normal |
| `hyper-female` | 190 | 250 | natural | normal |
| `child` | 350 | 130 | natural | normal |
| `strong-man` | 75 | 140 | natural | normal |
| `mellow` | 190 | 140 | natural | breathy |
| `singing-girl` | 310 | 90 | sung | normal |
| `strong-woman` | 200 | 140 | natural | normal |
| `fly` | 480 | 150 | natural | normal |
| `little-robot` | 90 | 150 | monotone | normal |
| `martian` | 80 | 150 | monotone | normal |
| `big-robot` | 66 | 138 | monotone | normal |
| `hyper-male` | 135 | 300 | natural | normal |
| `old-woman` | 270 | 115 | natural | normal |
| `little-man` | 90 | 150 | natural | normal |
| `imaginary-man` | 110 | 200 | natural | normal |
| `nerd` | 140 | 155 | natural | normal |
| `whiner` | 120 | 165 | natural | normal |
| `wobbly` | 145 | 115 | sung | normal |
| `singing-boy` | 310 | 90 | sung | normal |

Names match ignoring case and anything that is not a letter, so
`little-robot`, `LittleRobot` and, on the command line, `"little robot"` pick
the same voice.

Each voice also has a timbre of its own: the shape of its vocal tract, how
long it holds its vowels, and how soft or buzzy its voice is. Big Robot and
Little Robot buzz like machines, Child and Fly have short vocal tracts, and
Singing Boy is soft. The timbres, the sounds, the timing, the intonation and
the English readings are measured from the original engine's output, and the
speech has its sound: 8-bit steps at 11.025 kHz.

## Text handling

Punctuation sets the pauses and the pitch at the end of a sentence: a comma
or a bracket pauses, a period, exclamation, colon or semicolon falls, and a
question mark rises. A short word such as `for` or `to` takes its full form
at the end of a phrase, as in `what is it for?`.

English numbers are read as the original engine reads them. Up to three
digits are a count: `42` is `forty two`. Four digits are a year: `1999` is
`nineteen ninety nine`, and `2005` is `two thousand five`. Five or more
digits are read one by one. Thousands commas make a count with a pause at
each comma: `1,500` is `one thousand, five hundred`. A point reads its digits
one by one: `3.14` is `three point one four`.

Spanish numbers follow the same engine. Up to four digits are a count:
`2024` is `dos mil veinte y cuatro`. Five or more digits are read one by
one, and thousands commas pause as in English. A point reads `y`, the digits
one by one and `decimal`: `3.5` is `tres y cinco decimal`. Accented letters
fold to their base letter before the Spanish rules run.

Command line playback uses the default output device and waits until the text
has sounded. A written file is 16-bit mono WAV at 11.025 kHz. The same text and
settings always render the same samples.
