//! The built-in Pattern methods a callback can be compiled against.
//!
//! Installation records each method it puts on the Pattern prototype,
//! together with what the method does natively. The seal keeps a name only
//! when its prototype slot still holds the recorded object after every
//! installation layer has run, and keeps the slots in a private root. A proof
//! compares the slot the prototype holds now with that private record. A tag
//! on the method itself would be an ordinary property that score code can
//! copy or forge, so it would prove nothing.

use super::*;
use rustel_core::compose::{Alignment, ComposeOp};
use rustel_core::controls::ControlSpec;
use rustel_core::register::Registration;
use rustel_core::value::FunctionRef;

/// The global the transpiler calls for every mini-notation literal.
const MINI_CALLEE: &str = "m";

/// The intrinsics that every built-in method reads when it shapes its
/// arguments, as the holder each one lives on and its name there.
const INTRINSICS: [(Option<&str>, &str); 3] = [
    (None, "Object"),
    (None, "Array"),
    (Some("Array"), "isArray"),
];

/// What one built-in method does with its receiver and arguments.
#[derive(Clone)]
pub(crate) enum BuiltinMethod {
    /// A registered combinator, installed by `install_combinator`.
    Registered(Box<Registration>),
    /// A control setter.
    Control(ControlSpec),
    /// A composition shortcut such as `struct`, or a bare alignment.
    Shortcut { op: ComposeOp, how: Alignment },
    /// A composer accessor such as `add`, called with the default alignment.
    Composer(ComposeOp),
}

impl BuiltinMethod {
    fn is_accessor(&self) -> bool {
        matches!(self, Self::Composer(_))
    }

    /// The native transformer that builds what `receiver.name(...arguments)`
    /// builds. `None` where the JavaScript method throws for these arguments.
    ///
    /// Each transformer calls the same native operation as the method, with
    /// the same arguments, so both build the same nodes, contexts and steps.
    /// A transformer claims to keep a pure receiver pure only when every
    /// argument pattern is pure. A mini-notation argument such as
    /// `"x(<3 5>,8)"` is not, and a program refuses such a transformer.
    pub(crate) fn transformer(&self, name: &str, arguments: Vec<Pattern>) -> Option<FunctionRef> {
        match self {
            Self::Registered(registration) => {
                let arguments = registered_method_arguments(registration, arguments)?;
                Some(bound_registered_transformer(
                    name,
                    Registration::clone(registration),
                    arguments,
                ))
            }
            Self::Control(spec) => {
                let spec = spec.clone();
                // A control composes pure operands into a pure result.
                let provable_pure = arguments.iter().all(Pattern::is_pure);
                let arguments = control_arguments(&spec, arguments, |_| false);
                Some(FunctionRef::registered(
                    name,
                    std::sync::Arc::new(move |pattern| {
                        apply_control_method(&spec, &pattern, arguments.clone())
                    }),
                    provable_pure,
                ))
            }
            Self::Shortcut { op, how } => {
                let (op, how) = (*op, *how);
                let other = (!arguments.is_empty()).then(|| sequence_patterns(arguments))?;
                let provable_pure = other.is_pure();
                Some(FunctionRef::registered(
                    name,
                    std::sync::Arc::new(move |pattern| {
                        rustel_core::compose::compose(&pattern, &other, op, how)
                    }),
                    provable_pure,
                ))
            }
            Self::Composer(op) => {
                let op = *op;
                let other = (!arguments.is_empty()).then(|| sequence_patterns(arguments))?;
                let provable_pure = other.is_pure();
                // The accessor reads the default alignment when it is called.
                Some(FunctionRef::registered(
                    name,
                    std::sync::Arc::new(move |pattern| {
                        rustel_core::compose::compose(
                            &pattern,
                            &other,
                            op,
                            rustel_core::compose::default_alignment(),
                        )
                    }),
                    provable_pure,
                ))
            }
        }
    }
}

/// The built-in methods recorded by installation.
#[derive(Default)]
pub(crate) struct BuiltinMethods {
    methods: BTreeMap<Box<str>, BuiltinMethod>,
    sealed: bool,
}

/// Whether a prototype slot named `name` can be compiled against. The
/// wrapper's own `polyJoin` and `query` fields shadow those slots, and a raw
/// `_name` method takes its arguments unshaped.
fn compilable(name: &str) -> bool {
    !name.starts_with('_') && !matches!(name, "polyJoin" | "query")
}

/// The private record of recorded slots, by name.
fn builtin_slots<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    let stack = host_stack(ctx)?;
    let existing: rquickjs::Value = stack.as_object().get(BUILTIN_SLOTS)?;
    if let Some(slots) = existing.into_object() {
        return Ok(slots);
    }
    let slots = rquickjs::Object::new(ctx.clone())?;
    slots.set_prototype(None)?;
    stack.as_object().set(BUILTIN_SLOTS, slots.clone())?;
    Ok(slots)
}

/// Record that installation put `slot` on the Pattern prototype as `name`.
/// A later record of the same name replaces this one, as the later
/// installation replaces the slot.
pub(crate) fn record_builtin_method<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    slot: rquickjs::Value<'js>,
    method: BuiltinMethod,
) -> rquickjs::Result<()> {
    if !compilable(name) {
        return Ok(());
    }
    let methods = host_builtin_methods(ctx)?;
    if methods.borrow().sealed {
        return Ok(());
    }
    builtin_slots(ctx)?.set(name, slot)?;
    methods.borrow_mut().methods.insert(name.into(), method);
    Ok(())
}

/// Record `alias` as the recorded method `canonical`, when `slot` is that
/// method's recorded slot.
pub(crate) fn record_builtin_alias<'js>(
    ctx: &Ctx<'js>,
    alias: &str,
    canonical: &str,
    slot: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    let method = host_builtin_methods(ctx)?
        .borrow()
        .methods
        .get(canonical)
        .cloned();
    let Some(method) = method else {
        return Ok(());
    };
    let recorded: rquickjs::Value = builtin_slots(ctx)?.get(canonical)?;
    if recorded != slot {
        return Ok(());
    }
    record_builtin_method(ctx, alias, slot, method)
}

/// Record the global `m` that installation defined.
pub(crate) fn record_mini_callee<'js>(
    ctx: &Ctx<'js>,
    function: &Function<'js>,
) -> rquickjs::Result<()> {
    host_stack(ctx)?
        .as_object()
        .set(BUILTIN_MINI_CALLEE, function.clone())
}

/// Whether the own property `name` of `object` is the recorded slot: the
/// same object, as the same kind of property, with no setter.
fn holds<'js>(
    ctx: &Ctx<'js>,
    object: &rquickjs::Object<'js>,
    name: &str,
    accessor: bool,
    slot: &rquickjs::Value<'js>,
) -> rquickjs::Result<bool> {
    let Some(property) = own_property(ctx, object, name)? else {
        return Ok(false);
    };
    Ok(if accessor {
        property.is_accessor() && property.getter == *slot && property.setter.is_undefined()
    } else {
        !property.is_accessor() && property.value == *slot
    })
}

/// Finish the inventory once every installation layer has run.
///
/// A name stays only while its prototype slot is the object recorded last
/// for it, so a method that a later layer replaced drops out. The gate opens
/// with the seal.
pub(crate) fn seal_builtin_methods<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> rquickjs::Result<()> {
    let slots = builtin_slots(ctx)?;
    let methods = host_builtin_methods(ctx)?;
    let mut methods = methods.borrow_mut();
    let mut replaced = Vec::new();
    for (name, method) in &methods.methods {
        let slot: rquickjs::Value = slots.get(&**name)?;
        if !holds(ctx, proto, name, method.is_accessor(), &slot)? {
            replaced.push(name.clone());
        }
    }
    for name in replaced {
        methods.methods.remove(&name);
        slots.remove(&*name)?;
    }
    let stack = host_stack(ctx)?;
    let mini: rquickjs::Value = stack.as_object().get(BUILTIN_MINI_CALLEE)?;
    if mini.is_undefined() || !holds(ctx, globals, MINI_CALLEE, false, &mini)? {
        stack.as_object().remove(BUILTIN_MINI_CALLEE)?;
    }
    let intrinsics = rquickjs::Object::new(ctx.clone())?;
    intrinsics.set_prototype(None)?;
    for (holder, name) in INTRINSICS {
        let value: rquickjs::Value = match holder {
            Some(holder) => match globals.get::<_, rquickjs::Value>(holder)?.into_object() {
                Some(holder) => holder.get(name)?,
                None => rquickjs::Value::new_undefined(ctx.clone()),
            },
            None => globals.get(name)?,
        };
        intrinsics.set(name, value)?;
    }
    stack.as_object().set(BUILTIN_INTRINSICS, intrinsics)?;
    methods.sealed = true;
    drop(methods);
    host_callback_ir(ctx)?.gate.borrow_mut().seal();
    Ok(())
}

/// The built-in method the Pattern prototype holds as `name` now, if it is
/// still the sealed one.
pub(crate) fn prove_builtin_method(ctx: &Ctx<'_>, name: &str) -> Option<BuiltinMethod> {
    let methods = host_builtin_methods(ctx).ok()?;
    let methods = methods.borrow();
    if !methods.sealed {
        return None;
    }
    let method = methods.methods.get(name)?;
    let slot: rquickjs::Value = builtin_slots(ctx).ok()?.get(name).ok()?;
    let proto = rquickjs::Class::<NativePatternWrapper>::prototype(ctx).ok()??;
    holds(ctx, &proto, name, method.is_accessor(), &slot)
        .ok()?
        .then(|| method.clone())
}

/// Whether the global `m` is still the sealed mini-notation callee.
pub(crate) fn prove_mini_callee(ctx: &Ctx<'_>) -> bool {
    let sealed = host_builtin_methods(ctx).is_ok_and(|methods| methods.borrow().sealed);
    let Ok(stack) = host_stack(ctx) else {
        return false;
    };
    let Ok(mini) = stack
        .as_object()
        .get::<_, rquickjs::Value>(BUILTIN_MINI_CALLEE)
    else {
        return false;
    };
    sealed
        && !mini.is_undefined()
        && holds(ctx, &ctx.globals(), MINI_CALLEE, false, &mini).unwrap_or(false)
}

/// Whether the intrinsics that argument shaping reads are still the sealed
/// ones, each an own data property of its holder.
pub(crate) fn prove_surface_intrinsics(ctx: &Ctx<'_>) -> bool {
    let sealed = host_builtin_methods(ctx).is_ok_and(|methods| methods.borrow().sealed);
    let Some(recorded) = host_stack(ctx).ok().and_then(|stack| {
        stack
            .as_object()
            .get::<_, rquickjs::Value>(BUILTIN_INTRINSICS)
            .ok()?
            .into_object()
    }) else {
        return false;
    };
    let globals = ctx.globals();
    sealed
        && INTRINSICS.into_iter().all(|(holder, name)| {
            let Ok(slot) = recorded.get::<_, rquickjs::Value>(name) else {
                return false;
            };
            let holder = match holder {
                None => Some(globals.clone()),
                Some(holder) => recorded
                    .get::<_, rquickjs::Value>(holder)
                    .ok()
                    .and_then(rquickjs::Value::into_object),
            };
            !slot.is_undefined()
                && holder
                    .is_some_and(|holder| holds(ctx, &holder, name, false, &slot).unwrap_or(false))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> JsRuntime {
        let runtime = JsRuntime::new().expect("runtime");
        runtime
            .install_semantic_bindings()
            .expect("semantic bindings");
        runtime
    }

    #[test]
    fn the_seal_proves_the_methods_the_benchmark_callbacks_call() {
        let runtime = runtime();
        with_ctx(&runtime, |ctx| {
            for name in [
                "rev", "fast", "gain", "add", "struct", "s", "vib", "fm", "fmh", "detune", "crush",
                "speed", "mask", "squeeze", "set", "when", "clip", "ply",
            ] {
                assert!(
                    prove_builtin_method(&ctx, name).is_some(),
                    "{name} is not a sealed built-in"
                );
            }
            assert!(prove_mini_callee(&ctx));
            for name in [
                "query",
                "polyJoin",
                "_fast",
                "p",
                "constructor",
                "withValue",
            ] {
                assert!(
                    prove_builtin_method(&ctx, name).is_none(),
                    "{name} must not be compilable"
                );
            }
        });
    }

    #[test]
    fn a_replaced_slot_or_a_copied_tag_fails_the_proof() {
        let runtime = runtime();
        runtime
            .eval(
                "Pattern.prototype.rev = Pattern.prototype.fast; \
                 const forged = (x) => x; forged.__rustel_combinator = fast.__rustel_combinator; \
                 Pattern.prototype.fast = forged; \
                 Object.defineProperty(Pattern.prototype, 'add', { get() { return () => 1; } }); \
                 globalThis.m = mini;",
            )
            .expect("replace slots");
        with_ctx(&runtime, |ctx| {
            for name in ["rev", "fast", "add"] {
                assert!(prove_builtin_method(&ctx, name).is_none(), "{name}");
            }
            assert!(prove_builtin_method(&ctx, "slow").is_some());
            assert!(!prove_mini_callee(&ctx));
        });
    }

    #[test]
    fn the_seal_proves_the_intrinsics_until_one_is_replaced() {
        for replace in [
            "globalThis.Object = { keys() { return [] } }",
            "Array.isArray = () => false",
            "globalThis.Array = function Array() {}",
        ] {
            let runtime = runtime();
            assert!(with_ctx(&runtime, |ctx| prove_surface_intrinsics(&ctx)));
            runtime.eval(replace).expect("replace an intrinsic");
            assert!(
                !with_ctx(&runtime, |ctx| prove_surface_intrinsics(&ctx)),
                "{replace}"
            );
        }
    }

    #[test]
    fn nothing_is_proven_before_the_seal() {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime, |ctx| {
            assert!(prove_builtin_method(&ctx, "rev").is_none());
            assert!(!prove_mini_callee(&ctx));
        });
    }
}
