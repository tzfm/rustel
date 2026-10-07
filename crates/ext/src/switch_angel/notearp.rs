use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::{NativeExtensionOperand, ORIGIN};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "notearp",
    "arpeggiate notes by voice index",
    "Arpeggiates the receiver's chord by voice index: the indices pattern selects which voices sound, and an index past the last voice wraps around an octave higher.",
    params: [ReferenceParam {
        name: "indices",
        r#type: "number | Pattern",
        description: "voice indices to pick from the chord; indices beyond the chord wrap upward by octaves. Missing argument means silence.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").notearp(\"<0 1 2 0 2 1>\")"],
    "tonal"
);

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["notearp"],
        REFERENCE,
        2,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| {
            let Some(indices) = args.first() else {
                return PatOps::pat_silence();
            };
            pattern.arp_indices(indices.pattern_handle())
        }),
    );
}
