//! Compiling a recognised method chain against the sealed built-ins.
//!
//! The program calls the same native operation as each JavaScript method,
//! with the same arguments, so it builds the same graph. A chain that calls
//! anything but a proven built-in, or that would throw, is declined and stays
//! a QuickJS callback.

use super::*;
use rustel_core::callback_ir::{
    MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS, PatternTransformInstruction, PatternTransformProgram,
};
use rustel_transpiler::{CallbackArgument, CallbackMethodCall};

/// A compiled chain and what it reads as its callback would.
pub(crate) struct LoweredCallback {
    pub(crate) program: PatternTransformProgram,
    pub(crate) dependencies: LoweringDependencies,
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
pub(crate) fn lower_method_chain(
    ctx: &Ctx<'_>,
    calls: &[CallbackMethodCall],
) -> Result<LoweredCallback, Declined> {
    // Every built-in method shapes its arguments through `Object` and
    // `Array`. A change to them that no text or host function announced
    // keeps the chain JavaScript.
    if calls.len() > MAX_PATTERN_TRANSFORM_IR_INSTRUCTIONS || !prove_surface_intrinsics(ctx) {
        return Err(Declined);
    }
    let mut dependencies = LoweringDependencies::default();
    let mut instructions = Vec::new();
    instructions
        .try_reserve_exact(calls.len())
        .map_err(|_| Declined)?;
    for call in calls {
        let method = prove_builtin_method(ctx, &call.method).ok_or(Declined)?;
        let arguments = call
            .arguments
            .iter()
            .map(|argument| lower_argument(ctx, argument, &mut dependencies))
            .collect::<Result<Vec<_>, _>>()?;
        let function = method
            .transformer(&call.method, arguments)
            .ok_or(Declined)?;
        dependencies.methods.push(call.method.clone());
        instructions.push(PatternTransformInstruction::Apply(function));
    }
    // A step that cannot keep a pure operand pure is refused here.
    let program = PatternTransformProgram::new(instructions).map_err(|_| Declined)?;
    Ok(LoweredCallback {
        program,
        dependencies,
    })
}

/// The pattern a method receives for one literal argument, as the method
/// reifies it.
fn lower_argument(
    ctx: &Ctx<'_>,
    argument: &CallbackArgument,
    dependencies: &mut LoweringDependencies,
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
    })
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
                    lower_method_chain(&ctx, &[call(method, vec![mini(source, 0)])]).is_err(),
                    "{method}({source})"
                );
                assert!(
                    lower_method_chain(&ctx, &[call(method, vec![mini("1 2", 0)])]).is_ok(),
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
            assert!(lower_method_chain(&ctx, &[call("rev", vec![])]).is_err());
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
                assert!(lower_method_chain(&ctx, &calls).is_err(), "{name}");
            }
        });
    }
}
