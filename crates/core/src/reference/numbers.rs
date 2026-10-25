//! The numbers each documented parameter takes, as a slider offers them.
//!
//! A reference parameter says `number`, which is true and says little: a
//! count of segments, a speed factor, a probability and a cutoff are all
//! numbers. A slider needs the travel a player reaches for and the step
//! between two values that differ, so `segment` moves through whole
//! segments and never lands on 15.99. Kept outside [`ReferenceParam`] for
//! the same reason the choices are: the entries stay compact, and every
//! reader - the studio's knobs, rendered documentation - reads one table.
//!
//! The travel is a best effort for most scores, not the engine's limit. A
//! score that holds a number outside it is still right, and a slider widens
//! to take it.
//!
//! This table covers the core's own entries. An extension declares the
//! numbers of its entries in its own descriptor, with the same builders.
//!
//! [`ReferenceParam`]: super::ReferenceParam

/// The travel and the step of one parameter of one reference entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReferenceNumber {
    pub entry: &'static str,
    pub parameter: &'static str,
    pub min: f64,
    pub max: f64,
    /// The smallest move that means something. A count steps by 1.
    pub step: f64,
}

impl ReferenceNumber {
    /// Whether the travel is one a slider can move along: its ends in
    /// order, a step that fits, and both ends on the step's grid, so a
    /// whole number on the travel is a stop a slider can land on.
    pub fn is_travel(&self) -> bool {
        let on_grid = |value: f64, step: f64| {
            let steps = value / step;
            (steps - steps.round()).abs() < 1e-9
        };
        self.min < self.max
            && self.step > 0.0
            && self.step <= self.max - self.min
            && on_grid(self.min, self.step)
            && on_grid(self.max - self.min, self.step)
            && (self.step >= 1.0 || on_grid(1.0, self.step))
    }
}

/// A whole number from `min` to `max`.
pub const fn count(
    entry: &'static str,
    parameter: &'static str,
    min: f64,
    max: f64,
) -> ReferenceNumber {
    ReferenceNumber {
        entry,
        parameter,
        min,
        max,
        step: 1.0,
    }
}

/// A number from `min` to `max` in steps of `step`.
pub const fn number(
    entry: &'static str,
    parameter: &'static str,
    min: f64,
    max: f64,
    step: f64,
) -> ReferenceNumber {
    ReferenceNumber {
        entry,
        parameter,
        min,
        max,
        step,
    }
}

/// A share of something, from 0 to 1: a probability, a mix, a position.
pub const fn share(entry: &'static str, parameter: &'static str) -> ReferenceNumber {
    number(entry, parameter, 0.0, 1.0, 0.01)
}

/// A speed factor: quarters from a quarter to eight times, so `fast(1.5)`
/// is on the rail and `fast(1.37)` is not.
const fn factor(entry: &'static str, parameter: &'static str) -> ReferenceNumber {
    number(entry, parameter, 0.25, 8.0, 0.25)
}

/// A place in a cycle, on sixteenths: `1/8`, `1/4` and `3/16` are on the
/// rail.
const fn cycle_place(entry: &'static str, parameter: &'static str) -> ReferenceNumber {
    number(entry, parameter, 0.0, 1.0, 0.0625)
}

/// A time in seconds, fine at the short end where an envelope is heard.
const fn seconds(entry: &'static str, parameter: &'static str) -> ReferenceNumber {
    number(entry, parameter, 0.0, 2.0, 0.001)
}

/// A frequency in hertz, across what the ear hears.
const fn hertz(entry: &'static str, parameter: &'static str) -> ReferenceNumber {
    number(entry, parameter, 20.0, 20000.0, 1.0)
}

pub const REFERENCE_NUMBERS: &[ReferenceNumber] = &[
    // Counts: a fractional segment, slice or repeat is not what a player
    // means, so these move in whole numbers.
    count("segment", "segments", 1.0, 64.0),
    count("ply", "factor", 1.0, 16.0),
    count("plyWith", "factor", 1.0, 16.0),
    count("plyForEach", "factor", 1.0, 16.0),
    count("iter", "n", 1.0, 16.0),
    count("iterBack", "n", 1.0, 16.0),
    count("chunk", "n", 1.0, 16.0),
    count("chunkBack", "n", 1.0, 16.0),
    count("fastChunk", "n", 1.0, 16.0),
    count("repeatCycles", "count", 1.0, 16.0),
    count("chop", "n", 1.0, 64.0),
    count("striate", "n", 1.0, 64.0),
    count("slice", "pieces", 1.0, 64.0),
    count("bite", "number", 1.0, 16.0),
    count("shuffle", "n", 1.0, 16.0),
    count("scramble", "n", 1.0, 16.0),
    count("firstOf", "n", 1.0, 16.0),
    count("lastOf", "n", 1.0, 16.0),
    count("swing", "subdivision", 1.0, 16.0),
    count("swingBy", "subdivision", 1.0, 16.0),
    count("echo", "times", 1.0, 16.0),
    count("stut", "times", 1.0, 16.0),
    count("echoWith", "times", 1.0, 16.0),
    count("applyN", "n", 0.0, 16.0),
    count("beat", "t", 0.0, 15.0),
    count("beat", "div", 1.0, 32.0),
    count("pace", "steps", 1.0, 32.0),
    count("take", "amount", -16.0, 16.0),
    count("drop", "amount", -16.0, 16.0),
    count("shrink", "amount", -16.0, 16.0),
    count("grow", "amount", -16.0, 16.0),
    count("expand", "factor", 1.0, 8.0),
    count("extend", "factor", 1.0, 8.0),
    count("replicate", "factor", 1.0, 8.0),
    count("contract", "factor", 1.0, 8.0),
    count("gap", "steps", 1.0, 16.0),
    count("irand", "n", 1.0, 16.0),
    count("randL", "n", 1.0, 16.0),
    count("seed", "n", 0.0, 100.0),
    // Euclidean rhythms: pulses, steps and rotations are whole, and no
    // pulses is silence. Negative pulses and steps have meanings of their
    // own, which a score that writes one widens the travel to reach.
    count("euclid", "pulses", 0.0, 16.0),
    count("euclid", "steps", 1.0, 32.0),
    count("euclidRot", "pulses", 0.0, 16.0),
    count("euclidRot", "steps", 1.0, 32.0),
    count("euclidRot", "rotation", 0.0, 16.0),
    count("euclidLegato", "pulses", 0.0, 16.0),
    count("euclidLegato", "steps", 1.0, 32.0),
    count("euclidLegatoRot", "pulses", 0.0, 16.0),
    count("euclidLegatoRot", "steps", 1.0, 32.0),
    count("euclidLegatoRot", "rotation", 0.0, 16.0),
    count("euclidish", "pulses", 0.0, 16.0),
    count("euclidish", "steps", 1.0, 32.0),
    share("euclidish", "groove"),
    // Speed: quarters, as a player counts them.
    factor("fast", "factor"),
    factor("slow", "factor"),
    factor("hurry", "factor"),
    factor("fastGap", "factor"),
    factor("loopAtCps", "factor"),
    // inside and outside slow and speed the pattern up, as slow and fast do.
    factor("inside", "factor"),
    factor("outside", "factor"),
    number("loopAt", "factor", 0.25, 16.0, 0.25),
    number("ribbon", "offset", 0.0, 64.0, 0.25),
    number("ribbon", "cycles", 0.25, 16.0, 0.25),
    // Tempo.
    count("cpm", "cpm", 30.0, 240.0),
    count("setCpm", "cpm", 30.0, 240.0),
    number("setCps", "cps", 0.1, 2.0, 0.01),
    number("cps", "value", 0.1, 2.0, 0.01),
    number("loopAtCps", "cps", 0.1, 2.0, 0.01),
    // Places and spans in a cycle.
    cycle_place("off", "time"),
    cycle_place("echo", "time"),
    cycle_place("stut", "time"),
    cycle_place("echoWith", "time"),
    cycle_place("compress", "begin"),
    cycle_place("compress", "end"),
    cycle_place("focus", "begin"),
    cycle_place("focus", "end"),
    cycle_place("zoom", "start"),
    cycle_place("zoom", "end"),
    cycle_place("within", "start"),
    cycle_place("within", "end"),
    number("linger", "fraction", -1.0, 1.0, 0.0625),
    // A nudge in time is heard in hundredths of a cycle.
    share("early", "cycles"),
    share("late", "cycles"),
    share("pressBy", "r"),
    share("swingBy", "amount"),
    // Chances and amounts from 0 to 1.
    share("degradeBy", "amount"),
    share("undegradeBy", "amount"),
    share("degradeByWith", "amount"),
    share("sometimesBy", "probability"),
    share("someCyclesBy", "probability"),
    share("brandBy", "probability"),
    share("juxBy", "by"),
    share("juxFlipBy", "by"),
    share("echo", "feedback"),
    share("stut", "feedback"),
    // Pitch.
    count("transpose", "amount", -24.0, 24.0),
    // An octave is an offset from 0: each one up doubles the frequency, and
    // each one down halves it.
    count("octave", "octave", -4.0, 4.0),
    count("penv", "semitones", -24.0, 24.0),
    // A scale degree below 0 wraps down the scale.
    count("n", "value", -16.0, 32.0),
    // MIDI and devices.
    count("control", "MIDI", 0.0, 127.0),
    count("gamepad", "index", 0.0, 3.0),
    count("setMaxPolyphony", "Max", 1.0, 256.0),
    count("binaryN", "nBits", 1.0, 32.0),
    count("binaryNL", "nBits", 1.0, 32.0),
    // Envelopes.
    seconds("adsr", "time"),
    share("adsr", "gain"),
    seconds("ad", "value"),
    seconds("ar", "value"),
    seconds("attack", "attack"),
    seconds("decay", "time"),
    seconds("release", "time"),
    share("sustain", "gain"),
    seconds("lpattack", "attack"),
    seconds("lpdecay", "decay"),
    seconds("lprelease", "release"),
    share("lpsustain", "sustain"),
    seconds("pattack", "time"),
    seconds("pdecay", "time"),
    seconds("prelease", "time"),
    number("lpenv", "modulation", -8.0, 8.0, 0.1),
    number("hpenv", "modulation", -8.0, 8.0, 0.1),
    number("bpenv", "modulation", -8.0, 8.0, 0.1),
    // Filters. lpf and hpf are "audible between 0 and 20000", freq
    // "between 20 and 20000 Hz", and bpf is a frequency the same way. The
    // floor of 20 lets a slider travel on the ear's own scale.
    hertz("lpf", "frequency"),
    hertz("hpf", "frequency"),
    hertz("bpf", "frequency"),
    hertz("freq", "frequency"),
    // roomlp(0) skips the damping, which is brighter than any cutoff: an
    // off value, not the dark end of the travel.
    hertz("roomlp", "frequency"),
    // "resonance factor between 0 and 50"
    number("lpq", "q", 0.0, 50.0, 0.1),
    number("hpq", "q", 0.0, 50.0, 0.1),
    number("bpq", "q", 0.0, 50.0, 0.1),
    number("lprate", "rate", 0.0, 16.0, 0.1),
    // Levels and mixes.
    number("gain", "amount", 0.0, 2.0, 0.01),
    number("postgain", "amount", 0.0, 2.0, 0.01),
    share("velocity", "velocity"),
    share("pan", "pan"),
    share("shape", "distortion"),
    share("spread", "spread"),
    share("noise", "wet"),
    share("dry", "dry"),
    share("delay", "level"),
    share("room", "level"),
    share("phaserdepth", "depth"),
    share("duckdepth", "depth"),
    share("tremolodepth", "depth"),
    share("irbegin", "begin"),
    share("chorus", "chorus"),
    number("clip", "factor", 0.0, 2.0, 0.01),
    // Effects.
    // "a threshold in dB, clamped to -100..0"
    count("compressor", "threshold", -100.0, 0.0),
    // "clamped to 0..0.98"
    number("delayfeedback", "feedback", 0.0, 0.98, 0.01),
    // "Positive times are capped at 1 second"
    number("delaytime", "delay", 0.0, 1.0, 0.01),
    // "admits values from 0 to 10"
    number("roomsize", "size", 0.0, 10.0, 0.1),
    number("roomfade", "seconds", 0.0, 10.0, 0.1),
    // "usually between 0 and 10"
    number("distort", "distortion", 0.0, 10.0, 0.1),
    share("distort", "volume"),
    // "clamped to 0.001..1"
    number("distortvol", "volume", 0.001, 1.0, 0.001),
    // "between 1 (for drastic reduction in bit-depth) to 16"
    number("crush", "depth", 1.0, 16.0, 0.1),
    count("coarse", "factor", 1.0, 64.0),
    // "renders 1 through 32 voices", and "above 32 refuse the event"
    count("unison", "numvoices", 1.0, 32.0),
    number("detune", "amount", 0.0, 2.0, 0.01),
    // The voice refuses an orbit from 16 up.
    count("orbit", "number", 0.0, 15.0),
    count("cut", "group", 0.0, 16.0),
    // "clamped to -0.99..0.99", and "usually 0..1"
    number("pw", "pulsewidth", 0.0, 0.99, 0.01),
    // "0 <= begin < end <= 1": each end of the slice keeps clear of the
    // other's end.
    number("begin", "amount", 0.0, 0.99, 0.01),
    number("end", "length", 0.01, 1.0, 0.01),
    number("speed", "speed", -2.0, 4.0, 0.01),
    count("squiz", "squiz", 1.0, 16.0),
    number("vib", "frequency", 0.0, 16.0, 0.1),
    number("vibmod", "depth", 0.0, 2.0, 0.01),
    number("tremolo", "speed", 0.0, 16.0, 0.1),
    number("phaser", "speed", 0.0, 16.0, 0.1),
    number("fmi", "brightness", 0.0, 16.0, 0.1),
    // Inharmonic ratios such as 1.06 and 1.61 are the point of fmh.
    number("fmh", "harmonicity", 0.0, 8.0, 0.01),
    // Waveshapers: an amount like distort's, and a level after it.
    number("asym", "distortion", 0.0, 10.0, 0.1),
    share("asym", "volume"),
    number("chebyshev", "distortion", 0.0, 10.0, 0.1),
    share("chebyshev", "volume"),
    number("cubic", "distortion", 0.0, 10.0, 0.1),
    share("cubic", "volume"),
    number("fold", "distortion", 0.0, 10.0, 0.1),
    share("fold", "volume"),
    number("hard", "distortion", 0.0, 10.0, 0.1),
    share("hard", "volume"),
    number("sinefold", "distortion", 0.0, 10.0, 0.1),
    share("sinefold", "volume"),
    number("soft", "distortion", 0.0, 10.0, 0.1),
    share("soft", "volume"),
];

/// The travel and step the table gives one parameter of one entry.
pub fn reference_number(entry: &str, parameter: &str) -> Option<&'static ReferenceNumber> {
    REFERENCE_NUMBERS
        .iter()
        .find(|number| number.entry == entry && number.parameter == parameter)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row is a travel a slider can move along, with whole numbers
    /// as stops, and one row names one parameter. A travel off its own
    /// grid is no travel.
    #[test]
    fn every_number_is_a_travel_on_its_own_grid() {
        for number in REFERENCE_NUMBERS {
            assert!(number.is_travel(), "{}.{}", number.entry, number.parameter);
        }
        assert!(!number("x", "y", 0.1, 1.0, 0.25).is_travel());
        assert!(!count("x", "y", 4.0, 2.0).is_travel());
        let mut names: Vec<_> = REFERENCE_NUMBERS
            .iter()
            .map(|number| (number.entry, number.parameter))
            .collect();
        let all = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all, "one row for each parameter");
    }
}
