use super::*;
use rquickjs::{
    Coerced, FromJs,
    class::{JsCell, JsClass, Readable},
    function::Params,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegistrationMode {
    Public,
    Canonical,
    RawOnly,
}

#[rquickjs::class]
#[derive(Trace, JsLifetime)]
struct SetupState<'js> {
    pattern: Function<'js>,
    pure: Function<'js>,
    fastcat: Function<'js>,
    proto: rquickjs::Object<'js>,
    scope: rquickjs::Object<'js>,
    raw_handler: rquickjs::Object<'js>,
    string_parser: rquickjs::Value<'js>,
    /// What each name a score's `register()` took meant before it: the
    /// prototype method, the raw `_name` method, and the scope entry.
    /// Emptied once the surface has been given back.
    displaced: rquickjs::Object<'js>,
    #[qjs(skip_trace)]
    sealed: Rc<RefCell<HashSet<String>>>,
}

/// Where the score realm keeps what a `register()` displaced, so the next
/// score can be handed the surface it started from.
const DISPLACED_STATE: &str = "__rustel_registration_displaced";
/// The Pattern prototype, the score scope, and the record itself.
const DISPLACED_PROTO: usize = 0;
const DISPLACED_SCOPE: usize = 1;
const DISPLACED_RECORD: usize = 2;
/// What one displaced name meant before: its prototype method, its raw
/// `_name` method, and its entry in the score scope. `undefined` in all
/// three is a name nothing answered to.
const DISPLACED_METHOD: usize = 0;
const DISPLACED_RAW: usize = 1;
const DISPLACED_SCOPED: usize = 2;

/// Write down what `name` meant before this score took it - once. A score
/// that registers the same name twice displaced the engine's only the
/// first time, and the second write must not record its own work as the
/// original.
fn remember_displaced<'js>(
    ctx: &Ctx<'js>,
    state: &rquickjs::Class<'js, SetupState<'js>>,
    name: &str,
) -> rquickjs::Result<()> {
    let (proto, scope, displaced) = {
        let state = state.borrow();
        (
            state.proto.clone(),
            state.scope.clone(),
            state.displaced.clone(),
        )
    };
    if displaced.contains_key(name)? {
        return Ok(());
    }
    let before = rquickjs::Array::new(ctx.clone())?;
    before.set(DISPLACED_METHOD, proto.get::<_, rquickjs::Value>(name)?)?;
    before.set(
        DISPLACED_RAW,
        proto.get::<_, rquickjs::Value>(format!("_{name}"))?,
    )?;
    before.set(DISPLACED_SCOPED, scope.get::<_, rquickjs::Value>(name)?)?;
    displaced.set(name, before)?;
    Ok(())
}

fn displaced_state<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Array<'js>>> {
    let stack = host_stack(ctx)?;
    let state: rquickjs::Value = stack.as_object().get(DISPLACED_STATE)?;
    Ok(state.into_array())
}

/// Put a name back the way the engine had it, or take it away if the
/// engine never had it.
///
/// The value goes back as an own data property - writable, enumerable and
/// configurable, as the engine's `set` bound it - so no setter or getter on
/// the prototype chain runs. A score turn restores before its deadline
/// starts, so this runs no score code.
fn put_back<'js>(
    target: &rquickjs::Object<'js>,
    name: &str,
    original: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    target.remove(name)?;
    if !original.is_undefined() {
        target.prop(
            name,
            rquickjs::object::Property::from(original)
                .writable()
                .enumerable()
                .configurable(),
        )?;
    }
    Ok(())
}

/// Give the surface back the names the last score's `register()` calls
/// TOOK from it.
///
/// `register()` writes onto the shared Pattern prototype and the score
/// scope, and both outlive the evaluation that wrote them. Which is what a
/// score wants for a name it INVENTS: declaring a helper in one update and
/// calling it from the next is how the studio is played, so a name nothing
/// answered to before stays, and this is the point at which it becomes part
/// of the surface every later score starts from.
///
/// A name the score DISPLACED is the other half. `register('fill', …)`
/// borrows a name the engine already had, and deleting that line and
/// playing again has to mean what it says: the engine's `fill` answers
/// again. Nothing else in a live-coded file needs a restart to undo, and
/// this did.
pub(crate) fn restore_surface(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    let Some(state) = displaced_state(ctx)? else {
        return Ok(());
    };
    let proto: rquickjs::Object = state.get(DISPLACED_PROTO)?;
    let scope: rquickjs::Object = state.get(DISPLACED_SCOPE)?;
    let record: rquickjs::Object = state.get(DISPLACED_RECORD)?;
    let taken = record
        .own_props::<String, rquickjs::Array>(rquickjs::object::Filter::new().string())
        .collect::<rquickjs::Result<Vec<_>>>()?;
    for (name, before) in taken {
        // Spent either way: what the name meant before this score is either
        // being put back now, or is what the score itself declared and is
        // the surface from here on.
        record.remove(&name)?;
        let method: rquickjs::Value = before.get(DISPLACED_METHOD)?;
        let raw: rquickjs::Value = before.get(DISPLACED_RAW)?;
        let scoped: rquickjs::Value = before.get(DISPLACED_SCOPED)?;
        if method.is_undefined() && raw.is_undefined() && scoped.is_undefined() {
            continue;
        }
        put_back(&proto, &name, method)?;
        put_back(&proto, &format!("_{name}"), raw)?;
        put_back(&scope, &name, scoped)?;
    }
    Ok(())
}

/// Take what has been registered so far as the surface every score starts
/// from, including a prelude's replacements. A prelude prepares the surface
/// for the scores after it, so its replacements stay.
pub(crate) fn forget_surface(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    let Some(state) = displaced_state(ctx)? else {
        return Ok(());
    };
    let record: rquickjs::Object = state.get(DISPLACED_RECORD)?;
    let names = record
        .own_keys::<String>(rquickjs::object::Filter::new().string())
        .collect::<rquickjs::Result<Vec<_>>>()?;
    for name in names {
        record.remove(name)?;
    }
    Ok(())
}

fn reify_value<'js>(
    ctx: &Ctx<'js>,
    state: &rquickjs::Class<'js, SetupState<'js>>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (pattern, parser, pure) = {
        let state = state.borrow();
        (
            state.pattern.clone(),
            state.string_parser.clone(),
            state.pure.clone(),
        )
    };
    if let Some(object) = value.as_object() {
        if object.is_instance_of(&pattern) {
            return Ok(value);
        }
        let marker: rquickjs::Value = object.get("_Pattern")?;
        if common::truthy(ctx, marker)? {
            return Ok(value);
        }
    }
    if value.is_string() && common::truthy(ctx, parser.clone())? {
        let parser = parser
            .into_function()
            .ok_or_else(|| throw_type_error(ctx, "stringParser is not a function"))?;
        return parser.call((value,));
    }
    pure.call((value,))
}

#[derive(Trace, JsLifetime)]
struct NativeReify<'js> {
    state: rquickjs::Class<'js, SetupState<'js>>,
}

impl<'js> JsClass<'js> for NativeReify<'js> {
    const NAME: &'static str = "NativeReify";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let state = this.borrow().state.clone();
        reify_value(
            params.ctx(),
            &state,
            params
                .arg(0)
                .unwrap_or_else(|| common::undefined(params.ctx())),
        )
    }
}

#[derive(Trace, JsLifetime)]
struct NativeSetStringParser<'js> {
    state: rquickjs::Class<'js, SetupState<'js>>,
}

impl<'js> JsClass<'js> for NativeSetStringParser<'js> {
    const NAME: &'static str = "NativeSetStringParser";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let state = this.borrow().state.clone();
        let parser = params
            .arg(0)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        state.borrow_mut().string_parser = parser.clone();
        Ok(parser)
    }
}

fn sequence<'js>(
    ctx: &Ctx<'js>,
    state: &rquickjs::Class<'js, SetupState<'js>>,
    values: Vec<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if values.len() == 1 {
        return reify_value(ctx, state, values.into_iter().next().expect("one value"));
    }
    let fastcat = state.borrow().fastcat.clone();
    common::call(&fastcat, common::undefined(ctx), values)
}

#[derive(Trace, JsLifetime)]
struct NativeRegisteredBody<'js> {
    function: Function<'js>,
    pattern: rquickjs::Value<'js>,
}

impl<'js> JsClass<'js> for NativeRegisteredBody<'js> {
    const NAME: &'static str = "NativeRegisteredBody";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (function, pattern) = {
            let state = this.borrow();
            (state.function.clone(), state.pattern.clone())
        };
        let values = params
            .arg(0)
            .and_then(rquickjs::Value::into_array)
            .ok_or_else(|| throw_type_error(params.ctx(), "registered values are not an array"))?;
        // The array's `length` is a JS-controlled claim, not a count of
        // backed elements: reserve under the heap-ceiling bound rather than
        // trusting it, so a sparse length cannot abort the host.
        let len = js_array_len(&values)?;
        let mut arguments = reserve_js_array(params.ctx(), len.saturating_add(1))?;
        for index in 0..len {
            arguments.push(values.get(index)?);
        }
        arguments.push(pattern);
        common::call(&function, common::undefined(params.ctx()), arguments)
    }
}

fn map_registered<'js>(
    ctx: &Ctx<'js>,
    values: &[rquickjs::Value<'js>],
    callable: Function<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let mut patterns = Vec::with_capacity(values.len());
    let mut sidecars = Vec::with_capacity(values.len() + 1);
    for value in values {
        let (pattern, sidecar) = reify_bridged(ctx, value)?;
        patterns.push(pattern);
        sidecars.push(sidecar);
    }
    let Some(first) = patterns.first().cloned() else {
        return Ok(derive_wrapper(ctx.clone(), rustel_core::silence(), &sidecars)?.into_value());
    };
    let mut accumulated = first.fmap_collect();
    for pattern in patterns.into_iter().skip(1) {
        accumulated = accumulated.app_left_collect(pattern);
    }
    let (id, sidecar) = bridge_callable(ctx, callable)?;
    sidecars.push(sidecar);
    Ok(derive_wrapper(
        ctx.clone(),
        rustel_core::pattern_of_js(accumulated, id),
        &sidecars,
    )?
    .into_value())
}

#[derive(Trace, JsLifetime)]
struct NativeRegistered<'js> {
    state: rquickjs::Class<'js, SetupState<'js>>,
    function: Function<'js>,
    join: Option<rquickjs::Value<'js>>,
    #[qjs(skip_trace)]
    arity: f64,
    #[qjs(skip_trace)]
    patternify: bool,
    #[qjs(skip_trace)]
    preserve_steps: bool,
}

impl<'js> JsClass<'js> for NativeRegistered<'js> {
    const NAME: &'static str = "NativeRegistered";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (state, function, join, arity, patternify, preserve_steps) = {
            let registered = this.borrow();
            (
                registered.state.clone(),
                registered.function.clone(),
                registered.join.clone(),
                registered.arity,
                registered.patternify,
                registered.preserve_steps,
            )
        };
        let mut arguments = Vec::with_capacity(params.len());
        for index in 0..params.len() {
            arguments.push(reify_value(
                params.ctx(),
                &state,
                params.arg(index).expect("argument exists"),
            )?);
        }
        let pattern = arguments
            .last()
            .cloned()
            .unwrap_or_else(|| common::undefined(params.ctx()));
        let result = if patternify {
            if arity == 1.0 {
                common::call(
                    &function,
                    common::undefined(params.ctx()),
                    [pattern.clone()],
                )?
            } else {
                let first = if arguments.is_empty() {
                    &[][..]
                } else {
                    &arguments[..arguments.len() - 1]
                };
                let mut pure_values = Vec::with_capacity(first.len());
                let mut locations = Vec::new();
                let mut all_pure = true;
                for argument in first {
                    let pure = common::get(params.ctx(), argument.clone(), "__pure")?;
                    if pure.is_undefined() || pure.is_null() {
                        all_pure = false;
                        break;
                    }
                    pure_values.push(pure);
                    let location = common::get(params.ctx(), argument.clone(), "__pure_loc")?;
                    if common::truthy(params.ctx(), location.clone())? {
                        locations.push(location);
                    }
                }
                if all_pure {
                    pure_values.push(pattern.clone());
                    let result =
                        common::call(&function, common::undefined(params.ctx()), pure_values)?;
                    pattern_surface::with_locations(
                        params.ctx(),
                        result,
                        common::array(params.ctx(), locations)?.into_value(),
                    )?
                } else {
                    let body = rquickjs::Class::instance(
                        params.ctx().clone(),
                        NativeRegisteredBody {
                            function,
                            pattern: pattern.clone(),
                        },
                    )?
                    .into_value()
                    .into_function()
                    .expect("a callable class is a function");
                    common::configure(&body, "", 1, false)?;
                    let mapped = map_registered(params.ctx(), first, body)?;
                    if let Some(join) = join {
                        let join = join.into_function().ok_or_else(|| {
                            throw_type_error(params.ctx(), "join is not a function")
                        })?;
                        join.call((mapped,))?
                    } else {
                        common::call_method(params.ctx(), mapped, "innerJoin", [])?
                    }
                }
            }
        } else {
            common::call(&function, common::undefined(params.ctx()), arguments)?
        };
        if preserve_steps {
            pattern_surface::copy_steps(params.ctx(), result, pattern)
        } else {
            Ok(result)
        }
    }
}

#[derive(Trace, JsLifetime)]
struct NativeRegisteredMethod<'js> {
    state: rquickjs::Class<'js, SetupState<'js>>,
    registered: Function<'js>,
    #[qjs(skip_trace)]
    arity: f64,
    #[qjs(skip_trace)]
    name: String,
}

impl<'js> JsClass<'js> for NativeRegisteredMethod<'js> {
    const NAME: &'static str = "NativeRegisteredMethod";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (state, registered, arity, name) = {
            let method = this.borrow();
            (
                method.state.clone(),
                method.registered.clone(),
                method.arity,
                method.name.clone(),
            )
        };
        let mut arguments: Vec<_> = (0..params.len())
            .filter_map(|index| params.arg(index))
            .collect();
        if arity == 2.0 && arguments.len() != 1 {
            // A sequence of nothing is silence, and a score that reads
            // fine but plays nothing is the mistake nobody can see. A
            // registered function with no argument is refused, like any
            // other wrong count.
            if arguments.is_empty() {
                return Err(rquickjs::Exception::throw_message(
                    params.ctx(),
                    &format!(".{name}() expects {} inputs but got 0.", arity - 1.0),
                ));
            }
            arguments = vec![sequence(params.ctx(), &state, arguments)?];
        } else if arity != arguments.len() as f64 + 1.0 {
            return Err(rquickjs::Exception::throw_message(
                params.ctx(),
                &format!(
                    ".{name}() expects {} inputs but got {}.",
                    arity - 1.0,
                    arguments.len()
                ),
            ));
        }
        for argument in &mut arguments {
            *argument = reify_value(params.ctx(), &state, argument.clone())?;
        }
        arguments.push(params.this());
        common::call(&registered, common::undefined(params.ctx()), arguments)
    }
}

#[derive(Trace, JsLifetime)]
struct NativeRawMethod<'js> {
    function: Function<'js>,
    #[qjs(skip_trace)]
    preserve_steps: bool,
}

fn constructor_receiver<'js>(
    ctx: &Ctx<'js>,
    new_target: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let prototype = common::get(ctx, new_target, "prototype")?;
    let object: rquickjs::Object = ctx.globals().get("Object")?;
    let create: Function = object.get("create")?;
    common::call(&create, object.into_value(), [prototype])
}

fn raw_arguments<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    let array = value
        .into_array()
        .ok_or_else(|| throw_type_error(ctx, "raw arguments are not an array"))?;
    // A JS `length` costs the sandbox nothing to inflate, so the reservation
    // is bounded by the heap ceiling instead of trusting it.
    js_array_values(ctx, &array)
}

fn raw_result<'js>(
    ctx: &Ctx<'js>,
    function: &Function<'js>,
    preserve_steps: bool,
    receiver: rquickjs::Value<'js>,
    mut arguments: Vec<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    arguments.push(receiver.clone());
    let result = common::call(function, common::undefined(ctx), arguments)?;
    if preserve_steps {
        pattern_surface::copy_steps(ctx, result, receiver)
    } else {
        Ok(result)
    }
}

#[derive(Trace, JsLifetime)]
struct NativeRawApply;

fn raw_target<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<(Function<'js>, bool)> {
    let target = value
        .as_object()
        .and_then(rquickjs::Class::<NativeRawMethod>::from_object)
        .ok_or_else(|| throw_type_error(ctx, "raw target is not a registered method"))?;
    let target = target.borrow();
    Ok((target.function.clone(), target.preserve_steps))
}

impl<'js> JsClass<'js> for NativeRawApply {
    const NAME: &'static str = "NativeRawApply";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        _this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (function, preserve_steps) = raw_target(
            params.ctx(),
            params
                .arg(0)
                .unwrap_or_else(|| common::undefined(params.ctx())),
        )?;
        let receiver = params
            .arg(1)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        let arguments = raw_arguments(
            params.ctx(),
            params
                .arg(2)
                .unwrap_or_else(|| common::undefined(params.ctx())),
        )?;
        raw_result(params.ctx(), &function, preserve_steps, receiver, arguments)
    }
}

#[derive(Trace, JsLifetime)]
struct NativeRawConstruct;

impl<'js> JsClass<'js> for NativeRawConstruct {
    const NAME: &'static str = "NativeRawConstruct";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        _this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (function, preserve_steps) = raw_target(
            params.ctx(),
            params
                .arg(0)
                .unwrap_or_else(|| common::undefined(params.ctx())),
        )?;
        let arguments = raw_arguments(
            params.ctx(),
            params
                .arg(1)
                .unwrap_or_else(|| common::undefined(params.ctx())),
        )?;
        let new_target = params
            .arg(2)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        let receiver = constructor_receiver(params.ctx(), new_target)?;
        let result = raw_result(
            params.ctx(),
            &function,
            preserve_steps,
            receiver.clone(),
            arguments,
        )?;
        Ok(if result.is_object() { result } else { receiver })
    }
}

fn raw_proxy<'js>(
    ctx: &Ctx<'js>,
    target: Function<'js>,
    handler: rquickjs::Object<'js>,
) -> rquickjs::Result<Function<'js>> {
    let proxy: rquickjs::function::Constructor = ctx.globals().get("Proxy")?;
    let function: Function = proxy.construct((target, handler))?;
    let prototype: rquickjs::Object = function.get("prototype")?;
    prototype.set("constructor", function.clone())?;
    Ok(function)
}

fn raw_handler<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    let apply = rquickjs::Class::instance(ctx.clone(), NativeRawApply)?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
    let construct = rquickjs::Class::instance(ctx.clone(), NativeRawConstruct)?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
    let handler = rquickjs::Object::new(ctx.clone())?;
    handler.set("apply", apply)?;
    handler.set("construct", construct)?;
    Ok(handler)
}

impl<'js> JsClass<'js> for NativeRawMethod<'js> {
    const NAME: &'static str = "NativeRawMethod";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (function, preserve_steps) = {
            let method = this.borrow();
            (method.function.clone(), method.preserve_steps)
        };
        let receiver = params.this();
        let arguments: Vec<_> = (0..params.len())
            .filter_map(|index| params.arg(index))
            .collect();
        raw_result(params.ctx(), &function, preserve_steps, receiver, arguments)
    }
}

#[derive(Trace, JsLifetime)]
struct NativeSetupCurry<'js> {
    function: Function<'js>,
    collected: Vec<rquickjs::Value<'js>>,
    #[qjs(skip_trace)]
    arity: f64,
}

impl<'js> JsClass<'js> for NativeSetupCurry<'js> {
    const NAME: &'static str = "NativeSetupCurry";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (function, arity, mut collected) = {
            let curry = this.borrow();
            (curry.function.clone(), curry.arity, curry.collected.clone())
        };
        collected.extend((0..params.len()).filter_map(|index| params.arg(index)));
        if collected.len() as f64 >= arity {
            return common::call(&function, common::undefined(params.ctx()), collected);
        }
        curry(params.ctx(), function, arity, collected)
    }
}

fn curry<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
    arity: f64,
    collected: Vec<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let curry = rquickjs::Class::instance(
        ctx.clone(),
        NativeSetupCurry {
            function,
            collected,
            arity,
        },
    )?
    .into_value()
    .into_function()
    .expect("a callable class is a function");
    common::configure(&curry, "curried", 0, false)?;
    Ok(curry.into_value())
}

fn defaulted_bool<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
    default: bool,
) -> rquickjs::Result<bool> {
    if value.is_undefined() {
        Ok(default)
    } else {
        common::truthy(ctx, value)
    }
}

#[derive(Clone)]
struct RegistrationOptions<'js> {
    function: Function<'js>,
    patternify: bool,
    preserve_steps: bool,
    join: Option<rquickjs::Value<'js>>,
}

/// Install one name - or a whole name array, recursively - and return its
/// curried callable.
///
/// `remaining` is the element budget of the ENTIRE name tree of one
/// `register` call, seeded from the heap ceiling at the entry point: every
/// declared element of every name array the recursion meets draws on it, so
/// nested sparse name arrays cannot mint a fresh cap per level. See
/// [`charge_js_elements`].
fn register_one<'js>(
    ctx: &Ctx<'js>,
    state: &rquickjs::Class<'js, SetupState<'js>>,
    mode: RegistrationMode,
    name: rquickjs::Value<'js>,
    options: RegistrationOptions<'js>,
    remaining: &Cell<usize>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if common::truthy(ctx, name.clone())? {
        let marker = common::get(ctx, name.clone(), "_Pattern")?;
        if common::truthy(ctx, marker)? {
            return Err(rquickjs::Exception::throw_message(
                ctx,
                "Name argument for register is a pattern, try using single quotes ('name') instead of double quotes (\"name\")",
            ));
        }
    }
    if let Some(names) = name.as_array() {
        // A score-authored name list (see `js_array_len`), and every claimed
        // element pays a full `register_one`: the declared length is read
        // without rquickjs' int assert (past `i32::MAX` it is a float64) and
        // charged to the name tree's SHARED budget before the walk, so a
        // chain of individually modest sparse arrays cannot sum past the
        // heap-ceiling cap. Unlike the sibling materialization sites there is
        // no `Vec` to fill here - what the loop keeps is the JS-side `result`
        // property - so the claim is charged without a reservation.
        let len = js_array_len(names)?;
        charge_js_elements(ctx, len, remaining)?;
        let result = rquickjs::Object::new(ctx.clone())?;
        for index in 0..len {
            let item: rquickjs::Value = names.get(index)?;
            let key = common::property_name(ctx, item.clone())?;
            result.set(
                key.as_str(),
                register_one(ctx, state, mode, item, options.clone(), remaining)?,
            )?;
        }
        return Ok(result.into_value());
    }
    let name = common::property_name(ctx, name)?;
    let publish = mode == RegistrationMode::Public;
    let install_method = mode != RegistrationMode::RawOnly;
    let publish_raw = publish || mode != RegistrationMode::Public;
    if publish && state.borrow().sealed.borrow().contains(&name) {
        return Err(rquickjs::Exception::throw_message(
            ctx,
            &format!(
                "register('{name}') would replace the existing built-in '{name}'. Pick a different name."
            ),
        ));
    }
    if publish {
        remember_displaced(ctx, state, &name)?;
    }
    let length: rquickjs::Value = options.function.get("length")?;
    let arity = Coerced::<f64>::from_js(ctx, length)?.0;
    let registered = rquickjs::Class::instance(
        ctx.clone(),
        NativeRegistered {
            state: state.clone(),
            function: options.function.clone(),
            join: options.join,
            arity,
            patternify: options.patternify,
            preserve_steps: options.preserve_steps,
        },
    )?
    .into_value()
    .into_function()
    .expect("a callable class is a function");
    common::configure(&registered, "pfunc", 0, true)?;

    let proto = state.borrow().proto.clone();
    if install_method {
        let method = rquickjs::Class::instance(
            ctx.clone(),
            NativeRegisteredMethod {
                state: state.clone(),
                registered: registered.clone(),
                arity,
                name: name.clone(),
            },
        )?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
        common::configure(&method, "", 0, true)?;
        proto.set(name.as_str(), method)?;
    }
    if publish_raw && arity > 1.0 {
        let target = rquickjs::Class::instance(
            ctx.clone(),
            NativeRawMethod {
                function: options.function.clone(),
                preserve_steps: options.preserve_steps,
            },
        )?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
        common::configure(&target, "", 0, true)?;
        let handler = state.borrow().raw_handler.clone();
        let raw = raw_proxy(ctx, target, handler)?;
        proto.set(format!("_{name}"), raw)?;
    }
    let curried = curry(ctx, registered, arity, Vec::new())?;
    if publish {
        state.borrow().scope.set(name.as_str(), curried.clone())?;
    }
    Ok(curried)
}

#[derive(Trace, JsLifetime)]
struct NativeRegister<'js> {
    state: rquickjs::Class<'js, SetupState<'js>>,
    #[qjs(skip_trace)]
    mode: RegistrationMode,
}

impl<'js> JsClass<'js> for NativeRegister<'js> {
    const NAME: &'static str = "NativeRegister";
    const KIND: rquickjs::class::ClassKind = rquickjs::class::ClassKind::Callable;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> rquickjs::Result<Option<rquickjs::Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(
        _ctx: &Ctx<'js>,
    ) -> rquickjs::Result<Option<rquickjs::function::Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(
        this: &JsCell<'js, Self>,
        params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let (state, mode) = {
            let register = this.borrow();
            (register.state.clone(), register.mode)
        };
        let name = params
            .arg(0)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        let function = params
            .arg(1)
            .and_then(rquickjs::Value::into_function)
            .ok_or_else(|| throw_type_error(params.ctx(), "func is not a function"))?;
        let patternify = defaulted_bool(
            params.ctx(),
            params
                .arg(2)
                .unwrap_or_else(|| common::undefined(params.ctx())),
            true,
        )?;
        let preserve_steps = defaulted_bool(
            params.ctx(),
            params
                .arg(3)
                .unwrap_or_else(|| common::undefined(params.ctx())),
            false,
        )?;
        let raw_join = params
            .arg(4)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        let join = (!raw_join.is_undefined()).then_some(raw_join);
        // One budget for this call's whole name tree: `register_one`
        // recurses into nested name arrays, and every level's declared
        // length must draw on the SAME heap-ceiling cap - a fresh cap per
        // level would let a depth-D sparse chain run D × cap uninterruptible
        // full registrations. This is the entry point for all three modes
        // (`register`, `registerCanonical`, `registerRawOnly`).
        let remaining = Cell::new(js_element_cap::<rquickjs::Value<'js>>(params.ctx())?);
        register_one(
            params.ctx(),
            &state,
            mode,
            name,
            RegistrationOptions {
                function,
                patternify,
                preserve_steps,
                join,
            },
            &remaining,
        )
    }
}

fn registrar<'js>(
    ctx: &Ctx<'js>,
    state: rquickjs::Class<'js, SetupState<'js>>,
    mode: RegistrationMode,
    name: &str,
    length: usize,
    constructible: bool,
) -> rquickjs::Result<Function<'js>> {
    let registrar = rquickjs::Class::instance(ctx.clone(), NativeRegister { state, mode })?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
    common::configure(&registrar, name, length, constructible)?;
    Ok(registrar)
}

fn finalize_registered_pure<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::function::Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    pattern_surface::with_locations(
        &ctx,
        value.0.unwrap_or_else(|| common::undefined(&ctx)),
        rquickjs::Array::new(ctx.clone())?.into_value(),
    )
}

pub(super) struct Surface<'js> {
    pub(super) register: Function<'js>,
    pub(super) reify: Function<'js>,
    pub(super) set_string_parser: Function<'js>,
    pub(super) register_canonical: Function<'js>,
    pub(super) register_raw_only: Function<'js>,
    pub(super) finalize_registered_pure: Function<'js>,
    pub(super) scope: rquickjs::Object<'js>,
    pub(super) sealed: Rc<RefCell<HashSet<String>>>,
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    pattern: Function<'js>,
    pure: Function<'js>,
    fastcat: Function<'js>,
) -> Result<Surface<'js>, String> {
    let scope = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
    let sealed = Rc::new(RefCell::new(HashSet::new()));
    let raw_handler = raw_handler(ctx).map_err(|error| error.to_string())?;
    let displaced = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
    displaced
        .set_prototype(None)
        .map_err(|error| error.to_string())?;
    // The record is reachable from the host roots, so a score turn can give
    // the surface back without holding on to the setup state itself.
    let state_slot = rquickjs::Array::new(ctx.clone()).map_err(|error| error.to_string())?;
    state_slot
        .set(DISPLACED_PROTO, proto.clone())
        .map_err(|error| error.to_string())?;
    state_slot
        .set(DISPLACED_SCOPE, scope.clone())
        .map_err(|error| error.to_string())?;
    state_slot
        .set(DISPLACED_RECORD, displaced.clone())
        .map_err(|error| error.to_string())?;
    host_stack(ctx)
        .map_err(|error| error.to_string())?
        .as_object()
        .set(DISPLACED_STATE, state_slot)
        .map_err(|error| error.to_string())?;
    let state = rquickjs::Class::instance(
        ctx.clone(),
        SetupState {
            pattern,
            pure,
            fastcat,
            proto: proto.clone(),
            scope: scope.clone(),
            raw_handler,
            string_parser: common::undefined(ctx),
            displaced,
            sealed: sealed.clone(),
        },
    )
    .map_err(|error| error.to_string())?;

    let register = registrar(
        ctx,
        state.clone(),
        RegistrationMode::Public,
        "register",
        6,
        true,
    )
    .map_err(|error| error.to_string())?;
    let register_canonical = registrar(
        ctx,
        state.clone(),
        RegistrationMode::Canonical,
        "registerCanonical",
        5,
        false,
    )
    .map_err(|error| error.to_string())?;
    let register_raw_only = registrar(
        ctx,
        state.clone(),
        RegistrationMode::RawOnly,
        "registerRawOnly",
        5,
        false,
    )
    .map_err(|error| error.to_string())?;

    let reify = rquickjs::Class::instance(
        ctx.clone(),
        NativeReify {
            state: state.clone(),
        },
    )
    .map_err(|error| error.to_string())?
    .into_value()
    .into_function()
    .expect("a callable class is a function");
    common::configure(&reify, "reify", 1, true).map_err(|error| error.to_string())?;

    let set_string_parser = rquickjs::Class::instance(ctx.clone(), NativeSetStringParser { state })
        .map_err(|error| error.to_string())?
        .into_value()
        .into_function()
        .expect("a callable class is a function");
    common::configure(&set_string_parser, "setStringParser", 1, false)
        .map_err(|error| error.to_string())?;

    let finalize_registered_pure =
        Function::new(ctx.clone(), finalize_registered_pure).map_err(|error| error.to_string())?;
    common::configure(
        &finalize_registered_pure,
        "finalizeRegisteredPure",
        1,
        false,
    )
    .map_err(|error| error.to_string())?;

    Ok(Surface {
        register,
        reify,
        set_string_parser,
        register_canonical,
        register_raw_only,
        finalize_registered_pure,
        scope,
        sealed,
    })
}
