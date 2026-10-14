//! Rust graph construction on the score thread, producing bounded native IR.

use super::*;
use rquickjs::{Array, Object, Value, function::Args};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

mod graph;
mod library;
mod reference;

pub(super) fn reference_entries()
-> impl Iterator<Item = &'static rustel_core::reference::ReferenceEntry> {
    reference::ENTRIES.iter().chain(reference::BYPASSES.iter())
}

const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_INPUTS: usize = 1024;

#[derive(JsLifetime)]
struct GraphCompiler<'js> {
    function_constructor: Function<'js>,
    active: RefCell<Vec<Rc<graph::State<'js>>>>,
}

pub(super) fn install<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
    let function_constructor = ctx.globals().get("Function")?;
    ctx.store_userdata(GraphCompiler {
        function_constructor,
        active: RefCell::new(Vec::new()),
    })?;
    Ok(())
}

pub(crate) fn clear(ctx: &Ctx<'_>) {
    let _ = ctx.remove_userdata::<GraphCompiler>();
}

/// Compiles a fresh graph with this event's numeric inputs and frequency in Hz.
/// User score execution retains the surrounding realm's CPU and heap limits.
/// Native graph construction has independent depth, operation and size limits.
pub(crate) fn compile_graph<'js>(
    ctx: &Ctx<'js>,
    source: &str,
    inputs: &[f64],
    frequency: f64,
) -> rquickjs::Result<String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(rquickjs::Exception::throw_range(
            ctx,
            "KabelSalat source exceeds 64 KiB",
        ));
    }
    if inputs.len() > MAX_INPUTS {
        return Err(rquickjs::Exception::throw_range(
            ctx,
            "KabelSalat pattern inputs exceed 1024 values",
        ));
    }
    if !frequency.is_finite() || inputs.iter().any(|value| !value.is_finite()) {
        return Err(rquickjs::Exception::throw_type(
            ctx,
            "KabelSalat inputs and frequency must be finite numbers",
        ));
    }
    let constructor = ctx
        .userdata::<GraphCompiler>()
        .ok_or_else(|| {
            rquickjs::Exception::throw_type(ctx, "KabelSalat bindings are not installed")
        })?
        .function_constructor
        .clone();
    let state = Rc::new(graph::State::new(ctx)?);
    {
        let compiler = ctx
            .userdata::<GraphCompiler>()
            .expect("installed graph compiler");
        let mut active = compiler.active.borrow_mut();
        if active.len() >= 8 {
            return Err(rquickjs::Exception::throw_range(
                ctx,
                "KabelSalat graph compilation nesting exceeds 8",
            ));
        }
        active.push(state.clone());
    }
    // Always discard event-local registrations and roots, including on exceptions.
    let result = (|| {
        let scope = graph::scope(ctx, inputs, frequency)?;
        let mut parameters = Args::new(ctx.clone(), scope.len() + 1);
        for (name, _) in &scope {
            parameters.push_arg(name.as_str())?;
        }
        parameters.push_arg(source)?;
        let body: Function = constructor.call_arg(parameters)?;
        let mut arguments = Args::new(ctx.clone(), scope.len());
        for (_, value) in scope {
            arguments.push_arg(value)?;
        }
        let _: Value = body.call_arg(arguments)?;
        let outputs = state.outputs.borrow().clone();
        let root = graph::get_node(ctx, "exit", outputs)?;
        graph::serialize(ctx, root)
    })();
    ctx.userdata::<GraphCompiler>()
        .expect("installed graph compiler")
        .active
        .borrow_mut()
        .pop();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(source: &str, inputs: &[f64], frequency: f64) -> String {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            install(&ctx).expect("graph bindings");
            compile_graph(&ctx, source, inputs, frequency).expect("graph")
        })
    }

    fn assert_graph(source: &str, predicate: &str) {
        let json = graph(source, &[3.0], 220.0);
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            let check: Function = ctx
                .eval(format!(
                    "(json) => {{ const {{nodes}} = JSON.parse(json); return {predicate}; }}"
                ))
                .expect("predicate");
            assert!(check.call::<_, bool>((json,)).expect("check"));
        });
    }

    #[test]
    fn numeric_event_inputs_and_gate_are_available() {
        assert_graph(
            "sine(sFreq / 2 + pat[0]).mul(sGate).out()",
            "nodes.some(n => n.type === 'n' && n.value === 113) && nodes.some(n => n.type === 'gate')",
        );
    }

    #[test]
    fn multichannel_expansion_wraps_inputs_and_collects_outputs() {
        assert_graph(
            "sine([100, 200, 300]).mul([0.1, 0.2]).out([0,1,2])",
            "nodes.filter(n => n.type === 'sine').length === 3 && nodes.filter(n => n.type === 'mul').length === 3 && nodes.filter(n => n.type === 'output').length === 3 && nodes[nodes.length - 1].type === 'exit'",
        );
    }

    #[test]
    fn feedback_edges_preserve_postorder_registers() {
        assert_graph(
            "sine(220).add(x => x.mul(0.5)).out()",
            "nodes.some((n, i) => n.inputs.some(input => input > i))",
        );
    }

    #[test]
    fn legacy_feedback_uses_registered_node_method() {
        assert_graph(
            "register('add', input => input.mul(4)); n(2).feedback(x => x).out(); feedback(x => x.mul(.5)).out()",
            "nodes.some(n => n.type === 'n' && n.value === 4) && nodes.filter(n => n.type === 'add').length === 1",
        );
    }

    #[test]
    fn registered_modules_expand_to_primitive_nodes() {
        assert_graph(
            "const voice = module('voice', freq => freq.sine().mul(.2)); voice(sFreq).out(); n(330).voice().out()",
            "nodes.filter(n => n.type === 'sine').length === 2 && nodes.every(n => n.type !== 'voice')",
        );
    }

    #[test]
    fn empty_source_is_silent_and_defaults_stay_native() {
        assert_eq!(
            graph("", &[], 220.0),
            r#"{"nodes":[{"type":"exit","inputs":[]}]}"#
        );
        assert_graph(
            "sine().out()",
            "nodes.find(n => n.type === 'sine').inputs.length === 0",
        );
    }

    #[test]
    fn expression_templates_are_preserved_as_source() {
        assert_graph(
            "time().mul(8000).bytebeat`t >> 4 | t`.out()",
            "nodes.some(n => n.type === 'bytebeat' && n.value === 't >> 4 | t')",
        );
    }

    #[test]
    fn native_builder_matches_upstream_graph_fixtures() {
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/kabelsalat_graphs.json"
        )))
        .expect("fixtures");
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            install(&ctx).expect("graph bindings");
            for fixture in fixtures.as_array().expect("cases") {
                let name = fixture["name"].as_str().expect("name");
                let source = fixture["source"].as_str().expect("source");
                let actual: serde_json::Value = serde_json::from_str(
                    &compile_graph(&ctx, source, &[3.0], 220.0)
                        .unwrap_or_else(|error| panic!("{name}: {error}")),
                )
                .expect("compiled graph");
                let actual = actual["nodes"].as_array().expect("actual nodes");
                let expected = fixture["graph"]["nodes"]
                    .as_array()
                    .expect("expected nodes");
                assert_eq!(actual.len(), expected.len(), "{name}: node count");
                for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                    assert_eq!(actual["type"], expected["type"], "{name}: node {index}");
                    assert_eq!(actual["inputs"], expected["inputs"], "{name}: node {index}");
                    match (actual["value"].as_f64(), expected["value"].as_f64()) {
                        (Some(actual), Some(expected)) => assert!(
                            (actual - expected).abs() <= 1e-12 * expected.abs().max(1.0),
                            "{name}: node {index}: {actual} != {expected}"
                        ),
                        _ => assert_eq!(actual["value"], expected["value"], "{name}: node {index}"),
                    }
                }
            }
        });
    }

    #[test]
    fn inlet_schemas_retain_names_defaults_and_dynamic_inputs() {
        assert_graph(
            "if (getInletName('sine', 2) !== 'phase' || getInletName('sine', 5) !== '') throw Error('inlet'); if (nodeRegistry.get('adsr').ins[1].default !== .02 || !nodeRegistry.get('add').ins[0].dynamic) throw Error('schema'); nodeRegistry.set('custom', {ins: [{name: 'value'}]}).set('extra', {}); if (getInletName('custom', 0) !== 'value') throw Error('custom'); sine(220).out()",
            "nodes.some(n => n.type === 'sine')",
        );
    }

    #[test]
    fn repeated_inspection_charges_the_construction_budget() {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            install(&ctx).expect("graph bindings");
            for source in [
                "let x = n(0); for (let i = 0; i < 100; i++) x = x.add(1); for (let i = 0; i < 1000; i++) x.select('missing')",
                "let x = n(0); for (let i = 0; i < 100; i++) x = x.add(1); for (let i = 0; i < 1000; i++) x.flatten()",
                "for (let i = 0; i < 33000; i++) nodeRegistry.get('sine')",
            ] {
                assert!(compile_graph(&ctx, source, &[], 220.0).is_err(), "{source}");
                let exception = ctx.catch();
                let error = rquickjs::Coerced::<String>::from_js(&ctx, exception)
                    .expect("exception text")
                    .0;
                assert!(error.contains("operation or depth limits"), "{error}");
                compile_graph(&ctx, "sine(220).out()", &[], 220.0).expect("recovered graph");
            }
        });
    }

    #[test]
    fn oversized_and_nonfinite_graphs_fail() {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            install(&ctx).expect("graph bindings");
            for source in [
                "n(Infinity).out()",
                "let x = n(0); for (let i = 0; i < 256; i++) x = x.add(1); x.out()",
                "n({type: 'n', ins: [], value: {}}).out()",
            ] {
                assert!(compile_graph(&ctx, source, &[], 220.0).is_err());
                let _ = ctx.catch();
            }
            assert!(compile_graph(&ctx, &" ".repeat(MAX_SOURCE_BYTES + 1), &[], 220.0).is_err());
            let _ = ctx.catch();
        });
    }

    #[test]
    fn registrations_preserve_function_identity_and_builtin_scope() {
        assert_graph(
            "const a = module('voice', x => x.add(2)); module('voice', x => x.mul(3)); a(1).out(0); n(1).voice().out(1)",
            "nodes.filter(n => n.type === 'add').length === 1 && nodes.filter(n => n.type === 'mul').length === 1",
        );
        assert_graph(
            "register('sine', x => x.mul(2)); sine(220).out(0); n(220).sine().out(1)",
            "nodes.filter(n => n.type === 'sine').length === 1 && nodes.filter(n => n.type === 'mul').length === 1",
        );
    }

    #[test]
    fn mutable_nodes_and_preorder_flatten_keep_shared_edges() {
        assert_graph(
            "const x = new Node('n', 7); const y = x.add(x); const flat = y.flatten(); if (flat[0].type !== 'add' || flat[0].ins[0] !== '1' || flat[0].ins[1] !== '1') throw Error('flatten'); x.value = 9; y.out()",
            "nodes.filter(n => n.type === 'n' && n.value === 9).length === 1",
        );
    }

    #[test]
    fn construction_limits_and_throwing_getters_leave_the_next_graph_usable() {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            install(&ctx).expect("graph bindings");
            for source in [
                "const x = []; x.push(x); n(x).out()",
                "for (let i = 0; i < 9000; i++) n(i)",
                "const x = n(1); Object.defineProperty(x, 'ins', {get() { throw Error('getter') }}); x.out()",
                "const x = n(1); x.ins = Array(1025).fill(x); x.out()",
                "const recurse = register('recurse', x => x.recurse()); n(1).recurse().out()",
            ] {
                assert!(compile_graph(&ctx, source, &[], 220.0).is_err(), "{source}");
                let _ = ctx.catch();
                compile_graph(&ctx, "sine(220).out()", &[], 220.0).expect("recovered graph");
            }
        });
    }

    #[test]
    fn escaped_registration_cannot_resolve_to_a_later_graph() {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            install(&ctx).expect("graph bindings");
            compile_graph(
                &ctx,
                "globalThis.savedVoice = module('voice', x => x.sine())",
                &[],
                220.0,
            )
            .expect("first graph");
            assert!(
                compile_graph(
                    &ctx,
                    "module('voice', x => x.saw()); savedVoice(220).out()",
                    &[],
                    220.0
                )
                .is_err()
            );
            let _ = ctx.catch();
            compile_graph(&ctx, "sine(220).out()", &[], 220.0).expect("recovered graph");
        });
    }

    #[test]
    fn graph_registrations_do_not_escape_the_event() {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            install(&ctx).expect("graph bindings");
            compile_graph(
                &ctx,
                "register('privateVoice', x => x.sine()); n(220).privateVoice().out()",
                &[],
                220.0,
            )
            .expect("first graph");
            assert!(compile_graph(&ctx, "n(220).privateVoice().out()", &[], 220.0).is_err());
            let _ = ctx.catch();
        });
    }
}
