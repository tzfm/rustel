//! Reference for the native graph surface, kept separate from pattern functions.
//!
//! Names such as `sine`, `range`, and `adsr` mean different things inside `K`.
//! The descriptions and examples here describe Rustel's implementation; upstream
//! graph/library definitions are the compatibility reference, not executable code.

use rustel_core::reference::{ReferenceEntry, ReferenceParam};

macro_rules! param {
    ($name:literal, $description:literal) => {
        ReferenceParam {
            name: $name,
            r#type: "number | Node | Array",
            description: $description,
        }
    };
    ($name:literal, $type:literal, $description:literal) => {
        ReferenceParam {
            name: $name,
            r#type: $type,
            description: $description,
        }
    };
}

macro_rules! entry {
    ($name:literal, [$($alias:literal),*], $tag:literal, $summary:literal,
     $description:literal, [$($param:expr),* $(,)?], $body:literal) => {
        ReferenceEntry {
            name: $name,
            synonyms: &[$($alias),*],
            summary: $summary,
            description: $description,
            params: &[$($param),*],
            examples: &[concat!("note(\"a3\").K(() => { ", $body, " })")],
            tags: &["kabelsalat", $tag],
            origin: "kabelsalat",
            ..ReferenceEntry::blank($name)
        }
    };
}

macro_rules! value {
    ($name:literal, $tag:literal, $summary:literal, $description:literal, $body:literal) => {
        ReferenceEntry {
            tags: &["kabelsalat", $tag, "value"],
            ..entry!($name, [], $tag, $summary, $description, [], $body)
        }
    };
}

macro_rules! unavailable {
    ($name:literal, $summary:literal, $description:literal, [$($param:expr),* $(,)?]) => {
        ReferenceEntry {
            name: $name,
            summary: $summary,
            description: $description,
            params: &[$($param),*],
            tags: &["kabelsalat", "unsupported"],
            no_autocomplete: true,
            origin: "kabelsalat",
            ..ReferenceEntry::blank($name)
        }
    };
}

// Context tags describe where the documented name itself is available. A
// global builder can register node methods without being a node method itself.
macro_rules! global_entry {
    ($name:literal, [$($alias:literal),*], $tag:literal, $summary:literal,
     $description:literal, [$($param:expr),* $(,)?], $body:literal) => {
        ReferenceEntry {
            tags: &["kabelsalat", $tag, "global"],
            ..entry!($name, [$($alias),*], $tag, $summary, $description, [$($param),*], $body)
        }
    };
}

macro_rules! global_value {
    ($name:literal, $tag:literal, $summary:literal, $description:literal, $body:literal) => {
        ReferenceEntry {
            tags: &["kabelsalat", $tag, "global", "value"],
            ..value!($name, $tag, $summary, $description, $body)
        }
    };
}

macro_rules! unavailable_global {
    ($name:literal, $summary:literal, $description:literal, [$($param:expr),* $(,)?]) => {
        ReferenceEntry {
            tags: &["kabelsalat", "unsupported", "global"],
            ..unavailable!($name, $summary, $description, [$($param),*])
        }
    };
}

pub(super) const ENTRIES: &[ReferenceEntry] = &[
    entry!(
        "sine",
        [],
        "oscillators",
        "Sine oscillator at a frequency in Hz.",
        "Produces a signal between -1 and 1. A positive edge at sync resets the oscillator; phase offsets it in cycles. In a chain, the receiver supplies freq: n(sFreq).sine().",
        [
            param!("freq", "Frequency in Hz; default 0."),
            param!("sync", "Reset signal; default 0."),
            param!("phase", "Phase offset in cycles; default 0.")
        ],
        "sine(sFreq).mul(sGate.ad(.01, .3)).out();"
    ),
    entry!(
        "saw",
        [],
        "oscillators",
        "Band-limited rising saw oscillator.",
        "Produces a rising -1 to 1 ramp with a discontinuity correction to reduce aliasing. Use zaw for the uncorrected ramp. The argument, or chained receiver, is a frequency in Hz.",
        [param!("freq", "Frequency in Hz; default 0.")],
        "saw(sFreq).lpf(.45).mul(.15).out();"
    ),
    entry!(
        "zaw",
        [],
        "oscillators",
        "Uncorrected rising saw oscillator.",
        "Produces a -1 to 1 ramp without anti-aliasing. Useful as a slow modulation signal; saw is the band-limited audio-rate alternative.",
        [param!("freq", "Frequency in Hz; default 0.")],
        "sine(sFreq).mul(zaw(2).range(.05, .2)).out();"
    ),
    entry!(
        "tri",
        [],
        "oscillators",
        "Triangle oscillator.",
        "Rises and falls linearly between -1 and 1. The argument is frequency in Hz, not a pattern speed or cycle duration.",
        [param!("freq", "Frequency in Hz; default 0.")],
        "tri(sFreq).mul(.2).out();"
    ),
    entry!(
        "pulse",
        [],
        "oscillators",
        "Pulse oscillator with variable width.",
        "Switches between 1 and -1. A width of .5 produces a square wave; smaller widths shorten the positive part of each cycle.",
        [
            param!("freq", "Frequency in Hz; default 0."),
            param!(
                "pw",
                "Positive fraction of a cycle, normally 0 to 1; default .5."
            )
        ],
        "pulse(sFreq, sine(.5).range(.2, .8)).mul(.1).out();"
    ),
    entry!(
        "impulse",
        [],
        "oscillators",
        "One-sample impulses at a frequency in Hz.",
        "Emits 1 when its phase wraps and 0 otherwise, including an initial impulse. Useful for seq and hold. The accepted phase argument is currently ignored by the native DSP.",
        [
            param!("freq", "Impulses per second; default 0."),
            param!(
                "phase",
                "Compatibility argument; default 0, currently ignored."
            )
        ],
        "impulse(4).seq(220, 330, 440).sine().mul(.15).out();"
    ),
    entry!(
        "clock",
        [],
        "sequencing",
        "Bipolar clock at 24 pulses per beat.",
        "The tempo is beats per minute. Divide by 24 for one clock cycle per beat. This clock runs within each graph voice; setcpm sets the pattern scheduler separately.",
        [param!("bpm", "Beats per minute; default 120.")],
        "clock(120).clockdiv(24).ad(.005, .1).mul(sine(sFreq)).out();"
    ),
    entry!(
        "clockdiv",
        [],
        "sequencing",
        "Divide a bipolar clock's rate.",
        "Counts positive/nonpositive transitions and toggles its output after divisor transitions. With clock(120), divisor 24 yields two clock cycles per second.",
        [
            param!("clock", "Input clock; default 0."),
            param!(
                "divisor",
                "Transition count per output transition; default 2."
            )
        ],
        "clock(120).clockdiv(12).seq(220, 330).sine().mul(.15).out();"
    ),
    entry!(
        "noise",
        [],
        "noise",
        "White noise with an optional update gate.",
        "Draws a new value between -1 and 1 on every sample while next is nonzero. With next zero it holds its previous value. For edge-triggered random steps, use noise().hold(trigger).",
        [param!("next", "Update enable; default 1.")],
        "noise().mul(sGate.ad(.005, .15)).mul(.15).out();"
    ),
    entry!(
        "lcgnoise",
        ["rng"],
        "noise",
        "Repeatable linear-congruential noise generator.",
        "Produces values between -1 and 1. next enables updates; reset restarts the generator from its voice seed before each enabled update. A sustained reset therefore repeats the first value.",
        [
            param!("next", "Update enable; default 1."),
            param!("reset", "Reset enable; default 0.")
        ],
        "lcgnoise().hold(impulse(6)).range(200, 600).sine().mul(.15).out();"
    ),
    entry!(
        "pink",
        [],
        "noise",
        "Pink noise with more low-frequency energy.",
        "Filters white noise into an approximate pink spectrum. There are no arguments. Attenuate before routing it to the output.",
        [],
        "pink().mul(.15).out();"
    ),
    entry!(
        "brown",
        [],
        "noise",
        "Smooth, dark brown noise.",
        "Integrates and damps white noise to emphasize low frequencies. There are no arguments.",
        [],
        "brown().mul(.3).out();"
    ),
    entry!(
        "dust",
        [],
        "noise",
        "Random positive impulses.",
        "Each sample has density/48000 probability of producing a random amplitude from 0 to 1. Density is approximately impulses per second at 48 kHz; the current compatibility formula uses that fixed rate.",
        [param!(
            "density",
            "Expected impulses per second at 48 kHz; default 0."
        )],
        "dust(8).ad(.001, .06).mul(sine(900)).mul(.3).out();"
    ),
    entry!(
        "lfnoise",
        [],
        "noise",
        "Stepped random modulation at a chosen rate.",
        "Samples white noise on impulse(freq), then holds each value. Output is between -1 and 1. Use lag after it to smooth the steps.",
        [param!("freq", "New random values per second.")],
        "lfnoise(4).lag(.1).range(150, 700).sine().mul(.15).out();"
    ),
    entry!(
        "adsr",
        [],
        "envelopes",
        "Gate-controlled attack, decay, sustain and release envelope.",
        "A positive gate starts the attack; a nonpositive gate releases it. Attack, decay and release use seconds; sustain is a level. The receiver in sGate.adsr(...) supplies the gate.",
        [
            param!("gate", "Gate signal; default 0."),
            param!("att", "Attack seconds; default .02."),
            param!("dec", "Decay seconds; default .1."),
            param!("sus", "Sustain level; default .2."),
            param!("rel", "Release seconds; default .1.")
        ],
        "saw(sFreq).mul(sGate.adsr(.01, .2, .3, .2)).mul(.2).out();"
    ),
    entry!(
        "ar",
        [],
        "envelopes",
        "Attack and release envelope with a full sustain level.",
        "Equivalent to gate.adsr(att, 0, 1, rel). The envelope holds at 1 while the gate remains positive and releases when it closes.",
        [
            param!("gate", "Gate signal; default 0."),
            param!("att", "Attack seconds; default .02."),
            param!("rel", "Release seconds; default .1.")
        ],
        "sine(sFreq).mul(sGate.ar(.02, .2)).mul(.2).out();"
    ),
    entry!(
        "ad",
        [],
        "envelopes",
        "Attack and decay envelope with no sustain.",
        "Equivalent to gate.adsr(att, dec, 0, dec). A positive edge starts the envelope; closing the gate uses dec as its release time too.",
        [
            param!("gate", "Trigger or gate; default 0."),
            param!("att", "Attack seconds; default .02."),
            param!("dec", "Decay and release seconds; default .1.")
        ],
        "sine(sFreq).mul(sGate.ad(.005, .25)).mul(.2).out();"
    ),
    entry!(
        "perc",
        [],
        "envelopes",
        "Instant attack with a release tail.",
        "Expands to gate.adsr(0, 0, 1, release). A short impulse yields a percussive envelope; a sustained gate holds the level until it closes.",
        [
            param!("gate", "Gate or impulse node."),
            param!("release", "Release seconds.")
        ],
        "impulse(4).perc(.12).mul(sine(sFreq)).mul(.2).out();"
    ),
    entry!(
        "slide",
        [],
        "smoothing",
        "Smooth a changing signal with a one-pole filter.",
        "Each sample moves 1/max(1, rate*1000) of the remaining distance toward the input. Larger rates move more slowly. This compatibility parameter is not a duration in seconds.",
        [
            param!("input", "Signal to smooth; default 0."),
            param!("rate", "Smoothing factor; default 1.")
        ],
        "impulse(4).seq(220, 440).slide(10).sine().mul(.15).out();"
    ),
    entry!(
        "lag",
        [],
        "smoothing",
        "Smooth a signal with the KabelSalat lag response.",
        "Each sample moves 1/max(1, rate*4410) of the remaining distance. Larger rates make slower changes; the response depends on the sample rate.",
        [
            param!("input", "Signal to smooth; default 0."),
            param!("rate", "Smoothing factor; default 1.")
        ],
        "impulse(4).seq(220, 440).lag(.15).saw().mul(.1).out();"
    ),
    entry!(
        "slew",
        [],
        "smoothing",
        "Limit upward and downward changes independently.",
        "Limits each sample's rise to up/48000 and fall to dn/48000. The controls are signal units per second at 48 kHz, following the upstream fixed-rate formula.",
        [
            param!("input", "Signal to smooth; default 0."),
            param!("up", "Maximum rising rate; default 1."),
            param!("dn", "Maximum falling rate; default 1.")
        ],
        "pulse(2).slew(20, 8).range(220, 440).sine().mul(.15).out();"
    ),
    entry!(
        "filter",
        ["lpf"],
        "filters",
        "Resonant low-pass with a normalized cutoff.",
        "Cuts high frequencies using the KabelSalat two-pole filter. Cutoff is a normalized control, usually 0 to 1, not Hz. Use qlpf for a cutoff specified in Hz.",
        [
            param!("input", "Audio signal; default 0."),
            param!("cutoff", "Normalized cutoff; default 1."),
            param!("reso", "Resonance, normally 0 to 1; default 0.")
        ],
        "saw(sFreq).filter(sine(.5).range(.2, .6), .2).mul(.15).out();"
    ),
    entry!(
        "hpf",
        [],
        "filters",
        "High-pass made by subtracting the low-pass output.",
        "Returns input - input.lpf(cutoff, reso). Cutoff uses the same normalized control as filter, not Hz. For a biquad high-pass in Hz use qhpf.",
        [
            param!("input", "Audio signal."),
            param!("cutoff", "Normalized cutoff."),
            param!("reso", "Resonance; default 0.")
        ],
        "noise().hpf(.6).mul(.08).out();"
    ),
    entry!(
        "bpf",
        [],
        "filters",
        "Band-pass output of the normalized two-pole filter.",
        "Keeps a band around a normalized cutoff. The cutoff is not in Hz; qbpf provides a biquad band-pass with a frequency in Hz.",
        [
            param!("input", "Audio signal; default 0."),
            param!("cutoff", "Normalized cutoff; default 1."),
            param!("reso", "Resonance; default 0.")
        ],
        "noise().bpf(.4, .4).mul(.15).out();"
    ),
    entry!(
        "qf",
        [],
        "filters",
        "Biquad filter selected by a numeric type.",
        "Types: 0 low-pass, 1 high-pass, 2 band-pass, 3 notch, 4 all-pass, 5 peak, 6 low shelf, 7 high shelf. Frequency is in Hz. The effective quality factor is 10^(q/20); gain is in dB for peak and shelf types.",
        [
            param!("input", "Audio signal; default 0."),
            param!("type", "Filter type 0 through 7; default 0."),
            param!("freq", "Frequency in Hz; default 500."),
            param!("q", "Quality control in dB; default 1."),
            param!("gain", "Peak or shelf gain in dB; default 1.")
        ],
        "saw(sFreq).qf(0, 1200, 6).mul(.15).out();"
    ),
    entry!(
        "qlpf",
        [],
        "filters",
        "Biquad low-pass with cutoff in Hz.",
        "Expands to input.qf(0, freq, q). The q control maps to an effective quality factor of 10^(q/20). This differs from filter/lpf's normalized cutoff.",
        [
            param!("input", "Audio node."),
            param!("freq", "Cutoff in Hz."),
            param!("q", "Quality control in dB; default 10.")
        ],
        "saw(sFreq).qlpf(1200, 3).mul(.1).out();"
    ),
    entry!(
        "qhpf",
        [],
        "filters",
        "Biquad high-pass with cutoff in Hz.",
        "Expands to input.qf(1, freq, q). Higher q values increase the resonance near the cutoff.",
        [
            param!("input", "Audio node."),
            param!("freq", "Cutoff in Hz."),
            param!("q", "Quality control in dB; default 10.")
        ],
        "noise().qhpf(3000, 3).mul(.08).out();"
    ),
    entry!(
        "qbpf",
        [],
        "filters",
        "Biquad band-pass with center frequency in Hz.",
        "Expands to input.qf(2, freq, q). Use it to select a frequency band from a broadband source.",
        [
            param!("input", "Audio node."),
            param!("freq", "Center frequency in Hz."),
            param!("q", "Quality control in dB; default 10.")
        ],
        "noise().qbpf(800, 6).mul(.1).out();"
    ),
    entry!(
        "qnf",
        [],
        "filters",
        "Biquad notch filter.",
        "Expands to input.qf(3, freq, q), suppressing a band around freq. q controls the width through an effective quality factor of 10^(q/20).",
        [
            param!("input", "Audio node."),
            param!("freq", "Notch frequency in Hz."),
            param!("q", "Quality control in dB; default 10.")
        ],
        "saw(sFreq).qnf(900, 6).mul(.1).out();"
    ),
    entry!(
        "qapf",
        [],
        "filters",
        "Compatibility helper that currently selects the notch type.",
        "Like the upstream library, qapf expands to qf type 3, so it behaves as a notch filter despite its name. For an all-pass filter use input.qf(4, freq, q).",
        [
            param!("input", "Audio node."),
            param!("freq", "Notch frequency in Hz."),
            param!("q", "Quality control in dB; default 10.")
        ],
        "saw(sFreq).qapf(900, 6).mul(.1).out();"
    ),
    entry!(
        "distort",
        [],
        "effects",
        "Apply a saturating distortion curve.",
        "A rational waveshaper whose amount is clamped between 0 and 1. Stronger settings flatten peaks. Reduce output gain when increasing the amount.",
        [
            param!("input", "Signal; default 0."),
            param!("amt", "Distortion amount 0 to 1; default 0.")
        ],
        "sine(sFreq).distort(.6).mul(.15).out();"
    ),
    entry!(
        "fold",
        [],
        "effects",
        "Fold a signal back into a bipolar range.",
        "Increases input drive by rate+1 and reflects the waveform at its bounds. Negative rates act as zero. A changing rate adds harmonics without simply clipping the peaks.",
        [
            param!("input", "Signal; default 0."),
            param!("rate", "Extra drive; default 0.")
        ],
        "sine(sFreq).fold(sine(.5).range(0, 6)).mul(.15).out();"
    ),
    entry!(
        "delay",
        [],
        "effects",
        "Delay a signal by a time in seconds.",
        "Uses a native per-voice delay buffer with interpolated reads. Feedback can be built with a callback edge. Persistent named delay buffers are unavailable.",
        [
            param!("input", "Signal; default 0."),
            param!(
                "time",
                "Delay in seconds; default 0, bounded by native delay storage."
            )
        ],
        "const dry = sine(sFreq).mul(sGate.ad(.005, .08)); dry.add(dry.delay(.15).mul(.5)).mul(.2).out();"
    ),
    entry!(
        "seq",
        [],
        "sequencing",
        "Advance through values on positive trigger edges.",
        "The trigger is the first argument, or receiver in a chain. Each positive edge advances the step and emits zero for that sample; between edges the selected step is passed through. Supply steps as separate arguments to avoid multichannel expansion.",
        [
            param!("trig", "Trigger signal; default 0."),
            param!("...steps", "Values or signal nodes to cycle through.")
        ],
        "impulse(6).seq(220, 330, 440).lag(.02).sine().mul(.15).out();"
    ),
    entry!(
        "hold",
        [],
        "sequencing",
        "Sample a signal on positive trigger edges.",
        "Captures input when trig crosses from nonpositive to positive, then holds that sample until the next edge. Before the first trigger its output is zero.",
        [
            param!("input", "Signal to sample; default 0."),
            param!("trig", "Capture trigger; default 0.")
        ],
        "noise().hold(impulse(4)).range(220, 660).sine().mul(.15).out();"
    ),
    entry!(
        "pick",
        [],
        "sequencing",
        "Select a signal by a wrapping numeric index.",
        "Wraps the index into the input count and floors it, including negative indices. Selection happens per sample; unlike channel(), this accepts an index signal.",
        [
            param!("index", "Selection index."),
            param!("...inputs", "Signals to choose from.")
        ],
        "pick(sine(1).range(0, 3), 220, 330, 440).sine().mul(.15).out();"
    ),
    entry!(
        "trig",
        [],
        "sequencing",
        "Turn positive crossings into one-sample triggers.",
        "Returns 1 on a nonpositive-to-positive crossing, and 0 otherwise. The lo and hi arguments are accepted for compatibility but do not change the current native threshold of zero.",
        [
            param!("input", "Signal to detect; default 0."),
            param!(
                "lo",
                "Compatibility argument; default -1, currently ignored."
            ),
            param!(
                "hi",
                "Compatibility argument; default 1, currently ignored."
            )
        ],
        "sine(4).trig().ad(.001, .1).mul(sine(660)).mul(.2).out();"
    ),
    entry!(
        "add",
        [],
        "math",
        "Add signals sample by sample.",
        "Sums its inputs, or adds arguments to the receiver in a chain. A callback receives the node being built, allowing a one-sample feedback edge. Arrays expand into channels.",
        [param!("...inputs", "Signals to add; no inputs gives 0.")],
        "sine(sFreq).add(sine(sFreq * 1.5)).mul(.1).out();"
    ),
    entry!(
        "mul",
        [],
        "math",
        "Multiply signals sample by sample.",
        "Multiplies inputs from left to right. Use it for amplitude envelopes, ring modulation and frequency scaling. A callback receives the node being built for a feedback edge.",
        [param!("...inputs", "Factors; no inputs gives 0.")],
        "sine(sFreq).mul(sGate.ad(.01, .25), .2).out();"
    ),
    entry!(
        "sub",
        [],
        "math",
        "Subtract each following signal from the first.",
        "Processes inputs from left to right. In a chain the receiver is the minuend. A single input passes through unchanged.",
        [param!(
            "...inputs",
            "Initial value followed by values to subtract; no inputs gives 0."
        )],
        "sine(sFreq).sub(sine(sFreq * 2).mul(.3)).mul(.15).out();"
    ),
    entry!(
        "div",
        [],
        "math",
        "Divide a signal by each following input.",
        "Processes division from left to right. Avoid divisors that cross zero: division does not clamp or protect the denominator.",
        [param!(
            "...inputs",
            "Numerator followed by divisors; no inputs gives 0."
        )],
        "sine(sFreq).div(6).out();"
    ),
    entry!(
        "mod",
        [],
        "math",
        "Take the signed remainder of division.",
        "Uses the remainder operation, so a negative dividend keeps a negative remainder. This is different from wrapping into an always-positive interval.",
        [
            param!("input", "Dividend; default 0."),
            param!(
                "divisor",
                "Divisor; required for a finite result (omitted input is 0)."
            )
        ],
        "sine(sFreq).mul(3).mod(1).mul(.15).out();"
    ),
    entry!(
        "pow",
        [],
        "math",
        "Raise a signal to a power.",
        "Computes input raised to power for each sample. Fractional powers require nonnegative inputs to keep the result finite; use unipolar() for a bipolar oscillator.",
        [
            param!("input", "Base; default 0."),
            param!("power", "Exponent; default 1.")
        ],
        "sine(sFreq).mul(sine(2).unipolar().pow(3)).mul(.2).out();"
    ),
    entry!(
        "log",
        [],
        "math",
        "Take the natural logarithm of a signal.",
        "Computes the logarithm to base e. Inputs must be positive for a finite result. Pair with exp() when shaping frequency ratios in logarithmic space.",
        [param!("input", "Positive input; default 0.")],
        "n(sFreq).log().exp().sine().mul(.2).out();"
    ),
    entry!(
        "exp",
        [],
        "math",
        "Raise e to the input signal.",
        "Computes the natural exponential per sample. Large positive inputs overflow; use it with bounded modulation or logarithmic frequency values.",
        [param!("input", "Exponent; default 0.")],
        "sine(1).mul(.1).exp().mul(sFreq).sine().mul(.2).out();"
    ),
    entry!(
        "sin",
        [],
        "math",
        "Take the sine of an angle signal in radians.",
        "This is a stateless mathematical function. Use sine(freq) for an oscillator whose argument is in Hz.",
        [param!("input", "Angle in radians; default 0.")],
        "sine(sFreq).mul(PI).sin().mul(.15).out();"
    ),
    entry!(
        "cos",
        [],
        "math",
        "Take the cosine of an angle signal in radians.",
        "This stateless function transforms the current input sample into its cosine; it does not advance an oscillator phase.",
        [param!("input", "Angle in radians; default 0.")],
        "sine(sFreq).mul(PI).cos().mul(.15).out();"
    ),
    entry!(
        "tan",
        [],
        "math",
        "Take the tangent of an angle signal in radians.",
        "The tangent grows without bound near odd multiples of pi/2. Keep the argument inside a safe interval when using it as a waveshaper.",
        [param!("input", "Angle in radians; default 0.")],
        "sine(sFreq).mul(.6).tan().mul(.15).out();"
    ),
    entry!(
        "asin",
        [],
        "math",
        "Take the inverse sine in radians.",
        "Maps inputs from -1 through 1 to angles from -pi/2 through pi/2. Inputs outside that domain produce a non-finite result.",
        [param!("input", "Signal in [-1, 1]; default 0.")],
        "sine(sFreq).asin().mul(.1).out();"
    ),
    entry!(
        "acos",
        [],
        "math",
        "Take the inverse cosine in radians.",
        "Maps inputs from -1 through 1 to angles from pi through 0. Subtract pi/2 to center the output for audio use.",
        [param!("input", "Signal in [-1, 1]; default 0.")],
        "sine(sFreq).acos().sub(PI.div(2)).mul(.1).out();"
    ),
    entry!(
        "atan",
        [],
        "math",
        "Take the inverse tangent in radians.",
        "Maps the whole real line into (-pi/2, pi/2). Applied to an amplified waveform it gives a smooth saturating waveshaper.",
        [param!("input", "Signal; default 0.")],
        "sine(sFreq).mul(3).atan().mul(.1).out();"
    ),
    entry!(
        "abs",
        [],
        "math",
        "Take the absolute value of each sample.",
        "Reflects negative samples above zero. Applied to an oscillator it creates full-wave rectification and doubles the repeating waveform's frequency.",
        [param!("input", "Signal; default 0.")],
        "sine(sFreq).abs().sub(.5).mul(.2).out();"
    ),
    entry!(
        "round",
        [],
        "math",
        "Round each sample to the nearest integer.",
        "Halfway values round toward positive infinity, matching the score language. Scale before and after rounding to quantize a waveform into steps.",
        [param!("input", "Signal; default 0.")],
        "sine(sFreq).mul(4).round().div(24).out();"
    ),
    entry!(
        "floor",
        [],
        "math",
        "Round each sample down to an integer.",
        "Always rounds toward negative infinity, including negative values. Useful for stepped control signals and waveform quantization.",
        [param!("input", "Signal; default 0.")],
        "sine(sFreq).mul(4).floor().div(24).out();"
    ),
    entry!(
        "ceil",
        [],
        "math",
        "Round each sample up to an integer.",
        "Always rounds toward positive infinity, including negative values. Unlike round(), any fractional part moves to the next integer.",
        [param!("input", "Signal; default 0.")],
        "sine(sFreq).mul(4).ceil().div(24).out();"
    ),
    entry!(
        "sign",
        [],
        "math",
        "Return the sign of each sample.",
        "Returns -1 for negative input, 1 for positive input and zero for zero. Applied to a smooth oscillator it creates a hard bipolar waveform.",
        [param!("input", "Signal; default 0.")],
        "sine(sFreq).sign().mul(.1).out();"
    ),
    entry!(
        "min",
        [],
        "math",
        "Choose the smallest input sample.",
        "Compares all input signals per sample. In a chain the receiver participates in the comparison. One input passes through unchanged.",
        [param!(
            "...inputs",
            "Signals to compare; no inputs gives 0."
        )],
        "sine(sFreq).min(.4).mul(.2).out();"
    ),
    entry!(
        "max",
        [],
        "math",
        "Choose the largest input sample.",
        "Compares all input signals per sample. Use min() and max() together to set explicit waveform bounds.",
        [param!(
            "...inputs",
            "Signals to compare; no inputs gives 0."
        )],
        "sine(sFreq).max(-.4).mul(.2).out();"
    ),
    entry!(
        "argmin",
        [],
        "math",
        "Return the index of the smallest input sample.",
        "Returns a zero-based input position instead of its value. Equal minima choose the last matching input. Use the result with pick() to select another signal.",
        [param!("...inputs", "Signals to compare.")],
        "argmin(sine(1), sine(1.5)).pick(220, 330).sine().mul(.15).out();"
    ),
    entry!(
        "argmax",
        [],
        "math",
        "Return the index of the largest input sample.",
        "Returns a zero-based input position instead of its value. Equal maxima choose the last matching input.",
        [param!("...inputs", "Signals to compare.")],
        "argmax(sine(1), sine(1.5)).pick(220, 330).sine().mul(.15).out();"
    ),
    entry!(
        "greater",
        ["gt"],
        "logic",
        "Compare whether the first signal exceeds the second.",
        "Produces a Boolean comparison result per sample. Multiplication converts it to a 0/1 gate. ifelse() tests for numeric 1, so convert comparisons through arithmetic before using it there.",
        [
            param!("a", "First signal; default 0."),
            param!("b", "Second signal; default 0.")
        ],
        "sine(sFreq).mul(sine(2).greater(0)).mul(.2).out();"
    ),
    entry!(
        "lower",
        ["lt"],
        "logic",
        "Compare whether the first signal is below the second.",
        "Produces a Boolean comparison result per sample. Multiply it by 1 to turn it into a numeric gate for strict numeric consumers such as ifelse().",
        [
            param!("a", "First signal; default 0."),
            param!("b", "Second signal; default 0.")
        ],
        "sine(sFreq).mul(sine(2).lower(.5)).mul(.2).out();"
    ),
    entry!(
        "and",
        [],
        "logic",
        "Return 1 when both signals are truthy.",
        "Returns a numeric 0 or 1. Zero and non-finite NaN samples are false; nonzero numeric samples are true.",
        [
            param!("a", "First gate; default 0."),
            param!("b", "Second gate; default 0.")
        ],
        "pulse(2).unipolar().and(pulse(3).unipolar()).mul(sine(sFreq)).mul(.2).out();"
    ),
    entry!(
        "or",
        [],
        "logic",
        "Return 1 when either signal is truthy.",
        "Returns numeric 0 or 1 for the truthiness of its two inputs. Use unipolar pulse gates when a zero-valued low phase is needed.",
        [
            param!("a", "First gate; default 0."),
            param!("b", "Second gate; default 0.")
        ],
        "pulse(2).unipolar().or(pulse(3).unipolar()).mul(sine(sFreq)).mul(.2).out();"
    ),
    entry!(
        "xor",
        [],
        "logic",
        "Return 1 when the two sample values differ.",
        "Compares the numeric values for inequality; it is not a bitwise XOR. For 0/1 gates this gives the usual exclusive-or truth table.",
        [
            param!("a", "First value; default 0."),
            param!("b", "Second value; default 0.")
        ],
        "pulse(2).unipolar().xor(pulse(3).unipolar()).mul(sine(sFreq)).mul(.2).out();"
    ),
    entry!(
        "not",
        [],
        "logic",
        "Return 1 when the input is numeric zero.",
        "Tests strict equality with numeric zero, returning 0 otherwise. Boolean comparison results are distinct from numeric zero; convert them through arithmetic first.",
        [param!("input", "Numeric gate; default 0.")],
        "pulse(8).unipolar().not().mul(sine(sFreq)).mul(.2).out();"
    ),
    entry!(
        "bool",
        [],
        "logic",
        "Convert a numeric nonzero test to 0 or 1.",
        "Tests strict inequality with numeric zero. Boolean comparison results are distinct from numeric zero and therefore yield 1, even when false. For comparisons, use arithmetic conversion instead.",
        [param!("input", "Numeric signal; default 0.")],
        "sine(2).round().bool().mul(sine(sFreq)).mul(.2).out();"
    ),
    entry!(
        "ifelse",
        [],
        "logic",
        "Choose between signals when the condition is numeric 1.",
        "Selects then when the condition is exactly the number 1, and otherwise selects else. Boolean comparison nodes need arithmetic conversion, for example greater(0).mul(1).",
        [
            param!("condition", "Strict numeric 1/other selector; default 0."),
            param!("then", "Signal selected for 1; default 0."),
            param!("else", "Signal selected otherwise; default 0.")
        ],
        "sine(2).greater(0).mul(1).ifelse(220, 330).sine().mul(.15).out();"
    ),
    entry!(
        "clamp",
        [],
        "mapping",
        "Limit a signal between two bounds.",
        "Sorts the lower and upper bounds before clamping, so crossing modulation bounds remain usable. For ordered bounds without sorting, use clip().",
        [
            param!("input", "Signal; default 0."),
            param!("min", "First bound; default -1."),
            param!("max", "Second bound; default 1.")
        ],
        "sine(sFreq).mul(3).clamp(-1, 1).mul(.1).out();"
    ),
    entry!(
        "clip",
        [],
        "mapping",
        "Clip a signal to ordered lower and upper bounds.",
        "Applies the supplied minimum and maximum in order. Unlike clamp(), it does not sort bounds that cross.",
        [
            param!("input", "Signal."),
            param!("min", "Lower bound; default -1."),
            param!("max", "Upper bound; default 1.")
        ],
        "sine(sFreq).clip(-.3, .3).mul(.4).out();"
    ),
    entry!(
        "range",
        [],
        "mapping",
        "Map a bipolar signal into a new interval.",
        "Maps -1 to min and 1 to max. The optional curve raises the normalized input to a power before scaling. Inputs outside [-1,1] extrapolate; range does not clamp.",
        [
            param!("input", "Bipolar source."),
            param!("min", "Value at -1."),
            param!("max", "Value at 1."),
            param!("curve", "Power curve; default 1.")
        ],
        "sine(1).range(220, 660, 2).sine().mul(.15).out();"
    ),
    entry!(
        "rangex",
        [],
        "mapping",
        "Map a bipolar signal exponentially between positive bounds.",
        "Interpolates in logarithmic space, making frequency sweeps perceptually even. Both endpoints must be positive; inputs outside [-1,1] extrapolate.",
        [
            param!("input", "Bipolar source."),
            param!("min", "Positive value at -1."),
            param!("max", "Positive value at 1.")
        ],
        "sine(.5).rangex(110, 880).sine().mul(.15).out();"
    ),
    entry!(
        "remap",
        [],
        "mapping",
        "Map between explicit input and output intervals.",
        "Linearly rescales input from [inMin,inMax] to [outMin,outMax] without clamping. The input interval must have nonzero width.",
        [
            param!("input", "Source; default 0."),
            param!("inMin", "Input minimum; default -1."),
            param!("inMax", "Input maximum; default 1."),
            param!("outMin", "Output minimum; default -1."),
            param!("outMax", "Output maximum; default 1.")
        ],
        "sine(1).unipolar().remap(0, 1, 220, 440).sine().mul(.15).out();"
    ),
    entry!(
        "bipolar",
        [],
        "mapping",
        "Map the interval [0,1] to [-1,1].",
        "Computes input*2-1 without clamping. Use this to center unipolar envelopes or control values around zero.",
        [param!("input", "Unipolar source.")],
        "sine(2).unipolar().bipolar().mul(10).add(sFreq).sine().mul(.15).out();"
    ),
    entry!(
        "unipolar",
        [],
        "mapping",
        "Map the interval [-1,1] to [0,1].",
        "Computes (input+1)/2 without clamping. Useful for turning bipolar oscillators into amplitude controls and numeric 0/1 pulse gates.",
        [param!("input", "Bipolar source.")],
        "sine(sFreq).mul(sine(2).unipolar()).mul(.2).out();"
    ),
    entry!(
        "midinote",
        [],
        "mapping",
        "Convert a MIDI note number into frequency in Hz.",
        "Uses equal temperament with MIDI note 69 at 440 Hz. Fractional notes are supported, so the input can be a continuous pitch signal.",
        [param!("input", "MIDI note number.")],
        "sine(.5).range(57, 69).midinote().sine().mul(.15).out();"
    ),
    entry!(
        "time",
        [],
        "expressions",
        "Read elapsed native voice time in seconds.",
        "Returns the elapsed time supplied to the graph by its native voice. The optional source annotation is retained as graph metadata; it does not replace the native clock.",
        [param!(
            "code",
            "string",
            "Optional upstream source annotation; ignored by the native clock."
        )],
        "time().mul(2 * Math.PI * sFreq).sin().mul(.15).out();"
    ),
    entry!(
        "raw",
        [],
        "expressions",
        "Evaluate a bounded arithmetic expression natively.",
        "The code string accepts arithmetic, comparisons, bitwise operators, conditionals and supported Math functions. t and $input name the input; time is voice time in seconds. Statements, arbitrary function calls and runtime code generation are unavailable. Source is limited to 8192 bytes.",
        [
            param!("input", "Expression input."),
            param!(
                "code",
                "string",
                "One arithmetic expression compiled by the Rust engine."
            )
        ],
        "sine(sFreq).raw('Math.atan(t * 3)').mul(.1).out();"
    ),
    entry!(
        "bytebeat",
        [],
        "expressions",
        "Evaluate an arithmetic bytebeat and scale its low byte.",
        "Evaluates the same bounded native expression language as raw(), wraps the result to an 8-bit value and scales it into [-1,1]. Supply a sample counter, commonly time().mul(8000), as t.",
        [
            param!("input", "Counter available as t or $input."),
            param!(
                "code",
                "string",
                "Bytebeat arithmetic expression; at most 8192 bytes."
            )
        ],
        "time().mul(8000).bytebeat('(t * 5) & (t >> 7)').mul(.15).out();"
    ),
    entry!(
        "floatbeat",
        [],
        "expressions",
        "Build the upstream-compatible bytebeat form.",
        "The current compatible builder emits a bytebeat node, including its 8-bit output scaling. Use raw() when the expression should produce an unscaled floating-point audio sample.",
        [
            param!("input", "Counter available as t or $input."),
            param!(
                "code",
                "string",
                "Arithmetic expression; at most 8192 bytes."
            )
        ],
        "time().mul(8000).floatbeat('(t * 3) & (t >> 8)').mul(.15).out();"
    ),
    entry!(
        "audioin",
        [],
        "routing",
        "Read the native audio input signal.",
        "Reads the mono input supplied to this graph by the Rust engine. In a K() effect it receives the preceding audio signal; with a live input route it can process external audio. It reads silence when no input signal is supplied.",
        [],
        "audioin().lpf(.5).mul(.5).out();"
    ),
    entry!(
        "output",
        [],
        "routing",
        "Create an explicit output-bus node.",
        "Adds its input to a numbered native output bus when the node is reachable from an out() root. The output node itself emits zero. Use out() for ordinary stereo output and root registration.",
        [
            param!("input", "Signal to send; default 0."),
            param!("id", "Output channel, 0 through 15; default 0.")
        ],
        "output(sine(sFreq).mul(.15), 0).out();"
    ),
    entry!(
        "src",
        [],
        "routing",
        "Read a native output bus in the current sample.",
        "Reads the selected bus's accumulated signal. Evaluation order matters: the producing output node must run first. This is a graph-local bus, not a sample-bank or external source lookup.",
        [param!("id", "Output channel, 0 through 15; default 0.")],
        "output(sine(sFreq).mul(.1), 0).out(); src(0).out(1);"
    ),
    entry!(
        "out",
        [],
        "routing",
        "Connect a signal to output channels and register a graph root.",
        "Routes the signal to each requested channel and returns the original signal for chaining. With no channel argument, mono is copied to channels 0 and 1; polyphonic channel arrays expand across them.",
        [
            param!("input", "Signal or channel array."),
            param!(
                "channels",
                "number | Array",
                "Output channel or channel list; default [0,1], supported indices 0 through 15."
            )
        ],
        "sine(sFreq).pan(sine(.5)).mul(.15).out();"
    ),
    entry!(
        "pan",
        [],
        "routing",
        "Position a mono signal across two channels.",
        "Uses equal-power cosine/sine gains: -1 is left, 0 is center, 1 is right. Returns a two-channel poly node. Positions outside this range are not clamped.",
        [
            param!("input", "Mono signal."),
            param!("position", "Pan position from -1 to 1.")
        ],
        "sine(sFreq).pan(sine(.5)).mul(.15).out();"
    ),
    entry!(
        "poly",
        [],
        "channels",
        "Construct a multichannel graph value.",
        "Collects signals into explicit channels. Most graph operations expand across channels and wrap shorter input arrays, so each channel retains its own oscillator and envelope state.",
        [param!("...inputs", "Channel signals.")],
        "poly(sine(sFreq), sine(sFreq * 1.005)).mul(.15).out();"
    ),
    entry!(
        "fork",
        [],
        "channels",
        "Make shallow copies of a node.",
        "Clones the receiver's own node and shares its upstream inputs. Each clone has independent DSP state. It does not recursively duplicate the entire graph. Returns a poly node; count is floored and bounded at 1024.",
        [
            param!("input", "Node to copy."),
            param!(
                "count",
                "Number of copies; default 1, nonpositive gives no channels."
            )
        ],
        "sine(sFreq).fork(2).mul(.15).out();"
    ),
    entry!(
        "split",
        [],
        "channels",
        "Pass the channel array to a graph-building callback.",
        "Calls the callback once with an array of channels, using a one-element array for mono input. Return the desired recombination, such as poly(channels[1],channels[0]).",
        [
            param!("input", "Mono or poly node."),
            param!(
                "callback",
                "function",
                "Receives the array of channel nodes and returns a graph value."
            )
        ],
        "sine([sFreq, sFreq * 1.5]).split(xs => poly(xs[1], xs[0])).mul(.15).out();"
    ),
    entry!(
        "mix",
        [],
        "channels",
        "Mix channels to mono or an evenly panned stereo pair.",
        "A channel count of 1 sums all poly channels. Any other supplied count requests stereo, with channels evenly panned left to right. Mono input passes through. Summing does not normalize the amplitude.",
        [
            param!("input", "Mono or poly signal."),
            param!("channels", "1 for mono, otherwise stereo; default 1.")
        ],
        "sine([sFreq, sFreq * 1.5, sFreq * 2]).mix(2).mul(.07).out();"
    ),
    entry!(
        "thru",
        [],
        "graph",
        "Pass a signal through unchanged.",
        "Creates an identity graph node. It is useful as a named point in a custom graph builder without changing the audio signal.",
        [param!("input", "Signal to pass through; required.")],
        "sine(sFreq).thru().mul(.15).out();"
    ),
    entry!(
        "scope",
        [],
        "graph",
        "Preserve a scope marker while passing audio through.",
        "Retains scope metadata in the graph and returns the input signal. The native editor does not currently display an upstream KabelSalat oscilloscope for this marker.",
        [
            param!("input", "Signal to mark."),
            param!("id", "string | number", "Optional marker identifier.")
        ],
        "sine(sFreq).scope('voice').mul(.15).out();"
    ),
    entry!(
        "n",
        ["B", "_"],
        "graph",
        "Wrap a constant or array as a graph value.",
        "Numbers become constant nodes, arrays become multichannel values, and existing nodes pass through. This lets event numbers such as sFreq use graph methods: n(sFreq).sine().",
        [param!("input", "Constant, existing node or array.")],
        "n(sFreq).sine().mul(.15).out();"
    ),
    global_entry!(
        "Node",
        ["node"],
        "graph",
        "Construct a low-level graph node with optional metadata.",
        "Creates a node of the given type without connecting inputs. Use getNode() for input coercion and multichannel expansion. Node.prototype is event-local and can define custom graph methods; this does not add a new native DSP opcode.",
        [
            param!("type", "string", "Native graph node type."),
            param!("value", "any", "Optional node value metadata.")
        ],
        "new Node('sine').withIns(n(sFreq)).mul(.15).out();"
    ),
    global_entry!(
        "getNode",
        [],
        "graph",
        "Construct a graph operation from its type and inputs.",
        "Coerces constants to nodes, expands channel arrays and resolves callback inputs as feedback connections. The type must be understood by the native DSP compiler when the node reaches an output.",
        [
            param!("type", "string", "Native node type."),
            param!("...inputs", "Inputs in the node type's inlet order.")
        ],
        "getNode('sine', sFreq).mul(.15).out();"
    ),
    entry!(
        "exit",
        [],
        "graph",
        "Group graph roots in a container node.",
        "Creates an exit node containing its inputs. This alone does not register output roots in K(); reachable audio still needs an out() call. Use it when inspecting or compiling a group of nodes.",
        [param!("...inputs", "Nodes to group.")],
        "const roots = exit(sine(sFreq), sine(sFreq * 1.5)); compile(roots); sine(sFreq).mul(.15).out();"
    ),
    entry!(
        "feedback",
        [],
        "graph",
        "Build a one-sample feedback connection.",
        "The callback receives the feedback node and returns its input graph. Cycles read their previously computed sample. As a method, feedback(callback) adds the returned feedback graph to the receiver.",
        [param!(
            "callback",
            "function",
            "Receives the feedback node and returns a graph node."
        )],
        "feedback(previous => sine(sFreq).add(previous.mul(.2))).mul(.1).out();"
    ),
    global_entry!(
        "register",
        [],
        "graph",
        "Register an event-local graph builder as a node method.",
        "Returns the supplied builder and installs its name and underscore bypass on Node.prototype for this graph only. In method form the receiver becomes the builder's first argument. With no builder, constructs the named native node type.",
        [
            param!("name", "string", "Method name."),
            param!(
                "builder",
                "function",
                "Optional function receiving the graph inputs."
            ),
            param!("schema", "object", "Optional inlet and display metadata.")
        ],
        "const quiet = register('quiet', input => input.mul(.1)); sine(sFreq).quiet().out();"
    ),
    global_entry!(
        "module",
        [],
        "graph",
        "Register a graph builder with module boundary metadata.",
        "Like register(), but converts arguments to nodes and annotates module inputs and output. Registration lasts for this K() graph only. The returned function can be called directly or through the installed node method.",
        [
            param!("name", "string", "Module and method name."),
            param!("builder", "function", "Builder receiving node inputs."),
            param!("schema", "object", "Optional inlet metadata.")
        ],
        "const voice = module('voice', freq => freq.sine().mul(.15)); voice(sFreq).out();"
    ),
    global_entry!(
        "registerNode",
        [],
        "graph",
        "Register a constructor and schema for a native node type.",
        "Returns a constructor that uses getNode() and installs a same-named node method. Schemas describe inlets; they do not implement DSP. Unknown types are rejected when the native program compiles.",
        [
            param!("name", "string", "Existing native node type."),
            param!(
                "schema",
                "object",
                "Metadata such as {ins:[{name:'freq'}]}."
            )
        ],
        "const oscillator = registerNode('sine', {ins:[{name:'freq'}]}); oscillator(sFreq).mul(.15).out();"
    ),
    global_entry!(
        "getInletName",
        [],
        "graph",
        "Read an inlet name from a registered node schema.",
        "Looks up schema.ins[index].name. Returns an empty string when no inlet schema exists; an inlet without a name can return undefined. This is construction-time metadata, not an audio signal.",
        [
            param!("type", "string", "Registered node type."),
            param!("index", "number", "Zero-based inlet index.")
        ],
        "registerNode('sine', {ins:[{name:'freq'}]}); const inlet = getInletName('sine', 0); sine(sFreq).mul(.15).out();"
    ),
    global_entry!(
        "compile",
        [],
        "graph",
        "Serialize a graph into native KabelSalat IR.",
        "Returns a JSON string describing the native graph. It does not emit or execute JavaScript DSP. Calling compile() does not replace the K() graph's out() roots.",
        [param!("input", "Graph root to serialize.")],
        "const voice = sine(sFreq).mul(.15); const ir = compile(voice); voice.out();"
    ),
    global_entry!(
        "S",
        [],
        "inputs",
        "Lift a pattern into a numeric KabelSalat event input.",
        "Inside a K() callback, the score transpiler evaluates this pattern outside the graph and substitutes its numeric event value. Use it to pattern graph parameters. pat exposes the lifted values by index. This special form is not a runtime graph constructor.",
        [param!(
            "pattern",
            "Pattern | string | number",
            "Pattern producing numeric values for the graph."
        )],
        "sine(S(\"220 330 440\")).mul(.15).out();"
    ),
    global_entry!(
        "m",
        [],
        "inputs",
        "Lift a mini-notation string into a graph event input.",
        "This transpiler-facing helper parses mini notation before graph construction. A bare m() call with a string literal is lifted into pat. The source offset is required; use -1 for a manual call without editor source locations. S(pattern) is the usual public form for lifting a pattern.",
        [
            param!(
                "mini",
                "string literal",
                "Mini-notation containing numeric values."
            ),
            param!(
                "offset",
                "number",
                "Required source offset; -1 disables source locations."
            )
        ],
        "sine(m('220 330 440', -1)).mul(.15).out();"
    ),
    global_value!(
        "sFreq",
        "inputs",
        "The current event's frequency in Hz.",
        "A number, not a Node. It follows the event's note or freq control and can participate in ordinary arithmetic. Pass it to a graph constructor, or wrap it with n(sFreq) before chaining graph methods.",
        "sine(sFreq).mul(.15).out();"
    ),
    global_value!(
        "sGate",
        "inputs",
        "The current native voice's gate node.",
        "A graph signal that stays high while the event is held and falls when it is released. Feed it into adsr(), ar() or ad() to shape this event's amplitude.",
        "sine(sFreq).mul(sGate.adsr(.01, .1, .5, .2)).mul(.2).out();"
    ),
    global_value!(
        "pat",
        "inputs",
        "Numeric inputs lifted from patterns in this K() callback.",
        "An array of event numbers in the order extracted by the score transpiler. S(pattern) and m('mini', -1) references are replaced with pat[index]. Values are numbers, so wrap them with n() before using graph methods.",
        "const pitch = S(\"220 330\"); sine(pat[0]).mul(.15).out();"
    ),
    global_value!(
        "PI",
        "math",
        "A constant graph node with the value pi.",
        "Unlike Math.PI, PI is already a Node and supports graph methods. Use Math.PI when ordinary construction-time arithmetic needs a numeric value.",
        "time().mul(sFreq).mul(PI.mul(2)).sin().mul(.15).out();"
    ),
    global_value!(
        "polyType",
        "graph",
        "The multichannel node type name, 'poly'.",
        "A string used when inspecting node.type. Audio channel construction normally uses poly(...signals).",
        "const voice = sine([sFreq, sFreq * 1.5]); if (voice.type === polyType) voice.mul(.1).out();"
    ),
    global_value!(
        "outputType",
        "graph",
        "The root-container node type name, 'exit'.",
        "A string used when inspecting graph containers. It names the exit node type, not the individual output-bus node type.",
        "const roots = exit(sine(sFreq)); if (roots.type === outputType) sine(sFreq).mul(.1).out();"
    ),
    global_value!(
        "Node.prototype",
        "graph",
        "The event-local prototype shared by graph nodes.",
        "Assign methods here to extend graph-building notation for this K() evaluation. A regular function receives the graph node as this; arrow functions do not bind this. Changes are discarded after the graph is compiled.",
        "Node.prototype.quiet = function () { return this.mul(.1); }; sine(sFreq).quiet().out();"
    ),
    global_entry!(
        "Node.parseInput",
        [],
        "graph",
        "Coerce a graph input or resolve a feedback callback.",
        "Returns existing nodes, wraps primitive values, and converts arrays to poly nodes. When input is a function it is called with node, which is the feedback target supplied by the caller.",
        [
            param!("input", "any", "Value or callback to convert."),
            param!(
                "node",
                "Node",
                "Feedback target passed to a callback input."
            )
        ],
        "Node.parseInput(sFreq).sine().mul(.15).out();"
    ),
    global_value!(
        "nodeRegistry",
        "graph",
        "Event-local node schemas with get() and set() accessors.",
        "Stores construction metadata used by getInletName() and registered graph modules. It is an accessor object, not a complete JavaScript Map, and adding a schema does not implement a native DSP operation.",
        "nodeRegistry.set('sine', {ins:[{name:'freq'}]}); sine(sFreq).mul(.15).out();"
    ),
    global_entry!(
        "nodeRegistry.get",
        [],
        "graph",
        "Read a node schema by name.",
        "Returns the schema registered in this K() evaluation, or undefined if none exists. Schema lookup runs only while building the graph.",
        [param!("name", "string", "Node or module name.")],
        "nodeRegistry.set('sine', {ins:[{name:'freq'}]}); const schema = nodeRegistry.get('sine'); sine(sFreq).mul(.15).out();"
    ),
    global_entry!(
        "nodeRegistry.set",
        [],
        "graph",
        "Store a node schema for this graph evaluation.",
        "Associates the name with a metadata value and returns nodeRegistry. This neither installs a constructor nor adds native DSP support; use registerNode() when a constructor is also needed.",
        [
            param!("name", "string", "Schema name, at most 128 bytes."),
            param!(
                "schema",
                "any",
                "Construction metadata, usually an object with ins."
            )
        ],
        "nodeRegistry.set('sine', {ins:[{name:'freq'}]}); sine(sFreq).mul(.15).out();"
    ),
    entry!(
        "withIns",
        [],
        "method",
        "Replace a node's inputs in place.",
        "Accepts separate node arguments and mutates the receiver, returning it. Inputs are not automatically coerced; wrap numbers with n(). Use getNode() when input conversion and channel expansion are wanted.",
        [param!("...inputs", "Node", "Replacement input nodes.")],
        "node('sine').withIns(n(sFreq)).mul(.15).out();"
    ),
    entry!(
        "inherit",
        [],
        "method",
        "Copy module boundary metadata from another node.",
        "Copies inputOf and outputOf annotations onto the receiver and returns it. It does not copy node type, value or audio inputs.",
        [param!(
            "parent",
            "Node",
            "Node whose module annotations should be inherited."
        )],
        "const voice = sine(sFreq); voice.mul(.15).inherit(voice).out();"
    ),
    entry!(
        "clone",
        [],
        "method",
        "Make a shallow copy of a graph node.",
        "Copies type, value and the input list while keeping upstream nodes shared. The copied node receives its own native DSP state. Use fork() to create several copies at once.",
        [],
        "const voice = sine(sFreq); poly(voice, voice.clone()).mul(.1).out();"
    ),
    entry!(
        "apply",
        [],
        "method",
        "Call a graph builder with this node.",
        "Passes the receiver as the callback's sole argument and returns the callback result. It is useful for extracting a reusable piece of graph notation.",
        [param!("callback", "function", "Called as callback(node).")],
        "sine(sFreq).apply(x => x.mul(.15)).out();"
    ),
    entry!(
        "apply2",
        [],
        "method",
        "Call a graph builder with this node twice.",
        "Calls callback(node,node) and returns its result. For example envelope.apply2(mul) squares an envelope while keeping the original envelope state shared.",
        [param!(
            "callback",
            "function",
            "Called as callback(node,node)."
        )],
        "sine(sFreq).mul(sGate.ad(.01, .3).apply2(mul)).mul(.2).out();"
    ),
    entry!(
        "map",
        [],
        "method",
        "Transform each channel of a graph value.",
        "For poly input, calls callback(channel,index,channels) for each channel and returns a poly result. Mono input calls callback(node) directly.",
        [param!(
            "callback",
            "function",
            "Channel transformation returning a graph value."
        )],
        "sine([sFreq, sFreq * 1.5]).map((x, i) => x.mul(i === 0 ? .15 : .1)).out();"
    ),
    entry!(
        "channel",
        [],
        "method",
        "Select a graph channel while building the graph.",
        "Mono input returns itself. Poly input selects index modulo channel count; use a nonnegative integer. The index is a construction-time number, not a signal; use pick() for audio-rate selection.",
        [param!("index", "number", "Nonnegative channel index.")],
        "sine([sFreq, sFreq * 2]).channel(0).mul(.15).out();"
    ),
    entry!(
        "select",
        [],
        "method",
        "Find the first upstream node with a given type.",
        "Searches inputs depth first, excluding the receiver itself, and returns the first match or undefined. Useful for graph inspection; the returned node can be shared by multiple outputs.",
        [param!("type", "string", "Node type to search for.")],
        "const voice = sine(sFreq).mul(.15); const oscillator = voice.select('sine'); voice.out();"
    ),
    entry!(
        "over",
        [],
        "method",
        "Add a transformed copy of a signal to itself.",
        "Calls callback(receiver) and adds its result to the original receiver. Both branches share the original graph and its DSP state.",
        [param!(
            "callback",
            "function",
            "Returns the parallel signal to add."
        )],
        "sine(sFreq).over(x => x.distort(.5).mul(.3)).mul(.1).out();"
    ),
    entry!(
        "debug",
        [],
        "method",
        "Inspect a node with a callback and keep chaining.",
        "Calls the optional callback with the receiver and then returns the receiver. With no callback this is a pass-through; it does not automatically print or open a graph view.",
        [param!(
            "callback",
            "function",
            "Optional construction-time inspection callback."
        )],
        "sine(sFreq).debug(x => { const type = x.type; }).mul(.15).out();"
    ),
    entry!(
        "dfs",
        [],
        "method",
        "Traverse and optionally rewrite a graph depth first.",
        "Calls the visitor for each encountered node, uses its returned node as the replacement, then visits and rewrites that node's inputs. A visited list prevents repeated traversal through cycles. Return the node when inspection should leave it intact.",
        [
            param!(
                "visitor",
                "function",
                "Receives a node and returns its replacement."
            ),
            param!(
                "visited",
                "Array",
                "Optional visited-node array; default []."
            )
        ],
        "sine(sFreq).dfs(x => x).mul(.15).out();"
    ),
    entry!(
        "flatten",
        [],
        "method",
        "Describe a graph as a root-first node array.",
        "Returns objects with type, optional value, and ins containing string indices into the array. This inspection representation supports shared nodes and cycles; compile() returns the native program serialization.",
        [],
        "const voice = sine(sFreq).mul(.15); const nodes = voice.flatten(); voice.out();"
    ),
    entry!(
        "toObject",
        [],
        "method",
        "Copy an acyclic graph into plain objects.",
        "Returns a JSON-compatible nested graph description. Cyclic feedback graphs cannot be represented by this conversion and are rejected; use flatten() or compile() for cycles.",
        [],
        "const voice = sine(sFreq).mul(.15); const description = voice.toObject(); voice.out();"
    ),
    entry!(
        "stringify",
        [],
        "method",
        "Format an acyclic graph as readable source-like text.",
        "Returns a formatted representation using single-quoted strings. The result is for inspection, not strict JSON. Cyclic feedback graphs are rejected.",
        [],
        "const voice = sine(sFreq).mul(.15); const description = voice.stringify(); voice.out();"
    ),
    entry!(
        "asModuleInput",
        [],
        "method",
        "Append a module-input annotation to a node.",
        "Adds [name,id,index] to inputOf and returns the receiver. These annotations describe graph construction boundaries and do not change audio processing.",
        [
            param!("name", "string", "Module name."),
            param!("id", "number", "Module instance identifier."),
            param!("index", "number", "Input position.")
        ],
        "n(sFreq).asModuleInput('voice', 0, 0).sine().mul(.15).out();"
    ),
    entry!(
        "asModuleOutput",
        [],
        "method",
        "Set a module-output annotation on a node.",
        "Sets outputOf to [name,id] and returns the receiver. This metadata does not register an audio output; call out() to make the graph audible.",
        [
            param!("name", "string", "Module name."),
            param!("id", "number", "Module instance identifier.")
        ],
        "sine(sFreq).asModuleOutput('voice', 0).mul(.15).out();"
    ),
    value!(
        "type",
        "method",
        "The graph node's type name.",
        "A string such as 'sine', 'n' or 'poly'. Read it during graph construction to inspect structure. Changing it changes the native operation and requires compatible inputs.",
        "const voice = sine(sFreq); if (voice.type === 'sine') voice.mul(.15).out();"
    ),
    value!(
        "value",
        "method",
        "Optional node-specific value metadata.",
        "Constant nodes store their numeric value here; raw and bytebeat nodes store expression text. Other node types may leave it undefined. This is construction data, not the node's current audio sample.",
        "const frequency = n(sFreq); sine(frequency.value).mul(.15).out();"
    ),
    value!(
        "ins",
        "method",
        "The node's array of upstream inputs.",
        "Contains graph nodes connected to the receiver. A poly node stores its channel nodes here. This array describes the graph; it does not contain rendered sample values.",
        "const voice = sine([sFreq, sFreq * 1.5]); voice.ins[0].mul(.15).out();"
    ),
    value!(
        "inputOf",
        "method",
        "Module-input annotations attached to a node.",
        "Optional array of [name,id,index] tuples identifying module inputs. asModuleInput() appends entries, and inherit() copies this metadata from another node.",
        "const freq = n(sFreq).asModuleInput('voice', 0, 0); const ports = freq.inputOf; freq.sine().mul(.15).out();"
    ),
    value!(
        "outputOf",
        "method",
        "The module-output annotation attached to a node.",
        "Optional [name,id] tuple set by asModuleOutput(). It describes a module boundary, not an audio output channel.",
        "const voice = sine(sFreq).asModuleOutput('voice', 0); const port = voice.outputOf; voice.mul(.15).out();"
    ),
    unavailable!(
        "midifreq",
        "Upstream MIDI frequency input is unavailable in native K().",
        "The name is recognized for compatibility, but a graph using this node is rejected by the native compiler. Use the event's sFreq value to follow note or freq patterns.",
        [param!(
            "channel",
            "MIDI channel selector; upstream default -1."
        )]
    ),
    unavailable!(
        "midigate",
        "Upstream MIDI gate input is unavailable in native K().",
        "The name is recognized, but the native graph compiler rejects this input node. Use sGate to follow the current pattern event's gate.",
        [param!(
            "channel",
            "MIDI channel selector; upstream default -1."
        )]
    ),
    unavailable!(
        "midivel",
        "Upstream MIDI velocity input is unavailable in native K().",
        "The name is recognized, but the native graph compiler rejects this input node. Pass a numeric pattern through S() to control graph amplitude.",
        [param!(
            "channel",
            "MIDI channel selector; upstream default -1."
        )]
    ),
    unavailable!(
        "midicc",
        "Upstream MIDI controller input is unavailable in native K().",
        "This builder is recognized for source compatibility but its node is rejected by the native DSP compiler. Patterned control values can enter the graph with S().",
        [
            param!("ccnumber", "Controller number; upstream default -1."),
            param!("channel", "MIDI channel; upstream default -1."),
            param!(
                "initValue",
                "Initial controller value; upstream default -1."
            )
        ]
    ),
    unavailable!(
        "cc",
        "Upstream external control input is unavailable in native K().",
        "The graph name exists, but no native control bus is attached to it and the DSP compiler rejects the node. Use S() for event values or a graph oscillator for modulation.",
        [
            param!(
                "id",
                "string | number",
                "Control identifier; upstream default 0."
            ),
            param!("value", "Initial value; upstream default 0.")
        ]
    ),
    ReferenceEntry {
        tags: &["kabelsalat", "unsupported", "global", "value"],
        ..unavailable!(
            "mouseX",
            "Upstream mouse X input is unavailable in native K().",
            "This value is an upstream cc input node. Native K() graphs reject it because the editor does not attach a pointer control bus.",
            []
        )
    },
    ReferenceEntry {
        tags: &["kabelsalat", "unsupported", "global", "value"],
        ..unavailable!(
            "mouseY",
            "Upstream mouse Y input is unavailable in native K().",
            "This value is an upstream cc input node. Native K() graphs reject it because the editor does not attach a pointer control bus.",
            []
        )
    },
    unavailable_global!(
        "evaluate",
        "Dynamic upstream graph evaluation is unavailable.",
        "K() builds and compiles its graph through the Rust engine. Calling the upstream evaluate API is rejected; connect graph output with out() instead.",
        [param!("input", "Graph to evaluate.")]
    ),
    unavailable_global!(
        "exportModule",
        "Upstream JavaScript module export is unavailable.",
        "Native K() does not generate executable JavaScript modules. Use compile() to inspect native graph IR, and ordinary graph-building functions for reuse.",
        [param!("input", "Graph to export.")]
    ),
    unavailable_global!(
        "registerUgen",
        "Custom JavaScript DSP unit registration is unavailable.",
        "Native K() runs Rust DSP operations. Registering executable JavaScript DSP is rejected. register() and module() can compose supported native nodes into reusable graph builders.",
        [
            param!("name", "string", "Requested unit name."),
            param!("implementation", "any", "Upstream unit implementation.")
        ]
    ),
];

// Every automatically installed underscore method bypasses its operation. The
// examples deliberately call the bypass with no arguments: all arguments are
// ignored by the method, though score-language argument expressions still run.
macro_rules! bypasses {
    ($($name:literal),* $(,)?) => { &[$(ReferenceEntry {
        name: concat!("_", $name),
        summary: concat!("Bypass the ", $name, " graph operation."),
        description: concat!("Returns the receiver unchanged and ignores its arguments. Use ._", $name, "() to disable a graph operation while keeping its place in a chain. Argument expressions still evaluate during graph construction."),
        params: &[param!("...ignored", "any", "Optional arguments are ignored.")],
        examples: &[concat!("note(\"a3\").K(() => { sine(sFreq)._", $name, "().mul(.15).out(); })")],
        tags: &["kabelsalat", "method", "bypass"],
        origin: "kabelsalat",
        ..ReferenceEntry::blank(concat!("_", $name))
    }),*] };
}

pub(super) const BYPASSES: &[ReferenceEntry] = bypasses![
    "n",
    "B",
    "_",
    "poly",
    "exit",
    "out",
    "feedback",
    "time",
    "raw",
    "bytebeat",
    "floatbeat",
    "adsr",
    "ar",
    "ad",
    "clock",
    "clockdiv",
    "distort",
    "noise",
    "rng",
    "lcgnoise",
    "pink",
    "brown",
    "dust",
    "impulse",
    "saw",
    "zaw",
    "sine",
    "tri",
    "pulse",
    "slide",
    "lag",
    "slew",
    "filter",
    "fold",
    "seq",
    "delay",
    "hold",
    "midifreq",
    "midigate",
    "midivel",
    "midicc",
    "cc",
    "audioin",
    "log",
    "exp",
    "pow",
    "sin",
    "cos",
    "tan",
    "acos",
    "asin",
    "atan",
    "mul",
    "add",
    "div",
    "sub",
    "mod",
    "abs",
    "round",
    "clamp",
    "floor",
    "ceil",
    "sign",
    "min",
    "max",
    "argmin",
    "argmax",
    "greater",
    "lower",
    "gt",
    "lt",
    "xor",
    "and",
    "or",
    "not",
    "bool",
    "ifelse",
    "range",
    "remap",
    "thru",
    "rangex",
    "midinote",
    "src",
    "output",
    "fork",
    "perc",
    "hpf",
    "lpf",
    "bpf",
    "lfnoise",
    "bipolar",
    "unipolar",
    "pan",
    "pick",
    "clip",
    "trig",
    "qf",
    "qlpf",
    "qhpf",
    "qbpf",
    "qnf",
    "qapf",
    "split",
    "mix",
    "scope",
];

#[cfg(test)]
mod tests {
    use super::super::{GraphCompiler, graph};
    use super::*;
    use crate::{JsRuntime, Slot, with_ctx};
    use rustel_core::Value;
    use rustel_fraction::Fraction;
    use rustel_transpiler::TranspileOptions;
    use std::{collections::BTreeSet, rc::Rc};

    #[test]
    fn catalog_matches_installed_globals_members_and_properties() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime.install_semantic_bindings().expect("bindings");
        let (mut globals, methods, values) = with_ctx(&runtime.ctx, |ctx| {
            let state = Rc::new(graph::State::new(&ctx).expect("graph state"));
            ctx.userdata::<GraphCompiler>()
                .expect("graph compiler")
                .active
                .borrow_mut()
                .push(state);
            let scope = graph::scope(&ctx, &[220.0], 220.0).expect("graph scope");
            let mut globals: BTreeSet<String> =
                scope.iter().map(|(name, _)| name.clone()).collect();
            let mut values: BTreeSet<String> = scope
                .iter()
                .filter(|(_, value)| !value.is_function())
                .map(|(name, _)| name.clone())
                .collect();
            let node = scope
                .iter()
                .find(|(name, _)| name == "Node")
                .unwrap()
                .1
                .as_object()
                .unwrap();
            let prototype: rquickjs::Object = node.get("prototype").expect("node prototype");
            let mut methods: BTreeSet<String> = prototype
                .keys::<String>()
                .collect::<rquickjs::Result<_>>()
                .expect("installed node methods");
            // Include static accessors by reading their actual object keys too.
            for owner in ["Node", "nodeRegistry"] {
                let object = scope
                    .iter()
                    .find(|(name, _)| name == owner)
                    .unwrap()
                    .1
                    .as_object()
                    .unwrap();
                for name in object.keys::<String>() {
                    let name = name.expect("static property");
                    if matches!(name.as_str(), "length" | "name") {
                        continue;
                    }
                    let qualified = format!("{owner}.{name}");
                    let value: rquickjs::Value = object.get(name).expect("static value");
                    if !value.is_function() {
                        values.insert(qualified.clone());
                    }
                    globals.insert(qualified);
                }
            }
            let node = graph::call(&ctx, "n", vec![graph::number(&ctx, 220.0)]).expect("constant");
            let node = graph::call(&ctx, "asModuleInput", vec![node]).expect("input metadata");
            let node = graph::call(&ctx, "asModuleOutput", vec![node]).expect("output metadata");
            for name in node.as_object().unwrap().keys::<String>() {
                let name = name.expect("node property");
                values.insert(name.clone());
                methods.insert(name);
            }
            ctx.userdata::<GraphCompiler>()
                .unwrap()
                .active
                .borrow_mut()
                .pop();
            (globals, methods, values)
        });
        // These two forms are extracted by the score transpiler, before graph
        // construction. The example test below exercises that complete path.
        globals.extend(["S".to_owned(), "m".to_owned()]);
        let installed: BTreeSet<_> = globals.union(&methods).cloned().collect();
        let mut documented = BTreeSet::new();
        for entry in ENTRIES.iter().chain(BYPASSES.iter()) {
            assert!(entry.tags.contains(&"kabelsalat"), "{}", entry.name);
            assert!(
                !entry.summary.is_empty() && !entry.description.is_empty(),
                "{}",
                entry.name
            );
            assert_eq!(
                entry.no_autocomplete,
                entry.tags.contains(&"unsupported"),
                "{}",
                entry.name
            );
            assert!(
                entry.no_autocomplete || !entry.examples.is_empty(),
                "{} has no example",
                entry.name
            );
            for name in std::iter::once(entry.name).chain(entry.synonyms.iter().copied()) {
                assert!(
                    documented.insert(name.to_owned()),
                    "duplicate graph name: {name}"
                );
                assert_eq!(
                    entry.tags.contains(&"global"),
                    globals.contains(name) && !methods.contains(name),
                    "global context: {name}"
                );
                assert_eq!(
                    entry.tags.contains(&"method"),
                    methods.contains(name) && !globals.contains(name),
                    "member context: {name}"
                );
                assert_eq!(
                    entry.tags.contains(&"value"),
                    values.contains(name),
                    "value context: {name}"
                );
            }
        }
        assert_eq!(
            documented, installed,
            "graph reference and native installation differ"
        );
    }

    #[test]
    fn every_supported_example_compiles_and_renders_native_audio() {
        for entry in ENTRIES
            .iter()
            .chain(BYPASSES.iter())
            .filter(|entry| !entry.no_autocomplete)
        {
            for source in entry.examples {
                let runtime = JsRuntime::new().expect("runtime");
                runtime.install_semantic_bindings().expect("bindings");
                runtime
                    .evaluate_score(source, &TranspileOptions::default())
                    .unwrap_or_else(|error| panic!("{}: {source}: {error}", entry.name));
                let haps = runtime
                    .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("{}: {source}: {error}", entry.name));
                let mut rendered = 0;
                for hap in haps {
                    let Some(graph) = hap.value.get("workletGraph").and_then(Value::as_str) else {
                        continue;
                    };
                    let program = rustel_kabel::Program::from_json(graph)
                        .unwrap_or_else(|error| panic!("{}: {source}: {error}", entry.name));
                    let mut state =
                        rustel_kabel::Graph::new(&program, 48_000.0).expect("graph state");
                    let mut peak = 0.0_f64;
                    for frame in 0..8192 {
                        for sample in state.process([0.25, 0.5], 1.0, f64::from(frame) / 48_000.0) {
                            assert!(
                                sample.is_finite(),
                                "nonfinite output for {}: {source}",
                                entry.name
                            );
                            peak = peak.max(sample.abs());
                        }
                    }
                    assert!(peak > 0.0001, "silent example for {}: {source}", entry.name);
                    rendered += 1;
                }
                assert!(
                    rendered > 0,
                    "example contains no native graph: {}: {source}",
                    entry.name
                );
            }
        }
    }
}
