//! Envelope shorthands.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{DeclaredIn, Registry, add_in};
use crate::Value;
use crate::combinators as c;

const ADSR: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "adsr",
    synonyms: &[],
    summary: "ADSR envelope: Combination of Attack, Decay, Sustain, and Release.",
    description: "ADSR envelope: Combination of Attack, Decay, Sustain, and Release.",
    params: &[
        crate::reference::ReferenceParam {
            name: "time",
            r#type: "number | Pattern",
            description: "attack time in seconds",
        },
        crate::reference::ReferenceParam {
            name: "time",
            r#type: "number | Pattern",
            description: "decay time in seconds",
        },
        crate::reference::ReferenceParam {
            name: "gain",
            r#type: "number | Pattern",
            description: "sustain level (0 to 1)",
        },
        crate::reference::ReferenceParam {
            name: "time",
            r#type: "number | Pattern",
            description: "release time in seconds",
        },
    ],
    examples: &["note(\"[c3 bb2 f3 eb3]*2\").sound(\"sawtooth\").lpf(600).adsr(\".1:.1:.5:.2\")"],
    tags: &["envelope", "amplitude"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const AD: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "ad",
    synonyms: &[],
    summary: "envelope shorthand: attack and decay in one control",
    description: "Sets attack and decay together - ad(x) gives both the same value, ad(\"a:d\") splits them across the two slots, and a missing decay falls back to the attack. Applied as two control sets, one after the other.",
    params: &[crate::reference::ReferenceParam {
        name: "value",
        r#type: "number | Pattern",
        description: "attack in seconds, optionally \"attack:decay\"; decay defaults to attack",
    }],
    examples: &["note(\"[c3 bb2 f3 eb3]*2\").s(\"sawtooth\").lpf(600).ad(\".1:.4\")"],
    tags: &["envelope"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const DS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "ds",
    synonyms: &[],
    summary: "envelope shorthand: decay and sustain in one control",
    description: "Sets decay and sustain together - ds(\"d:s\") splits the two slots, and a missing sustain falls back to 0.",
    params: &[crate::reference::ReferenceParam {
        name: "value",
        r#type: "number | Pattern",
        description: "\"decay:sustain\"; sustain defaults to 0",
    }],
    examples: &["note(\"[c3 bb2 f3 eb3]*2\").s(\"sawtooth\").lpf(600).ds(\".2:.5\")"],
    tags: &["envelope"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const AR: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "ar",
    synonyms: &[],
    summary: "envelope shorthand: attack and release in one control",
    description: "Sets attack and release together - ar(x) gives both the same value, ar(\"a:r\") splits them across the two slots, and a missing release falls back to the attack.",
    params: &[crate::reference::ReferenceParam {
        name: "value",
        r#type: "number | Pattern",
        description: "attack in seconds, optionally \"attack:release\"; release defaults to attack",
    }],
    examples: &["note(\"[c3 bb2 f3 eb3]*2\").s(\"sawtooth\").lpf(600).ar(\".05:.3\")"],
    tags: &["envelope"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // -- envelope shorthands ------------------------------------------------
    //
    // Registered here rather than in the control registry: they are late
    // combinator declarations, so they shadow the same-named control aliases
    // (`ds` shadows `delaysync`'s alias).
    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["adsr"],
        ADSR,
        2,
        false,
        crate::native_combinator!(|args, pat| c::adsr(
            &pat,
            args.first().unwrap_or(&Value::Undefined)
        )),
    );

    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["ad"],
        AD,
        2,
        false,
        crate::native_combinator!(|args, pat| c::ad(
            &pat,
            args.first().unwrap_or(&Value::Undefined)
        )),
    );

    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["ds"],
        DS,
        2,
        false,
        crate::native_combinator!(|args, pat| c::ds(
            &pat,
            args.first().unwrap_or(&Value::Undefined)
        )),
    );

    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["ar"],
        AR,
        2,
        false,
        crate::native_combinator!(|args, pat| c::ar(
            &pat,
            args.first().unwrap_or(&Value::Undefined)
        )),
    );
}
