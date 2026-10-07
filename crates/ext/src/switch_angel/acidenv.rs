use rustel_core::Value;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;

pub(super) const REFERENCE: ReferenceEntry = ReferenceEntry {
    name: "acidenv",
    synonyms: &[],
    summary: "an acid low-pass envelope, opened by x",
    description: "Switch Angel's acid filter in one call: a resonant low-pass at 100 Hz whose envelope opens by x times nine, with a short sustain and decay - pat.lpf(100).lpenv(x * 9).lps(.2).lpd(.12).lpq(2). Hand it a slider and the filter follows your hand.",
    params: &[ReferenceParam {
        name: "x",
        r#type: "number | Pattern",
        description: "how far the envelope opens; 0 to 1 suits a slider.",
    }],
    examples: &["n(\"<0 4 0 9 7>*16\").scale(\"g:minor\").s(\"sawtooth\").acidenv(slider(0.45))"],
    tags: &["switch angel", "filter"],
    no_autocomplete: false,
    deprecated: false,
    origin: "switch angel",
};

/// Merge the extension's acid-filter controls over one hap value.
pub fn merge(left: &Value, amount: &Value) -> Value {
    let left = rustel_core::materialize_js_value(left);
    let Value::Object(base) = &left else {
        return left;
    };
    let Some(amount) = rustel_core::materialize_js_value(amount).as_f64() else {
        return left;
    };
    let mut merged = base.clone();
    merged.insert("cutoff".to_owned(), Value::F64(100.0));
    merged.insert("lpenv".to_owned(), Value::F64(amount * 9.0));
    merged.insert("lpsustain".to_owned(), Value::F64(0.2));
    merged.insert("lpdecay".to_owned(), Value::F64(0.12));
    merged.insert("resonance".to_owned(), Value::F64(2.0));
    Value::Object(merged)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["acidenv"],
        REFERENCE,
        2,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| {
            let Some(amount) = args.first() else {
                return PatOps::pat_silence();
            };
            pattern.app_left_with(amount.clone(), merge)
        }),
    );
}
