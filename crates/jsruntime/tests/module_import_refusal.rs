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

use rustel_jsruntime::{JsRuntime, QueryError};
use rustel_transpiler::TranspileOptions;
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(10);

/// A dynamic import of a remote URL is refused by the engine itself - no
/// request can be issued because nothing resolves the module.
#[test]
fn dynamic_import_of_a_remote_url_is_refused() {
    let rt = JsRuntime::new().unwrap();
    let error = rt
        .evaluate_prelude(
            r#"await import('https://strudel.b-cdn.net/not-a-module.mjs');"#,
            &TranspileOptions::default(),
            BUDGET,
        )
        .expect_err("a remote module import must refuse");
    match error {
        QueryError::Message(message) => assert!(
            // The AST validator owns a source-visible `import(...)`.
            message.contains("dynamic import is disabled in native score code"),
            "the refusal must name the layer that made it: {message}"
        ),
        other => panic!("wrong refusal shape: {other:?}"),
    }
}

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

/// Static module syntax is a syntax error in evaluated script, so an import
/// statement cannot smuggle a load in either.
#[test]
fn static_import_statements_fail_closed() {
    let rt = JsRuntime::new().unwrap();
    for source in [
        r#"import { x } from "https://example.com/mod.mjs";"#,
        r#""use strict"; import "https://example.com/mod.mjs";"#,
        r#"export { p } from "https://example.com/mod.mjs";"#,
    ] {
        assert!(
            rt.evaluate_prelude(source, &TranspileOptions::default(), BUDGET)
                .is_err(),
            "static module syntax must not evaluate: {source}"
        );
    }
}

/// The other layer: an import the outer parse cannot see.
///
/// `Function(...)` and `eval(...)` build their bodies at runtime, so the AST
/// validator never sees the `import`. The rejecting resolver refuses it.
#[test]
fn runtime_assembled_imports_are_refused_by_the_resolver() {
    let rt = JsRuntime::new().unwrap();
    // Every specifier is built at runtime. A bare `eval("import('...')")`
    // cannot be used here: the mini-notation pass rewrites the quoted body
    // into an `m(...)` call before `eval` ever sees it, so the probe would
    // measure the rewriter rather than the resolver.
    for source in [
        r#"await Function('t', 'return import(t)')('file:///etc/passwd');"#,
        r#"await Function('t', 'return import(t)')('https://example.com/mod.mjs');"#,
        r#"await Function('t', 'return import(t)')('./secrets.mjs');"#,
    ] {
        let error = rt
            .evaluate_prelude(source, &TranspileOptions::default(), BUDGET)
            .expect_err(&format!("{source} must refuse"));
        match error {
            QueryError::Message(message) => assert!(
                // NOT the AST validator's message: the outer parse saw no
                // import at all, so this proves the resolver did the refusing.
                message.contains("module loading is disabled in native score code"),
                "{source}: expected the resolver to refuse, got: {message}"
            ),
            other => panic!("{source}: wrong refusal shape: {other:?}"),
        }
    }
}
