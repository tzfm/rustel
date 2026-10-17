# Talkit

Speak text. `talkit` is a speech synthesizer built on the parameter model of
the old Talk It! program, with a formant engine written in Rust. It has no GUI
and no external libraries, just text in and sound out.

## In a score

`talkit(text, options)` speaks text, the way `s("...")` plays a sample. The
text is one phrase: its spaces and punctuation are spoken as written, and the
phrase is one event per cycle.

```javascript
$: talkit("hello there").slow(2)
$: talkit("hola mundo", { personality: "robotoid", language: "spanish" })
$: talkit(["question?", "answer."], { effort: "whispered" })
```

An array plays its phrases one after another in the cycle. Any pattern works
as the text too: `talkit(mini("one two three"))` gives one word per event,
and `s("bd hh").talkit("hello")` speaks on the events of another pattern.

Each field of the options object is itself a pattern, so
`quality: "sung monotone"` gives one quality to each event, and
`quality: "<natural sung>"` alternates by cycle. The keys are the parameter
table below, with the short forms `v`, `p`, `s`, `q`, `e` and `l`.

Write a voice name with hyphens: `{ v: "the-fly" }`. An option value is
mini-notation, where a space separates two events, so `"the fly"` asks for a
voice called `the`.

### Melodies

A note sets the pitch the voice starts on:

```javascript
$: note("c3 e3 g3 c4").talkit("la")
$: n("0 2 4 7").scale("C3:major").talkit("do", { v: "choir-boy" })
```

`.freq(220)` sets the pitch in Hz, and `.octave(1)` moves a note or a
frequency up one octave. A note can be from C1 to C7. An explicit `pitch`
option outranks the note.

Start from the notes, as with a synth. `talkit("la")` makes one event per
cycle, so `talkit("la").note("c3 e3 g3")` keeps that one event and speaks
only at C3.

The first syllable sits exactly on the note. After it, the quality decides:
monotone holds the note, natural drifts down and falls or rises at a final
`.` or `?`, and sung walks a major chord up from the note. A word of one
syllable with no final punctuation sounds at the note in all three.

### Controls

The `speed` option is how fast the synthesizer speaks. The hap control
`.speed` stays playback rate, so `.speed(2)` plays the rendering twice as
fast. The other controls apply as to a sample: `.gain`, `.pan`, `.cut`,
`.release` and effects. A synth named by `s` on the same event does not
sound.

Each phrase, voice and pitch is rendered once, the first time an event asks
for it, and then plays from memory. A second of speech renders in about a
millisecond, so the first event sounds. Studio lets go of renderings that no
playing score uses and renders them again when a score asks.

## From the command line

Build the command from a copy of this repository:

```sh
cargo install --path crates/talkit
```

Then:

```sh
talkit hello world
talkit personality=robotoid "I am a robot"
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
| `quality` or `q` | natural, monotone, sung | monotone holds one pitch, sung walks a chord |
| `effort` or `e` | normal, breathy, whispered | whispered drops voicing entirely |
| `language`, `lang` or `l` | english, spanish | letter rules and number words per language |
| `out` or `o` | a `.wav` path | command line only, writes the file instead of playing |

On the command line a personality sets pitch, speed, quality and effort in
one step, so put single overrides after it.

## Voices

```text
male  female  large-male  child  giant-male  mellow-female  mellow-male
crisp-male  the-fly  robotoid  martian  colossus  fast-fred  old-woman
munchkin  troll  nerd  milk-toast  tipsy  choir-boy
```

Names match ignoring case and anything that is not a letter, so `the-fly`,
`TheFly` and, on the command line, `"the fly"` pick the same voice.

Each voice starts from the pitch, speed, quality and effort of its original
preset. Colossus also has a harder buzz, the sound of a machine, and Child a
shorter vocal tract, which raises every formant. Both are measured from the
samples the original engine's makers publish. The other voices share one
timbre.

## Text handling

Punctuation sets the pauses and the pitch at the end of a sentence: a comma
pauses briefly, a period or exclamation falls, a question mark rises. Numbers
are spoken as words: `3.5` becomes `three point five`, `1,500` becomes
`one thousand five hundred` and `-2` becomes `minus two`. In Spanish the same
`3.5` becomes `tres coma cinco`. Accented letters fold to their base letter
before the rules run.

Command line playback uses the default output device and waits until the text
has sounded. A written file is 16-bit mono WAV at 22.05 kHz. The same text and
settings always render the same samples.
