//! An import must fail closed, for every specifier a score can spell.
//!
//! The engine evaluates music code as untrusted input inside QuickJS, and the
//! refusal happens at TWO layers. Imports visible in the source - static
//! declarations and a direct `import(...)` - are rejected by parsed AST node
//! before anything executes. Imports assembled at runtime are invisible to
//! that pass, so a rejecting module resolver stands behind it and refuses to
//! resolve anything at all.
//!
//! Both layers are tested separately and on purpose. They produce identical
//! outcomes today, so a single test could pass while either one quietly
//! stopped working. The messages differ, which is what lets these tests say
//! WHICH layer refused.
//!
//! One import resolves: a script that the host fetched and ran itself. The
//! realm still loads nothing.

use rustel_jsruntime::{ImportScope, JsRuntime, QueryError};
use rustel_transpiler::TranspileOptions;
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(10);

/// A file specifier is refused identically: the sandbox has no filesystem
/// view, so a score cannot read anything off this machine as code.
#[test]
fn dynamic_import_of_a_file_path_is_refused() {
    let rt = JsRuntime::new().unwrap();
    for specifier in ["file:///etc/passwd", "./secrets.mjs", "/etc/shadow"] {
        let error = rt
            .evaluate_prelude(
                &format!(r#"await import('{specifier}');"#),
                &TranspileOptions::default(),
                BUDGET,
            )
            .expect_err(&format!("specifier {specifier:?} must refuse"));
        match error {
            QueryError::Message(message) => assert!(
                // The AST validator owns a source-visible `import(...)`: it
                // refuses before execution, so the resolver is never reached.
                message.contains("dynamic import is disabled in native score code"),
                "{specifier}: wrong refusal: {message}"
            ),
            other => panic!("{specifier}: wrong refusal shape: {other:?}"),
        }
    }
}

/// The host fetches a script and the realm runs it. From then on its
/// specifier resolves, and every other import is still refused.
#[test]
fn a_script_the_host_ran_resolves_and_nothing_else_does() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    let url = "https://example.com/helpers.js";
    // The script is JavaScript as written. Code it hands to the repl is
    // score code, so its double-quoted string is mini-notation.
    let script = r#"globalThis.rawText = "bd sd";
(async () => { await window.strudelMirror.repl.evaluate('globalThis.viaRepl = "bd sd";'); })();"#;
    let never = std::sync::atomic::AtomicBool::new(false);
    let options = TranspileOptions::default();
    rt.run_import(url, script, &options, BUDGET, &never, ImportScope::Score)
        .expect("the script runs");
    assert!(rt.import_loaded(url));
    rt.evaluate_prelude(
        &format!(
            r#"await import("{url}");
await Function('t', 'return import(t)')('{url}');
if (typeof rawText !== 'string') throw new Error('a string of the script became a pattern');
if (typeof viaRepl !== 'object') throw new Error('repl code kept its mini-notation as text');
if (typeof strudelMirror !== 'undefined') throw new Error('the repl mirror stayed');"#
        ),
        &options,
        BUDGET,
    )
    .expect("the import resolves");
    for refused in [
        "await import('https://example.com/other.js');",
        "await Function('t', 'return import(t)')('https://example.com/other.js');",
    ] {
        rt.evaluate_prelude(refused, &options, BUDGET)
            .expect_err("a script the host did not run");
    }
}
