//! Compiling a recognised method chain against the sealed built-ins.
//!
//! The program calls the same native operation as each JavaScript method,
//! with the same arguments, so it builds the same graph. A chain that calls
//! anything but a proven built-in, or that would throw, is declined and stays
//! a QuickJS callback.

use super::*;
use rustel_core::callback_ir::{MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS, PatternTransformProgram};
use rustel_transpiler::{
    CallbackArgument, CallbackMethodCall, MAX_CALLBACK_TRANSFORM_DEPTH, PatternTransformCandidate,
};

/// A compiled chain and what it reads as its callback would.
pub(crate) struct LoweredCallback<'js> {
    pub(crate) program: PatternTransformProgram,
    pub(crate) dependencies: LoweringDependencies,
    /// The callback cells of nested transform arguments, for the caller to
    /// own together with the compiled callback's own cell.
    pub(crate) sidecars: Vec<Sidecar<'js>>,
}

/// A chain that stays a QuickJS callback.
pub(crate) struct Declined;

/// Compile `parameter.method(...arguments)...` against the sealed built-ins.
///
/// Every throw such a chain can raise follows from its literals: an
/// argument count a method refuses, an empty composer call, an unknown
/// method, or mini-notation that does not parse. Each of those declines, so
/// the callback keeps throwing where and when it does. A compiled chain
/// throws in neither form.
pub(crate) fn lower_method_chain<'js>(
    ctx: &Ctx<'js>,
    calls: &[CallbackMethodCall],
    depth: usize,
) -> Result<LoweredCallback<'js>, Declined> {
    // Every built-in method shapes its arguments through `Object` and
    // `Array`. A change to them that no text or host function announced
    // keeps the chain JavaScript.
    if calls.len() > MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS
        || depth > MAX_CALLBACK_TRANSFORM_DEPTH
        || !prove_surface_intrinsics(ctx)
    {
        return Err(Declined);
    }
    let mut dependencies = LoweringDependencies::default();
    let mut nested = Vec::new();
    let mut instructions = Vec::new();
    instructions
        .try_reserve_exact(calls.len())
        .map_err(|_| Declined)?;
    for call in calls {
        let method = prove_builtin_method(ctx, &call.method).ok_or(Declined)?;
        let takes_transform = call
            .arguments
            .iter()
            .any(|argument| matches!(argument, CallbackArgument::Transform { .. }));
        // A nested arrow can fill only a registered method's function slot.
        // Checked before the arguments lower, so a call that declines does
        // not lower - and count - nested arrows it then throws away: once at
        // evaluation, and again per query through the JavaScript the
        // callback keeps.
        match &method {
            BuiltinMethod::Registered(registration) if takes_transform => {
                if registration.function_argument
                    != rustel_core::register::FunctionArgument::Transformer
                {
                    return Err(Declined);
                }
            }
            _ if takes_transform => {
                // No other method takes a function the registration could
                // apply; the JavaScript call stays the observable one.
                return Err(Declined);
            }
            _ => {}
        }
        let arguments = call
            .arguments
            .iter()
            .map(|argument| lower_argument(ctx, argument, &mut dependencies, &mut nested, depth))
            .collect::<Result<Vec<_>, _>>()?;
        instructions.push(
            method
                .step(&call.method, arguments, takes_transform)
                .ok_or(Declined)?,
        );
        dependencies.methods.push(call.method.clone());
    }
    // A step that cannot keep a pure operand pure is refused here.
    let program = PatternTransformProgram::new(instructions).map_err(|_| Declined)?;
    Ok(LoweredCallback {
        program,
        dependencies,
        sidecars: nested,
    })
}

/// The pattern a method receives for one literal argument, as the method
/// reifies it.
fn lower_argument<'js>(
    ctx: &Ctx<'js>,
    argument: &CallbackArgument,
    dependencies: &mut LoweringDependencies,
    nested: &mut Vec<Sidecar<'js>>,
    depth: usize,
) -> Result<Pattern, Declined> {
    Ok(match argument {
        CallbackArgument::Number(value) => rustel_core::pure(Value::F64(*value)),
        CallbackArgument::Bool(value) => rustel_core::pure(Value::Bool(*value)),
        // With no string parser configured, `reify` makes a string pure.
        CallbackArgument::String(text) => {
            dependencies.raw_strings = true;
            rustel_core::pure(Value::Str(text.to_string()))
        }
        CallbackArgument::Mini { source, offset } => {
            if !prove_mini_callee(ctx) {
                return Err(Declined);
            }
            dependencies.mini_callee = true;
            transpiled_mini(source, *offset).map_err(|_| Declined)?
        }
        CallbackArgument::Transform { source, candidate } => {
            let (pattern, sidecars) = lower_transform(ctx, source, candidate, depth)?;
            nested.extend(sidecars);
            pattern
        }
    })
}

/// Lower one nested transform arrow into the function value its method call
/// receives: bridge an equivalent JavaScript function for the fallback id,
/// compile the arrow's own program, and let the host own the choice between
/// them.
///
/// Recognition proved the arrow capture-free - its body is method calls on
/// its own parameter with literal arguments - so evaluating its exact source
/// in the realm builds a function that behaves as the arrow does. Its only
/// possible free name is the mini-notation callee `m`, which the arrow's own
/// standings cover. The arrow's program holds its own standings, so a change
/// the inner chain reads reaches its callback again even while the outer
/// program still stands.
fn lower_transform<'js>(
    ctx: &Ctx<'js>,
    source: &str,
    candidate: &PatternTransformCandidate,
    depth: usize,
) -> Result<(Pattern, Vec<Sidecar<'js>>), Declined> {
    let function: Function<'js> = ctx.eval(source).map_err(|_| Declined)?;
    let (id, sidecar) = bridge_callable(ctx, function).map_err(|_| Declined)?;
    let state = host_callback_ir(ctx).map_err(|_| Declined)?;
    let mode = state.mode.get();
    let lowered = match candidate {
        PatternTransformCandidate::Identity => LoweredCallback {
            program: PatternTransformProgram::new([]).map_err(|_| Declined)?,
            dependencies: LoweringDependencies::default(),
            sidecars: Vec::new(),
        },
        PatternTransformCandidate::MethodChain(calls) => lower_method_chain(ctx, calls, depth + 1)?,
    };
    let program = {
        let mut gate = state.gate.borrow_mut();
        if !gate.admits(&lowered.dependencies) {
            return Err(Declined);
        }
        let standings = gate.issue(&lowered.dependencies, mode == PatternTransformIrMode::Auto);
        lowered.program.with_standings(standings)
    };
    state.count(|stats| stats.lowered = stats.lowered.saturating_add(1));
    let function = if mode == PatternTransformIrMode::Auto {
        rustel_core::value::FunctionRef::lowered_js(id, program)
    } else {
        rustel_core::value::FunctionRef::js_pattern_ir(id, program)
    };
    let mut sidecars = vec![sidecar];
    sidecars.extend(lowered.sidecars);
    Ok((
        rustel_core::pure(Value::Function(Box::new(function))),
        sidecars,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, arguments: Vec<CallbackArgument>) -> CallbackMethodCall {
        CallbackMethodCall {
            method: method.into(),
            arguments: arguments.into_boxed_slice(),
        }
    }

    fn mini(source: &str, offset: i64) -> CallbackArgument {
        CallbackArgument::Mini {
            source: source.into(),
            offset,
        }
    }

    #[test]
    fn a_chain_of_proven_built_ins_records_what_it_reads() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime
            .install_semantic_bindings()
            .expect("semantic bindings");
        with_ctx(&runtime, |ctx| {
            let Ok(lowered) = lower_method_chain(
                &ctx,
                &[
                    call("fast", vec![CallbackArgument::Number(2.0)]),
                    call("s", vec![mini("bd sd", 4)]),
                    call("add", vec![CallbackArgument::Number(-0.0)]),
                ],
                0,
            ) else {
                panic!("a chain of built-ins compiles");
            };
            assert_eq!(lowered.program.len(), 3);
            assert_eq!(
                lowered.dependencies,
                LoweringDependencies {
                    methods: vec!["fast".into(), "s".into(), "add".into()],
                    mini_callee: true,
                    raw_strings: false,
                }
            );
            let Ok(lowered) = lower_method_chain(
                &ctx,
                &[call("s", vec![CallbackArgument::String("bd".into())])],
                0,
            ) else {
                panic!("a raw string compiles");
            };
            assert!(lowered.dependencies.raw_strings);
        });
    }

    #[test]
    fn a_patterned_mini_argument_declines_every_kind_of_method() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime
            .install_semantic_bindings()
            .expect("semantic bindings");
        with_ctx(&runtime, |ctx| {
            for (method, source) in [
                ("struct", "x(<3 5>,8)"),
                ("mask", "1(3,<8 16>)"),
                ("s", "bd(<3 5>,8)"),
                ("gain", "1(3,8,<0 2>)"),
                ("n", "<0 2> .. 3"),
                ("add", "0 .. <2 4>"),
                ("fast", "1(<3 5>,8)"),
            ] {
                assert!(
                    lower_method_chain(&ctx, &[call(method, vec![mini(source, 0)])], 0).is_err(),
                    "{method}({source})"
                );
                assert!(
                    lower_method_chain(&ctx, &[call(method, vec![mini("1 2", 0)])], 0).is_ok(),
                    "{method} with a pure argument"
                );
            }
        });
    }

    #[test]
    fn an_unannounced_change_to_an_intrinsic_declines_every_chain() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime
            .install_semantic_bindings()
            .expect("semantic bindings");
        runtime
            .eval("globalThis.Object = { keys() { return [] } }")
            .expect("replace Object");
        with_ctx(&runtime, |ctx| {
            assert!(lower_method_chain(&ctx, &[call("rev", vec![])], 0).is_err());
        });
    }

    #[test]
    fn a_chain_that_would_throw_or_leave_the_built_ins_is_declined() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime
            .install_semantic_bindings()
            .expect("semantic bindings");
        with_ctx(&runtime, |ctx| {
            for (name, calls) in [
                ("an empty one-input call", vec![call("fast", vec![])]),
                (
                    "an extra argument",
                    vec![call("rev", vec![CallbackArgument::Number(1.0)])],
                ),
                ("an empty composer call", vec![call("add", vec![])]),
                ("an empty shortcut call", vec![call("struct", vec![])]),
                ("an unknown method", vec![call("nope", vec![])]),
                ("a device route", vec![call("midi", vec![])]),
                (
                    "a lane",
                    vec![call("p", vec![CallbackArgument::Number(1.0)])],
                ),
                (
                    "a raw method",
                    vec![call("_fast", vec![CallbackArgument::Number(2.0)])],
                ),
                (
                    "mini-notation that does not parse",
                    vec![call("struct", vec![mini("[", 0)])],
                ),
                (
                    "more calls than a program holds",
                    std::iter::repeat_with(|| call("rev", vec![]))
                        .take(rustel_core::callback_ir::MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS + 1)
                        .collect(),
                ),
            ] {
                assert!(lower_method_chain(&ctx, &calls, 0).is_err(), "{name}");
            }
        });
    }
}
