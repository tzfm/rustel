//! Behavioral coverage for the strudel.cc compatibility surface:
//! global operator forms, missing `_name` pattern methods, and one-time logged
//! no-op stand-ins.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot, strudel_compat_globals, strudel_compat_methods};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
}

fn eval_and_show(runtime: &JsRuntime, source: &str) -> Vec<String> {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("score failed ({source}): {error}"));
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap_or_else(|error| panic!("query failed ({source}): {error}"))
        .into_iter()
        .map(|hap| hap.show())
        .collect()
}

#[test]
fn every_compose_operator_has_a_global_and_raw_method_form() {
    let runtime = runtime();
    let cases = [
        ("pow", "pure(3)", "2"),
        ("lt", "pure(3)", "5"),
        ("gt", "pure(5)", "3"),
        ("lte", "pure(3)", "3"),
        ("gte", "pure(3)", "3"),
        ("eq", "pure(3)", "3"),
        ("eqt", "pure(3)", "3"),
        ("ne", "pure(3)", "4"),
        ("net", "pure(3)", "4"),
        ("and", "pure(1)", "2"),
        ("or", "pure(0)", "2"),
        ("mod", "pure(7)", "4"),
        ("band", "pure(6)", "3"),
        ("bor", "pure(4)", "2"),
        ("bxor", "pure(6)", "3"),
        ("blshift", "pure(3)", "2"),
        ("brshift", "pure(12)", "2"),
    ];

    for (op, receiver, rhs) in cases {
        let method = eval_and_show(&runtime, &format!("{receiver}.{op}({rhs})"));
        let direct_global = eval_and_show(&runtime, &format!("{op}({rhs}, {receiver})"));
        let curried_global = eval_and_show(&runtime, &format!("{receiver}.every(1, {op}({rhs}))"));
        let raw_method = eval_and_show(&runtime, &format!("{receiver}._{op}({rhs})"));

        assert_eq!(direct_global, method, "global {op}({rhs}, pat) mismatch");
        assert_eq!(
            curried_global, method,
            "curried {op}({rhs}) inside every() mismatch"
        );
        assert_eq!(raw_method, method, "raw ._{op}({rhs}) mismatch");
    }

    assert_eq!(
        eval_and_show(&runtime, "plyWith(2, x => x, s('bd'))"),
        eval_and_show(&runtime, "s('bd').plyWith(2, x => x)")
    );
    assert_eq!(
        eval_and_show(&runtime, "s('bd')._plyWith(2, x => x)"),
        eval_and_show(&runtime, "s('bd').plyWith(2, x => x)")
    );
    assert_eq!(
        eval_and_show(&runtime, "s('bd')._slice(4, '0 1')"),
        eval_and_show(&runtime, "s('bd').slice(4, '0 1')")
    );
    assert_eq!(
        eval_and_show(&runtime, "s('bd')._bite(4, '0 1')"),
        eval_and_show(&runtime, "s('bd').bite(4, '0 1')")
    );
    assert_eq!(
        eval_and_show(&runtime, "bind(x => pure(x + 1), pure(2))"),
        eval_and_show(&runtime, "pure(2).bind(x => pure(x + 1))")
    );
    assert_eq!(
        eval_and_show(&runtime, "timeline(1, s('bd'))"),
        eval_and_show(&runtime, "s('bd').timeline(1)")
    );
    runtime
        .evaluate_score(
            "globalThis.__drawMatch = Number(s('bd').drawLine(10) === drawLine(s('bd'), 10) && s('bd').drawLine(10).includes('bd')); silence",
            &TranspileOptions::default(),
        )
        .expect("drawLine method");
    assert_eq!(runtime.get_number("__drawMatch"), Some(1.0));
}

#[test]
fn every_listed_strudel_global_and_method_is_installed() {
    let runtime = runtime();
    let globals: Vec<&str> = strudel_compat_globals().collect();
    let methods: Vec<&str> = strudel_compat_methods().collect();
    assert!(!globals.is_empty());
    assert!(!methods.is_empty());

    let mut script = String::from("/* mini-off */\nglobalThis.__missingGlobals = [];\n");
    for name in &globals {
        script.push_str(&format!(
            "if (!Object.hasOwn(globalThis, '{name}')) globalThis.__missingGlobals.push('{name}');\n"
        ));
    }
    script.push_str("globalThis.__missingMethods = [];\n");
    for name in &methods {
        script.push_str(&format!(
            "if (!Object.hasOwn(Pattern.prototype, '{name}')) globalThis.__missingMethods.push('{name}');\n"
        ));
        let raw = format!("_{name}");
        script.push_str(&format!(
            "if (!Object.hasOwn(Pattern.prototype, '{raw}')) globalThis.__missingMethods.push('{raw}');\n"
        ));
    }
    script.push_str(
        "globalThis.__missingGlobalsCount = globalThis.__missingGlobals.length;\n\
         globalThis.__missingMethodsCount = globalThis.__missingMethods.length;\n\
         silence",
    );
    runtime
        .evaluate_score(&script, &TranspileOptions::default())
        .expect("surface inventory check");
    assert_eq!(runtime.get_number("__missingGlobalsCount"), Some(0.0));
    assert_eq!(runtime.get_number("__missingMethodsCount"), Some(0.0));

    // K() is KabelSalat and stays out of stand-ins.
    assert!(!runtime.compat_standin_names().contains(&"K"));
    assert!(
        runtime
            .evaluate_score("K()", &TranspileOptions::default())
            .is_err(),
        "bare K() must still be refused"
    );
}

#[test]
fn standin_globals_signals_and_methods_log_once_and_keep_scores_running() {
    let runtime = runtime();
    let _ = runtime.take_logs();

    // Global stand-ins (including getPerformanceTimeSeconds, tagged templates,
    // and constructible names) return silence and log once across calls.
    let global_haps = eval_and_show(
        &runtime,
        r#"
          const a = getPerformanceTimeSeconds();
          const b = getPerformanceTimeSeconds();
          const c = tidal`bd sd`;
          const d = new Cyclist();
          globalThis.__allPatterns = Number(
            a instanceof Pattern && b instanceof Pattern && c instanceof Pattern && d instanceof Pattern
          );
          stack(a, b, c, d)
        "#,
    );
    assert!(global_haps.is_empty(), "{global_haps:?}");
    assert_eq!(runtime.get_number("__allPatterns"), Some(1.0));
    let logs = runtime.take_logs();
    assert_eq!(
        logs.iter()
            .filter(|line| line.contains("getPerformanceTimeSeconds is not supported"))
            .count(),
        1,
        "{logs:?}"
    );
    assert_eq!(
        logs.iter()
            .filter(|line| line.contains("tidal is not supported"))
            .count(),
        1,
        "{logs:?}"
    );
    assert_eq!(
        logs.iter()
            .filter(|line| line.contains("Cyclist is not supported"))
            .count(),
        1,
        "{logs:?}"
    );

    // Repeating a stand-in on the next score does not log again.
    let _ = eval_and_show(&runtime, "getPerformanceTimeSeconds()");
    assert!(runtime.take_logs().is_empty());

    // Motion-sensor signals behave as silent Patterns and allow assignment.
    let signal_haps = eval_and_show(
        &runtime,
        r#"
          const x = accX.range(200, 2000);
          const y = accX.range(100, 400);
          stack(x, y)
        "#,
    );
    assert!(signal_haps.is_empty(), "{signal_haps:?}");
    let logs = runtime.take_logs();
    assert_eq!(
        logs.iter()
            .filter(|line| line.contains("accX is not supported"))
            .count(),
        1,
        "{logs:?}"
    );
    let assigned_signal = eval_and_show(
        &runtime,
        r#"
          accX = pure(42);
          accX
        "#,
    );
    assert_eq!(assigned_signal, eval_and_show(&runtime, "pure(42)"));

    // Stand-in methods return `this` unchanged and share one-time logging with `_name`.
    let base = eval_and_show(&runtime, "s('bd sd')");
    let chained = eval_and_show(&runtime, "s('bd sd').mqtt()._mqtt().mqtt()");
    assert_eq!(chained, base);
    let logs = runtime.take_logs();
    assert_eq!(
        logs.iter()
            .filter(|line| line.contains("mqtt is not supported"))
            .count(),
        1,
        "{logs:?}"
    );

    // register() replaces a stand-in without a built-in replacement notice.
    let replaced = eval_and_show(
        &runtime,
        r#"
          register('supradough', (pat) => pat.fast(2));
          s('bd').supradough()
        "#,
    );
    assert_eq!(replaced.len(), 2, "{replaced:?}");
    let logs = runtime.take_logs();
    assert!(
        logs.iter()
            .all(|line| !line.contains("replaces the built-in")),
        "stand-in replacement emitted built-in warning: {logs:?}"
    );
}
