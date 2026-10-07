use super::*;
use rquickjs::{
    class::{JsCell, JsClass, Readable},
    function::Params,
};

#[derive(Clone, Copy)]
enum TempoUnit {
    Cps,
    Cpm,
}

#[derive(Trace, JsLifetime)]
struct NativeTempoSetter<'js> {
    silence: rquickjs::Value<'js>,
    #[qjs(skip_trace)]
    unit: TempoUnit,
    #[qjs(skip_trace)]
    transaction: Rc<RefCell<Option<ScoreEffects>>>,
    #[qjs(skip_trace)]
    policy: Rc<Cell<EffectPolicy>>,
    #[qjs(skip_trace)]
    refusal: Rc<RefCell<Option<String>>>,
}

impl<'js> NativeTempoSetter<'js> {
    fn unpure(
        ctx: &Ctx<'js>,
        value: rquickjs::Value<'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        if value.is_null() {
            return Err(throw_type_error(
                ctx,
                "Cannot read properties of null (reading '_Pattern')",
            ));
        }
        if value.is_undefined() {
            return Err(throw_type_error(
                ctx,
                "Cannot read properties of undefined (reading '_Pattern')",
            ));
        }
        let marker = common::get_property(ctx, value.clone(), "_Pattern")?;
        if rquickjs::Coerced::<bool>::from_js(ctx, marker)?.0 {
            common::get_property(ctx, value, "__pure")
        } else {
            Ok(value)
        }
    }
}

impl<'js> JsClass<'js> for NativeTempoSetter<'js> {
    const NAME: &'static str = "NativeTempoSetter";
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
        let value = params
            .arg(0)
            .unwrap_or_else(|| common::undefined(params.ctx()));
        let value = Self::unpure(params.ctx(), value)?;
        let state = this.borrow();
        if !state.policy.get().allows(EffectPolicy::TEMPO) {
            return Err(throw_effect_policy(
                params.ctx(),
                &state.refusal,
                TEMPO_SCOPE_POLICY,
            ));
        }
        let cps = match state.unit {
            TempoUnit::Cps if value.is_undefined() => 0.5,
            TempoUnit::Cps => value.as_number().ok_or_else(|| {
                throw_effect_policy(params.ctx(), &state.refusal, TEMPO_VALUE_POLICY)
            })?,
            TempoUnit::Cpm => rquickjs::Coerced::<f64>::from_js(params.ctx(), value)?.0 / 60.0,
        };
        if !cps.is_finite() || cps <= 0.0 {
            return Err(throw_effect_policy(
                params.ctx(),
                &state.refusal,
                TEMPO_VALUE_POLICY,
            ));
        }
        stage_effect(
            params.ctx(),
            &state.policy,
            EffectPolicy::TEMPO,
            &state.transaction,
            &state.refusal,
            TEMPO_SCOPE_POLICY,
            |_ctx, effects| {
                effects.cps = Some(cps);
                Ok(())
            },
        )?;
        Ok(state.silence.clone())
    }
}

fn define_data_property(
    ctx: &Ctx<'_>,
    target: &rquickjs::Object<'_>,
    name: &str,
    value: &rquickjs::Value<'_>,
    enumerable: bool,
    configurable: bool,
) -> bool {
    let raw = ctx.as_raw().as_ptr();
    let atom = unsafe { rquickjs::qjs::JS_NewAtomLen(raw, name.as_ptr().cast(), name.len() as _) };
    if atom == rquickjs::qjs::JS_ATOM_NULL {
        return false;
    }
    let mut flags = rquickjs::qjs::JS_PROP_WRITABLE | rquickjs::qjs::JS_PROP_THROW;
    if enumerable {
        flags |= rquickjs::qjs::JS_PROP_ENUMERABLE;
    }
    if configurable {
        flags |= rquickjs::qjs::JS_PROP_CONFIGURABLE;
    }
    let duplicated = unsafe { rquickjs::qjs::JS_DupValue(raw, value.as_raw()) };
    let status = unsafe {
        rquickjs::qjs::JS_DefinePropertyValue(raw, target.as_raw(), atom, duplicated, flags as i32)
    };
    unsafe { rquickjs::qjs::JS_FreeAtom(raw, atom) };
    if status < 0 {
        let _ = ctx.catch();
        false
    } else {
        true
    }
}

fn repair<'js>(
    ctx: Ctx<'js>,
    target: rquickjs::Object<'js>,
    name: rquickjs::Coerced<String>,
    value: rquickjs::Value<'js>,
) -> bool {
    let raw = ctx.as_raw().as_ptr();
    let atom =
        unsafe { rquickjs::qjs::JS_NewAtomLen(raw, name.0.as_ptr().cast(), name.0.len() as _) };
    if atom == rquickjs::qjs::JS_ATOM_NULL {
        return false;
    }
    let mut descriptor = std::mem::MaybeUninit::<rquickjs::qjs::JSPropertyDescriptor>::uninit();
    let status = unsafe {
        rquickjs::qjs::JS_GetOwnProperty(raw, descriptor.as_mut_ptr(), target.as_raw(), atom)
    };
    unsafe { rquickjs::qjs::JS_FreeAtom(raw, atom) };
    if status < 0 {
        let _ = ctx.catch();
        return false;
    }
    if status == 0 {
        return define_data_property(&ctx, &target, &name.0, &value, true, true);
    }

    let descriptor = unsafe { descriptor.assume_init() };
    let configurable = descriptor.flags & rquickjs::qjs::JS_PROP_CONFIGURABLE as i32 != 0;
    let enumerable = descriptor.flags & rquickjs::qjs::JS_PROP_ENUMERABLE as i32 != 0;
    let writable = descriptor.flags & rquickjs::qjs::JS_PROP_WRITABLE as i32 != 0;
    let accessor = !unsafe { rquickjs::qjs::JS_IsUndefined(descriptor.getter) }
        || !unsafe { rquickjs::qjs::JS_IsUndefined(descriptor.setter) };
    let same = !accessor
        && unsafe { rquickjs::qjs::JS_IsStrictEqual(raw, descriptor.value, value.as_raw()) };
    unsafe {
        rquickjs::qjs::JS_FreeValue(raw, descriptor.value);
        rquickjs::qjs::JS_FreeValue(raw, descriptor.getter);
        rquickjs::qjs::JS_FreeValue(raw, descriptor.setter);
    }
    if accessor && !configurable {
        return false;
    }
    if !accessor && !writable && !configurable {
        return same;
    }
    if !accessor && writable && same {
        return true;
    }
    define_data_property(&ctx, &target, &name.0, &value, enumerable, configurable)
}

fn setter<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    silence: rquickjs::Value<'js>,
    unit: TempoUnit,
    name: &str,
) -> Result<Function<'js>, String> {
    let function = rquickjs::Class::instance(
        ctx.clone(),
        NativeTempoSetter {
            silence,
            unit,
            transaction: runtime.effect_transaction.clone(),
            policy: runtime.effect_policy.clone(),
            refusal: runtime.effect_policy_refusal.clone(),
        },
    )
    .map_err(|error| error.to_string())?
    .into_value()
    .into_function()
    .expect("a tempo setter is callable");
    configure_function(&function, name, 1, false).map_err(|error| error.to_string())?;
    Ok(function)
}

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let silence: rquickjs::Value = globals.get("silence").map_err(|error| error.to_string())?;
    let cps = setter(runtime, ctx, silence.clone(), TempoUnit::Cps, "setCps")?;
    let cpm = setter(runtime, ctx, silence, TempoUnit::Cpm, "setCpm")?;
    let scope: rquickjs::Object = globals
        .get("rustelScope")
        .map_err(|error| error.to_string())?;
    let canonical = host_repl_tempo_surface(ctx).map_err(|error| error.to_string())?;
    canonical
        .set(0, scope.clone())
        .map_err(|error| error.to_string())?;
    for (index, (name, function)) in [
        ("setCps", cps.clone()),
        ("setcps", cps.clone()),
        ("setCpm", cpm.clone()),
        ("setcpm", cpm),
    ]
    .into_iter()
    .enumerate()
    {
        canonical
            .set(index + 1, function.clone())
            .map_err(|error| error.to_string())?;
        globals
            .set(name, function.clone())
            .map_err(|error| error.to_string())?;
        scope
            .set(name, function)
            .map_err(|error| error.to_string())?;
    }
    // In REPL scores the free `cps(...)` form is a tempo lane. A scalar
    // value can cross the same atomic score-effect boundary as setCps; the
    // returned silence still supports label syntax and harmless chains such
    // as `tempochanges: cps(1).gain(0)` without emitting an audio event.
    globals
        .set("cps", cps.clone())
        .map_err(|error| error.to_string())?;
    scope.set("cps", cps).map_err(|error| error.to_string())?;
    let repair = Function::new(ctx.clone(), repair).map_err(|error| error.to_string())?;
    configure_function(&repair, "repair", 3, false).map_err(|error| error.to_string())?;
    canonical
        .set(5, repair)
        .map_err(|error| error.to_string())?;
    Ok(())
}
