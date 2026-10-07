//! strudel.cc's page-level painter spellings: `all(pianoroll)` hands the
//! painter the stack, `all(pianoroll({ labels: 1 }))` hands it options
//! first and expects a function that takes the pattern. Both must evaluate,
//! and so must `wordfall`, the punchcard on its side.

use rustel_jsruntime::JsRuntime;
use rustel_transpiler::{TranspileOptions, VISUAL_WIDGET_METHODS};

fn evaluate(source: &str) -> Result<(), String> {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    // The studio's own options: the underscored spellings are rewritten
    // to the pattern methods with their widget slot, as in the editor.
    let options = TranspileOptions {
        widget_methods: VISUAL_WIDGET_METHODS
            .iter()
            .map(|method| (*method).to_owned())
            .collect(),
        ..TranspileOptions::default()
    };
    runtime
        .evaluate_score(source, &options)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[test]
fn all_accepts_a_painter_with_or_without_options() {
    evaluate("$: s(\"bd\")\nall(pianoroll)").expect("all(pianoroll)");
    evaluate("$: s(\"bd\")\nall(pianoroll({ labels: 1 }))").expect("all(pianoroll(options))");
    evaluate("$: s(\"bd\")\nall(punchcard({ fold: 0 }))").expect("all(punchcard(options))");
    evaluate("$: s(\"bd\")\nall(wordfall)").expect("all(wordfall)");
}

#[test]
fn wordfall_is_a_pattern_method_in_both_spellings() {
    evaluate("$: s(\"bd\").wordfall()").expect("wordfall");
    evaluate("$: s(\"bd\")._wordfall({ labels: 0 })").expect("_wordfall");
}
