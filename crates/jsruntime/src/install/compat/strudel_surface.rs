//! Strudel score-scope compatibility layer: global operator forms, missing
//! raw `_name` method aliases, and one-time logged stand-ins for names
//! present on strudel.cc without a native implementation.

use super::{bindings::install_method_fallback, *};

/// Registered pattern methods with arity > 1 installed outside `registry()`
/// that receive a `_name` method alias pointing at `name`.
const EXTRA_RAW_METHODS: &[&str] = &[
    "plyWith",
    "plywith",
    "plyForEach",
    "plyforeach",
    "slice",
    "splice",
    "loopAt",
    "loopat",
    "scrub",
    "isobite",
    "cyclestruct",
    "cycleStruct",
    "chunkInto",
    "chunkinto",
    "bite",
    "csoundm",
    "drawLine",
    "xfade",
    "worklet",
    "stackCenter",
    "stackCentre",
    "stackLeft",
    "stackRepeat",
    "stackRight",
];

/// Existing Pattern methods whose curried global form is also exposed on
/// strudel.cc (in addition to `ComposeOp::ALL`).
const EXTRA_GLOBAL_METHODS: &[(&str, usize)] = &[
    ("bind", 2),
    ("innerBind", 2),
    ("outerBind", 2),
    ("squeezeBind", 2),
    ("timeline", 2),
];

/// Phone motion sensor globals on strudel.cc. Scores read them as signal
/// patterns (`accX.range(...)`), not as functions.
const MOTION_SIGNALS: &[&str] = &[
    "absOriA",
    "absOriB",
    "absOriG",
    "absOriX",
    "absOriY",
    "absOriZ",
    "absoluteOrientationAlpha",
    "absoluteOrientationBeta",
    "absoluteOrientationGamma",
    "absoluteOrientationX",
    "absoluteOrientationY",
    "absoluteOrientationZ",
    "accX",
    "accY",
    "accZ",
    "accelerationX",
    "accelerationY",
    "accelerationZ",
    "gravX",
    "gravY",
    "gravZ",
    "gravityX",
    "gravityY",
    "gravityZ",
    "oriA",
    "oriB",
    "oriG",
    "oriX",
    "oriY",
    "oriZ",
    "orientationA",
    "orientationAlpha",
    "orientationB",
    "orientationBeta",
    "orientationG",
    "orientationGamma",
    "orientationX",
    "orientationY",
    "orientationZ",
    "rotA",
    "rotB",
    "rotG",
    "rotX",
    "rotY",
    "rotZ",
    "rotationAlpha",
    "rotationBeta",
    "rotationGamma",
    "rotationX",
    "rotationY",
    "rotationZ",
];

fn object_helper<'js>(
    globals: &rquickjs::Object<'js>,
    name: &str,
) -> Result<Function<'js>, String> {
    let object: rquickjs::Object = globals.get("Object").map_err(|error| error.to_string())?;
    object.get(name).map_err(|error| error.to_string())
}

fn has_own<'js>(
    has_own_fn: &Function<'js>,
    target: &rquickjs::Object<'js>,
    name: &str,
) -> Result<bool, String> {
    has_own_fn
        .call::<_, bool>((target.clone(), name))
        .map_err(|error| error.to_string())
}

fn warn_once(
    notified: &RefCell<HashSet<&'static str>>,
    sink: &RefCell<Vec<String>>,
    name: &'static str,
) {
    if notified.borrow_mut().insert(name) {
        let mut logs = sink.borrow_mut();
        if logs.len() < MAX_BUFFERED_LOGS {
            logs.push(format!("[warn] {name} is not supported in rustel"));
        }
    }
}

/// Install curried global operator and method forms plus missing `_name`
/// prototype aliases for real native methods. Runs before `RegistrationSeal`.
pub(in crate::install) fn install_aliases<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let has_own_fn = object_helper(globals, "hasOwn")?;

    // Every composition operator has a curried global form: `pow(2, pat)`,
    // `lt(5, pat)`, `and(b, a)`, etc.
    for op in rustel_core::compose::ComposeOp::ALL {
        install_method_fallback(ctx, proto, globals, op.name(), 2)?;
    }
    for &(name, arity) in EXTRA_GLOBAL_METHODS {
        install_method_fallback(ctx, proto, globals, name, arity)?;
    }

    // Point each missing `_name` method for registered arity > 1 combinators at
    // `name`, leaving existing raw methods and `_steps` untouched.
    for name in registry().names() {
        if name.starts_with('_') || name == "steps" {
            continue;
        }
        let registration = registry().get(name).expect("name from registry");
        if registration.arity <= 1 {
            continue;
        }
        let raw_name = format!("_{name}");
        if has_own(&has_own_fn, proto, &raw_name)? || !has_own(&has_own_fn, proto, name)? {
            continue;
        }
        let method: rquickjs::Value = proto.get(name).map_err(|error| error.to_string())?;
        if method.is_function() {
            proto
                .set(raw_name.as_str(), method)
                .map_err(|error| error.to_string())?;
        }
    }

    // Composition operators are getters on `Pattern.prototype`, so their missing
    // `_op` forms forward through `compose_call` with the caller's `this`.
    for &op in rustel_core::compose::ComposeOp::ALL {
        let raw_name = format!("_{}", op.name());
        if has_own(&has_own_fn, proto, &raw_name)? {
            continue;
        }
        let method = Function::new(
            ctx.clone(),
            hr_thisval_rest(move |ctx, this, args| {
                compose_call(
                    ctx,
                    &this.0,
                    &args.0,
                    op,
                    rustel_core::compose::default_alignment(),
                )
            }),
        )
        .map_err(|error| error.to_string())?;
        configure_function(&method, "", 1, true).map_err(|error| error.to_string())?;
        proto
            .set(raw_name.as_str(), method)
            .map_err(|error| error.to_string())?;
    }

    for &name in EXTRA_RAW_METHODS {
        let raw_name = format!("_{name}");
        if has_own(&has_own_fn, proto, &raw_name)? || !has_own(&has_own_fn, proto, name)? {
            continue;
        }
        let method: rquickjs::Value = proto.get(name).map_err(|error| error.to_string())?;
        if method.is_function() {
            proto
                .set(raw_name.as_str(), method)
                .map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

/// Install no-op stand-ins for every still-missing strudel.cc global and
/// pattern method. Runs after `RegistrationSeal` so user `register()` calls
/// replace a stand-in without a built-in replacement warning, and before
/// `install_clear_scope` so `clearScope()` preserves them.
pub(in crate::install) fn install_standins<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let has_own_fn = object_helper(globals, "hasOwn")?;
    let define_property = object_helper(globals, "defineProperty")?;
    let silence: rquickjs::Value = globals.get("silence").map_err(|error| error.to_string())?;
    let make_method: Function = ctx
        .eval("(warn, index) => function () { warn(index); return this; }")
        .map_err(|error| error.to_string())?;
    let make_global: Function = ctx
        .eval("(warn, index, silence) => function () { warn(index); return silence; }")
        .map_err(|error| error.to_string())?;
    let make_setter: Function = ctx
        .eval(
            "((defineProperty, target) => (name) => function (value) {\
                defineProperty(target, name, {\
                    value, writable: true, enumerable: true, configurable: true\
                });\
            })(Object.defineProperty, globalThis)",
        )
        .map_err(|error| error.to_string())?;

    let names: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let notified: Rc<RefCell<HashSet<&'static str>>> = Rc::default();
    let sink = Rc::clone(runtime.logs());
    let warn_names = Rc::clone(&names);
    let warn_fn = Function::new(ctx.clone(), move |index: usize| {
        if let Some(&name) = warn_names.borrow().get(index) {
            warn_once(&notified, &sink, name);
        }
    })
    .map_err(|error| error.to_string())?;
    let mut standins = runtime.compat_standins.borrow_mut();
    let mut installed_method_standins: HashSet<&'static str> = HashSet::new();

    for name in strudel_compat_methods() {
        if has_own(&has_own_fn, proto, name)? {
            continue;
        }
        let index = {
            let mut list = names.borrow_mut();
            let index = list.len();
            list.push(name);
            index
        };
        let method: Function = make_method
            .call((warn_fn.clone(), index))
            .map_err(|error| error.to_string())?;
        configure_function(&method, name, 0, true).map_err(|error| error.to_string())?;
        proto
            .prop(
                name,
                rquickjs::object::Property::from(method.clone())
                    .writable()
                    .configurable(),
            )
            .map_err(|error| error.to_string())?;
        let raw_name = format!("_{name}");
        if !has_own(&has_own_fn, proto, &raw_name)? {
            proto
                .prop(
                    raw_name.as_str(),
                    rquickjs::object::Property::from(method)
                        .writable()
                        .configurable(),
                )
                .map_err(|error| error.to_string())?;
        }
        installed_method_standins.insert(name);
        if !has_own(&has_own_fn, globals, name)? {
            standins.insert(name);
        }
    }

    for name in strudel_compat_globals() {
        if has_own(&has_own_fn, globals, name)? {
            continue;
        }
        let index = {
            let mut list = names.borrow_mut();
            let index = list.len();
            list.push(name);
            index
        };
        if MOTION_SIGNALS.binary_search(&name).is_ok() {
            install_signal_standin(
                ctx,
                globals,
                &define_property,
                &make_global,
                &make_setter,
                &warn_fn,
                index,
                &silence,
                name,
            )?;
        } else {
            let function: Function = make_global
                .call((warn_fn.clone(), index, silence.clone()))
                .map_err(|error| error.to_string())?;
            configure_function(&function, name, 0, true).map_err(|error| error.to_string())?;
            globals
                .set(name, function)
                .map_err(|error| error.to_string())?;
        }
        let has_native_method =
            has_own(&has_own_fn, proto, name)? && !installed_method_standins.contains(name);
        if !has_native_method {
            standins.insert(name);
        }
    }

    Ok(())
}

/// Define a motion-sensor signal stand-in as a configurable accessor on
/// `globalThis`. Reading it logs once and returns `silence`. Assigning to it
/// replaces the accessor with an ordinary own data property.
#[allow(clippy::too_many_arguments)]
fn install_signal_standin<'js>(
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    define_property: &Function<'js>,
    make_global: &Function<'js>,
    make_setter: &Function<'js>,
    warn_fn: &Function<'js>,
    index: usize,
    silence: &rquickjs::Value<'js>,
    name: &'static str,
) -> Result<(), String> {
    let getter: Function = make_global
        .call((warn_fn.clone(), index, silence.clone()))
        .map_err(|error| error.to_string())?;
    let setter: Function = make_setter
        .call((name,))
        .map_err(|error| error.to_string())?;
    let descriptor = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
    descriptor
        .set("get", getter)
        .map_err(|error| error.to_string())?;
    descriptor
        .set("set", setter)
        .map_err(|error| error.to_string())?;
    descriptor
        .set("enumerable", true)
        .map_err(|error| error.to_string())?;
    descriptor
        .set("configurable", true)
        .map_err(|error| error.to_string())?;
    define_property
        .call::<_, rquickjs::Value>((globals.clone(), name, descriptor))
        .map_err(|error| error.to_string())?;
    Ok(())
}
