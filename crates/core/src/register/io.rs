//! The wire: MIDI, OSC, serial and sysex. Configuration, not pattern.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{DeclaredIn, JoinKind, Registration, Registry, add, arg_text};
use crate::Value;
use crate::combinators as c;
use std::sync::Arc;

const SERIAL: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "serial",
    synonyms: &[],
    summary: "send the pattern to a serial device",
    description: "Configuration, not a pattern: it marks the events for the named serial port at the given baud, with optional CRC and short IDs, and the serial bridge does the sending. A baud that is not finite and positive falls back to 115200; a port no score names is \"default\".",
    params: &[
        crate::reference::ReferenceParam {
            name: "baud",
            r#type: "number",
            description: "baud rate; default 115200",
        },
        crate::reference::ReferenceParam {
            name: "sendCrc",
            r#type: "boolean",
            description: "append a CRC to the message",
        },
        crate::reference::ReferenceParam {
            name: "shortIds",
            r#type: "boolean",
            description: "use short component IDs",
        },
        crate::reference::ReferenceParam {
            name: "port",
            r#type: "string",
            description: "the serial port name; default \"default\"",
        },
    ],
    examples: &["note(\"c e g\").serial(9600)"],
    tags: &["io", "serial"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const MIDI: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "midi",
    synonyms: &[],
    summary: "MIDI output: Opens a MIDI output port.",
    description: "MIDI output: Opens a MIDI output port.",
    params: &[
        crate::reference::ReferenceParam {
            name: "midiport",
            r#type: "midiout | number",
            description: "MIDI device name or index defaulting to 0",
        },
        crate::reference::ReferenceParam {
            name: "options",
            r#type: "object",
            description: "Additional MIDI configuration options",
        },
    ],
    examples: &[
        "note(\"c4\").midichan(1).midi('IAC Driver Bus 1')",
        "note(\"c4\").midichan(1).midi('IAC Driver Bus 1', { controller: true, latency: 50 })",
    ],
    tags: &["external_io"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const OSC: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "osc",
    synonyms: &[],
    summary: "send the pattern to SuperDirt over OSC",
    description: "Marks every event for the OSC output on the given port; with no argument the port is 57120. A port that is not a finite whole number between 0 and 65536 falls back to the default. An oscport set on a hap itself wins - osc() keeps rather than overwrites it. The port is routing, not music: it does not travel down the wire.",
    params: &[crate::reference::ReferenceParam {
        name: "port",
        r#type: "number",
        description: "destination UDP port; default 57120",
    }],
    examples: &["s(\"bd sd\").osc()"],
    tags: &["io", "osc"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SYSEX: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "sysex",
    synonyms: &[],
    summary: "MIDI sysex: Sends a MIDI sysex message.",
    description: "MIDI sysex: Sends a MIDI sysex message.",
    params: &[
        crate::reference::ReferenceParam {
            name: "id",
            r#type: "number | Pattern",
            description: "Sysex ID",
        },
        crate::reference::ReferenceParam {
            name: "data",
            r#type: "number | Pattern",
            description: "Sysex data",
        },
    ],
    examples: &["note(\"c4\").sysex([\"0x77\", \"0x01:0x02:0x03:0x04\"]).midichan(1).midi()"],
    tags: &["external_io", "midi"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // Configuration, not a pattern: the same reasoning as `.midi()`.
    r.register(Registration {
        names: vec![Arc::from("serial")],
        reference: SERIAL,
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
        arity: 5,
        patternify: false,
        preserve_steps: false,
        join: JoinKind::Inner,
        func: crate::native_combinator!(|args, pat| {
            let baud = match args.first() {
                Some(Value::F64(baud)) if baud.is_finite() && *baud > 0.0 => *baud,
                _ => 115_200.0,
            };
            let truthy = |index: usize| {
                matches!(args.get(index), Some(Value::Bool(true)))
                    || matches!(args.get(index), Some(Value::F64(value)) if *value != 0.0)
            };
            let port = match args.get(3) {
                Some(Value::Str(name)) => name.clone(),
                _ => "default".to_string(),
            };
            c::serial(&pat, baud, truthy(1), truthy(2), &port)
        }),
    });

    // Registered directly rather than through `add_fn`, which hardcodes
    // `patternify: true`. A patternified argument turns the port NAME into
    // mini-notation, so "IAC Driver Bus 1" becomes four haps whose first is
    // "IAC"; `.midi()` takes its port as configuration instead.
    r.register(Registration {
        names: vec![Arc::from("midi")],
        reference: MIDI,
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
        arity: 2,
        patternify: false,
        preserve_steps: false,
        join: JoinKind::Inner,
        func: crate::native_combinator!(|args, pat| c::midi(
            &pat,
            &arg_text(args, 0),
            args.get(1).unwrap_or(&Value::Undefined)
        )),
    });

    // Same reasoning as `.midi()`: the destination is configuration, not a
    // pattern, so it must not be patternified into mini-notation.
    r.register(Registration {
        names: vec![Arc::from("osc")],
        reference: OSC,
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
        arity: 2,
        patternify: false,
        preserve_steps: false,
        join: JoinKind::Inner,
        func: crate::native_combinator!(|args, pat| {
            // A wire port is a whole number: a fraction is not a port the
            // bridge could route, so it falls back to the default like every
            // other unusable argument rather than being silently dropped
            // downstream.
            let port = match args.first() {
                Some(Value::F64(port))
                    if port.is_finite()
                        && port.fract() == 0.0
                        && *port > 0.0
                        && *port < 65536.0 =>
                {
                    *port
                }
                _ => 57120.0,
            };
            c::osc(&pat, port)
        }),
    });

    add(
        r,
        &["sysex"],
        SYSEX,
        2,
        false,
        crate::native_combinator!(|args, pat| c::sysex(
            &pat,
            args.first().unwrap_or(&Value::Undefined)
        )),
    );
}
