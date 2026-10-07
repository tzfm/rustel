//! A KabelSalat call is refused with the KabelSalat message wherever the
//! transpiler leaves it as code.

use rustel_jsruntime::JsRuntime;
use rustel_transpiler::TranspileOptions;

/// `K(S(K(1)))` evaluates the lifted `K(1)` before the outer worklet, and
/// both refuse as KabelSalat rather than as an undefined name.
#[test]
fn a_kabelsalat_call_left_as_code_is_refused_as_kabelsalat() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    for source in ["K(1)", "K(S(K(1)))"] {
        let error = runtime
            .evaluate_score(source, &TranspileOptions::default())
            .expect_err("a KabelSalat call must be refused");
        assert!(error.contains("K() needs kabelsalat"), "{source}: {error}");
        assert!(!error.contains("not defined"), "{source}: {error}");
    }
}
