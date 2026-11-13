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
use rustel_core::callback_ir::PatternTransformInstruction;
use rustel_core::compose::{Alignment, ComposeOp};
use rustel_core::controls::ControlSpec;
use rustel_core::register::Registration;
use rustel_core::value::FunctionRef;

/// The global the transpiler calls for every mini-notation literal.
pub(crate) const MINI_CALLEE: &str = "m";

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
    /// `pick` or one of its kin, given one lookup of patterns by key.
    Pick {
        index: rustel_core::PickIndexMode,
        join: rustel_core::JoinMode,
    },
    /// A visual such as `pianoroll`, given the widget id and the slot the
    /// transpiler writes for it.
    Visual,
    /// `layer` or `superimpose`, given transformers: a stack of what each
    /// one makes of the receiver, with or without the receiver itself.
    Layer { keep_original: bool },
}

impl BuiltinMethod {
    fn is_accessor(&self) -> bool {
        matches!(self, Self::Composer(_))
    }

    /// What `receiver.name(...arguments)` builds, as the method builds it.
    /// `None` where the JavaScript method throws for these arguments, and
    /// for a method whose arguments are not patterns.
    pub(crate) fn apply(&self, receiver: &Pattern, arguments: Vec<Pattern>) -> Option<Pattern> {
        Some(match self {
            Self::Registered(registration) => {
                let arguments = registered_method_arguments(registration, arguments)?;
                registration.call(&arguments, receiver.clone())
            }
            Self::Control(spec) => apply_control_method(
                spec,
                receiver,
                control_arguments(spec, arguments, |_| false),
            ),
            Self::Shortcut { op, how } => {
                let other = (!arguments.is_empty()).then(|| sequence_patterns(arguments))?;
                rustel_core::compose::compose(receiver, &other, *op, *how)
            }
            // The accessor reads the default alignment when it is called.
            Self::Composer(op) => {
                let other = (!arguments.is_empty()).then(|| sequence_patterns(arguments))?;
                rustel_core::compose::compose(
                    receiver,
                    &other,
                    *op,
                    rustel_core::compose::default_alignment(),
                )
            }
            Self::Pick { .. } | Self::Visual | Self::Layer { .. } => return None,
        })
    }

    /// The step of a callback program that builds what
    /// `receiver.name(...arguments)` builds. `None` as for
    /// [`Self::transformer`].
    ///
    /// A registered method whose function slot a transformer fills is its
    /// own step: the registration applies the argument patterns exactly as
    /// its method call does, so the program builds the graph the call
    /// builds.
    pub(crate) fn step(
        &self,
        name: &str,
        arguments: Vec<Pattern>,
        holds_transformer: bool,
    ) -> Option<PatternTransformInstruction> {
        Some(match self {
            Self::Registered(registration) if holds_transformer => {
                PatternTransformInstruction::ApplyRegistered {
                    arguments: registered_method_arguments(registration, arguments)?,
                    registration: registration.clone(),
                }
            }
            _ => PatternTransformInstruction::Apply(self.transformer(name, arguments)?),
        })
    }

    /// The native transformer that builds what `receiver.name(...arguments)`
    /// builds. `None` where the JavaScript method throws for these arguments,
    /// and for a method whose arguments are not patterns.
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
            Self::Pick { .. } | Self::Visual | Self::Layer { .. } => None,
        }
    }
}

/// A global that a score can use without JavaScript.
#[derive(Clone)]
pub(crate) enum BuiltinGlobal {
    /// A free control such as `s`, applied to one value. The control
    /// registry holds its spec under the same name.
    Control,
    /// A list constructor such as `stack` or `cat`.
    List(ListKind),
    /// A tempo setter, in cycles per second or per minute.
    Tempo { per_minute: bool },
    /// A signal such as `sine`: a pattern held as a value, not called.
    Signal(Pattern),
}

/// What a list constructor builds of its patterns.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ListKind {
    Stack,
    SlowCat,
    FastCat,
}

/// The built-in methods and globals recorded by installation.
#[derive(Default)]
pub(crate) struct BuiltinMethods {
    methods: BTreeMap<Box<str>, BuiltinMethod>,
    globals: BTreeMap<&'static str, BuiltinGlobal>,
    sealed: bool,
}

/// Whether a prototype slot named `name` can be recorded as `method`. The
/// wrapper's own `polyJoin` and `query` fields shadow those slots, and a raw
/// `_name` method takes its arguments unshaped. A visual's `_name` is the
/// inline form of the same visual.
fn recordable(name: &str, method: &BuiltinMethod) -> bool {
    !matches!(name, "polyJoin" | "query")
        && (!name.starts_with('_') || matches!(method, BuiltinMethod::Visual))
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
    if !recordable(name, &method) {
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

/// The private record of recorded global slots, by name.
fn global_slots<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    let stack = host_stack(ctx)?;
    let existing: rquickjs::Value = stack.as_object().get(BUILTIN_GLOBAL_SLOTS)?;
    if let Some(slots) = existing.into_object() {
        return Ok(slots);
    }
    let slots = rquickjs::Object::new(ctx.clone())?;
    slots.set_prototype(None)?;
    stack.as_object().set(BUILTIN_GLOBAL_SLOTS, slots.clone())?;
    Ok(slots)
}

/// Record that installation put `slot` on the globals as `name`. A later
/// record of the same name replaces this one, as the later installation
/// replaces the slot.
pub(crate) fn record_builtin_global<'js>(
    ctx: &Ctx<'js>,
    name: &'static str,
    slot: rquickjs::Value<'js>,
    global: BuiltinGlobal,
) -> rquickjs::Result<()> {
    global_slots(ctx)?.set(name, slot)?;
    host_builtin_methods(ctx)?
        .borrow_mut()
        .globals
        .insert(name, global);
    Ok(())
}

/// The global `name`, while it is still the slot installation recorded for
/// it: an own data property of the globals.
pub(crate) fn prove_builtin_global(ctx: &Ctx<'_>, name: &str) -> Option<BuiltinGlobal> {
    let methods = host_builtin_methods(ctx).ok()?;
    let methods = methods.borrow();
    if !methods.sealed {
        return None;
    }
    let global = methods.globals.get(name)?;
    let slot: rquickjs::Value = global_slots(ctx).ok()?.get(name).ok()?;
    (!slot.is_undefined() && holds(ctx, &ctx.globals(), name, false, &slot).ok()?)
        .then(|| global.clone())
}

/// What a fresh installation proves, one name a line: `method` or
/// `global`, the name, then what the name does, by its kind and the native
/// table that holds it. A test writes this file from an installed realm and
/// checks it against one, so the list cannot drift from the installation.
const NATIVE_INVENTORY: &str = include_str!("../../assets/native-inventory.txt");

/// The inventory asset by name, for one evaluation: what a realm with no
/// surface yet would prove once installed. A lookup searches the lines of
/// the asset, which are in order, and reads the native tables, so the list
/// holds no copy of either. A score calls the same few names many times, so
/// each name is looked up once and kept until the evaluation ends.
#[derive(Default)]
pub(crate) struct ListedNames {
    methods: RefCell<BTreeMap<Box<str>, Option<BuiltinMethod>>>,
    globals: RefCell<BTreeMap<Box<str>, Option<BuiltinGlobal>>>,
}

impl ListedNames {
    pub(crate) fn method(&self, name: &str) -> Option<BuiltinMethod> {
        if let Some(known) = self.methods.borrow().get(name) {
            return known.clone();
        }
        let method = listing("method", name).and_then(BuiltinMethod::listed);
        self.methods
            .borrow_mut()
            .insert(name.into(), method.clone());
        method
    }

    pub(crate) fn global(&self, name: &str) -> Option<BuiltinGlobal> {
        if let Some(known) = self.globals.borrow().get(name) {
            return known.clone();
        }
        let global =
            listing("global", name).and_then(|listing| BuiltinGlobal::listed(name, listing));
        self.globals
            .borrow_mut()
            .insert(name.into(), global.clone());
        global
    }
}

/// The section and the name of a line of the inventory, and what the line
/// lists for the name.
fn inventory_entry(line: &str) -> Option<((&str, &str), &str)> {
    let mut words = line.splitn(3, ' ');
    Some(((words.next()?, words.next()?), words.next()?))
}

/// What the inventory lists for `name` in `section`.
fn listing(section: &str, name: &str) -> Option<&'static str> {
    listing_in(NATIVE_INVENTORY, section, name)
}

/// What `text` lists for `name` in `section`, found by a search of its
/// lines, which are in the order of section and name.
fn listing_in<'t>(text: &'t str, section: &str, name: &str) -> Option<&'t str> {
    let (mut low, mut high) = (0, text.len());
    while low < high {
        let middle = low + (high - low) / 2;
        // The line that holds the byte at `middle`.
        let start = text[..middle].rfind('\n').map_or(0, |end| end + 1);
        let end = text[middle..]
            .find('\n')
            .map_or(text.len(), |end| middle + end);
        // A checkout can end each line with a carriage return too.
        let (entry, listing) = inventory_entry(text[start..end].trim_end_matches('\r'))?;
        match entry.cmp(&(section, name)) {
            std::cmp::Ordering::Equal => return Some(listing),
            std::cmp::Ordering::Less => low = end + 1,
            std::cmp::Ordering::Greater => high = start,
        }
    }
    None
}

impl BuiltinMethod {
    /// This method as the inventory lists it: its kind, then what holds it
    /// in the native tables. `None` for a registration no registry holds.
    #[cfg(test)]
    fn listing(&self) -> Option<String> {
        Some(match self {
            Self::Registered(registration) => {
                let name = registry().names().into_iter().find(|name| {
                    registry()
                        .get(name)
                        .is_some_and(|held| held.same_definition(registration))
                })?;
                format!("registered {name}")
            }
            Self::Control(spec) => format!("control {}", spec.name()),
            Self::Shortcut { op, how } => format!("shortcut {} {}", op.name(), how.name()),
            Self::Composer(op) => format!("composer {}", op.name()),
            Self::Pick { index, join } => format!("pick {index:?} {join:?}"),
            Self::Visual => "visual".into(),
            Self::Layer { keep_original } => {
                format!("layer {}", if *keep_original { "over" } else { "alone" })
            }
        })
    }

    /// The method a listing names, read from the native tables of this
    /// build. `None` when this build holds no such entry.
    fn listed(listing: &str) -> Option<Self> {
        use rustel_core::{JoinMode, PickIndexMode};
        let mut words = listing.split(' ');
        Some(match (words.next()?, words.next(), words.next()) {
            ("registered", Some(name), None) => {
                Self::Registered(Box::new(registry().get(name)?.clone()))
            }
            ("control", Some(name), None) => Self::Control(control_registry().get(name)?.clone()),
            ("shortcut", Some(op), Some(how)) => Self::Shortcut {
                op: ComposeOp::from_name(op)?,
                how: Alignment::from_name(how)?,
            },
            ("composer", Some(op), None) => Self::Composer(ComposeOp::from_name(op)?),
            ("pick", Some(index), Some(join)) => Self::Pick {
                index: [
                    PickIndexMode::Clamp,
                    PickIndexMode::Remainder,
                    PickIndexMode::Modulo,
                ]
                .into_iter()
                .find(|mode| format!("{mode:?}") == index)?,
                join: [
                    JoinMode::Inner,
                    JoinMode::Outer,
                    JoinMode::Mix,
                    JoinMode::Squeeze,
                    JoinMode::Reset,
                    JoinMode::Restart,
                ]
                .into_iter()
                .find(|mode| format!("{mode:?}") == join)?,
            },
            ("visual", None, None) => Self::Visual,
            ("layer", Some("over"), None) => Self::Layer {
                keep_original: true,
            },
            ("layer", Some("alone"), None) => Self::Layer {
                keep_original: false,
            },
            _ => return None,
        })
    }
}

impl BuiltinGlobal {
    /// This global as the inventory lists it.
    #[cfg(test)]
    fn listing(&self) -> String {
        match self {
            Self::Control => "control".into(),
            Self::List(kind) => format!("list {kind:?}"),
            Self::Tempo { per_minute: true } => "tempo minute".into(),
            Self::Tempo { per_minute: false } => "tempo second".into(),
            Self::Signal(_) => "signal".into(),
        }
    }

    /// The global `name` that a listing describes, read from the native
    /// tables of this build.
    fn listed(name: &str, listing: &str) -> Option<Self> {
        Some(match listing {
            "control" => {
                control_registry().get(name)?;
                Self::Control
            }
            "list Stack" => Self::List(ListKind::Stack),
            "list SlowCat" => Self::List(ListKind::SlowCat),
            "list FastCat" => Self::List(ListKind::FastCat),
            "tempo minute" => Self::Tempo { per_minute: true },
            "tempo second" => Self::Tempo { per_minute: false },
            "signal" => {
                // Built once: a signal read by two scores is one pattern.
                static SIGNALS: std::sync::OnceLock<Vec<(&'static str, Pattern)>> =
                    std::sync::OnceLock::new();
                let signals = SIGNALS.get_or_init(rustel_core::signal::signals);
                let (_, pattern) = signals.iter().find(|(signal, _)| *signal == name)?;
                Self::Signal(pattern.clone())
            }
            _ => return None,
        })
    }
}

/// What `ctx`, fully installed and before any score, proves: the text of
/// the inventory asset.
#[cfg(test)]
fn proven_inventory(ctx: &Ctx<'_>) -> String {
    let methods = host_builtin_methods(ctx).expect("the inventory");
    let methods = methods.borrow();
    let mut lines = Vec::new();
    for name in methods.methods.keys() {
        if let Some(method) = prove_builtin_method(ctx, name) {
            let listing = method
                .listing()
                .expect("a registry holds each registration");
            lines.push(format!("method {name} {listing}\n"));
        }
    }
    for name in methods.globals.keys() {
        if let Some(global) = prove_builtin_global(ctx, name) {
            lines.push(format!("global {name} {}\n", global.listing()));
        }
    }
    lines.sort();
    lines.concat()
}

/// Record the lane method `p` that installation put on the prototype.
pub(crate) fn record_lane_method<'js>(
    ctx: &Ctx<'js>,
    slot: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    host_stack(ctx)?.as_object().set(BUILTIN_LANE_METHOD, slot)
}

/// Whether the prototype's `p` is still the sealed lane method.
pub(crate) fn prove_lane_method(ctx: &Ctx<'_>) -> bool {
    let sealed = host_builtin_methods(ctx).is_ok_and(|methods| methods.borrow().sealed);
    let Some(slot) = host_stack(ctx).ok().and_then(|stack| {
        stack
            .as_object()
            .get::<_, rquickjs::Value>(BUILTIN_LANE_METHOD)
            .ok()
    }) else {
        return false;
    };
    let Some(proto) = rquickjs::Class::<NativePatternWrapper>::prototype(ctx)
        .ok()
        .flatten()
    else {
        return false;
    };
    sealed && !slot.is_undefined() && holds(ctx, &proto, "p", false, &slot).unwrap_or(false)
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
/// A name stays only while its slot is the object recorded last for it, so
/// a method or global that a later layer replaced drops out. The gate opens
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
    let slots = global_slots(ctx)?;
    let mut replaced = Vec::new();
    for name in methods.globals.keys() {
        let slot: rquickjs::Value = slots.get(*name)?;
        if !holds(ctx, globals, name, false, &slot)? {
            replaced.push(*name);
        }
    }
    for name in replaced {
        methods.globals.remove(name);
        slots.remove(name)?;
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

    /// Set `RUSTEL_WRITE_INVENTORY` to write the asset from this realm,
    /// in a build with every feature.
    #[test]
    fn the_inventory_asset_lists_what_a_fresh_installation_proves() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime.ensure_surface().expect("the whole surface");
        let proven = with_ctx(&runtime, |ctx| {
            assert!(prove_mini_callee(&ctx) && prove_lane_method(&ctx));
            assert!(prove_surface_intrinsics(&ctx));
            proven_inventory(&ctx)
        });
        if std::env::var_os("RUSTEL_WRITE_INVENTORY").is_some() {
            let asset = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/native-inventory.txt");
            std::fs::write(asset, &proven).expect("write the inventory asset");
        }
        // A lookup searches the lines, so they are in the order of section
        // and name, each name once.
        let entries: Vec<_> = NATIVE_INVENTORY
            .lines()
            .map(|line| {
                inventory_entry(line)
                    .expect("a section, a name and a listing")
                    .0
            })
            .collect();
        assert!(entries.windows(2).all(|pair| pair[0] < pair[1]));
        // Every entry this build can read is one the realm proves, with the
        // same meaning. A build with fewer features reads fewer entries.
        let listed = ListedNames::default();
        for line in NATIVE_INVENTORY.lines() {
            let mut words = line.splitn(3, ' ');
            let (section, name) = (words.next().expect("section"), words.next().expect("name"));
            let readable = match section {
                "method" => listed.method(name).is_some(),
                _ => listed.global(name).is_some(),
            };
            assert!(
                !readable || proven.lines().any(|proven| proven == line),
                "the realm does not prove `{line}`"
            );
        }
        // With every feature, the asset is the whole of what it proves.
        #[cfg(all(feature = "extensions", feature = "hydra"))]
        assert_eq!(
            proven, NATIVE_INVENTORY,
            "run this test with RUSTEL_WRITE_INVENTORY=1 to write the asset again"
        );
    }

    #[test]
    fn a_search_reads_lines_that_end_with_a_carriage_return() {
        let checkout = NATIVE_INVENTORY.replace('\n', "\r\n");
        for line in NATIVE_INVENTORY.lines() {
            let ((section, name), listed) =
                inventory_entry(line).expect("a section, a name and a listing");
            assert_eq!(listing_in(&checkout, section, name), Some(listed), "{line}");
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
