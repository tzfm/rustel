use super::{bindings::*, *};
use rquickjs::{
    FromJs,
    class::{JsCell, JsClass, Readable},
    function::{Args, Params, Rest, This},
};
use rustel_core::{
    ops::PatOps,
    register::{CombinatorFn, DeclaredIn, JoinKind, Registration},
    value::FunctionRef,
};
use std::sync::Arc;

fn number(args: &[Value], index: usize) -> f64 {
    args.get(index)
        .and_then(|value| rustel_core::util::parse_numeral(value).ok())
        .unwrap_or(f64::NAN)
}

fn fraction(args: &[Value], index: usize) -> Fraction {
    args.get(index)
        .and_then(rustel_core::register::value_to_fraction)
        .unwrap_or(Fraction::ZERO)
}

fn function(args: &[Value], index: usize) -> Option<&FunctionRef> {
    args.get(index).and_then(Value::as_function)
}

/// The compatibility shims' documentation, carried beside their
/// registrations: Documentation text from the Strudel project
/// (AGPL-3.0-or-later), https://strudel.cc.
const PLY_WITH: rustel_core::reference::ReferenceEntry = rustel_core::reference::ReferenceEntry {
    name: "plyWith",
    synonyms: &["plywith"],
    summary: "The plyWith function repeats each event the given number of times, applying the given function to each event.",
    description: "The plyWith function repeats each event the given number of times, applying the given function to each event.",
    params: &[
        rustel_core::reference::ReferenceParam {
            name: "factor",
            r#type: "number",
            description: "how many times to repeat",
        },
        rustel_core::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply, given the pattern",
        },
    ],
    examples: &["\"<0 [2 4]>\"\n.plyWith(4, (p) => p.add(2))\n.scale(\"C:minor\").note()"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const PLY_FOR_EACH: rustel_core::reference::ReferenceEntry =
    rustel_core::reference::ReferenceEntry {
        name: "plyForEach",
        synonyms: &["plyforeach"],
        summary: "The plyForEach function repeats each event the given number of times, applying the given function to each event.",
        description: "The plyForEach function repeats each event the given number of times, applying the given function to each event.\nThis version of ply uses the iteration index as an argument to the function, similar to echoWith.",
        params: &[
            rustel_core::reference::ReferenceParam {
                name: "factor",
                r#type: "number",
                description: "how many times to repeat",
            },
            rustel_core::reference::ReferenceParam {
                name: "func",
                r#type: "function",
                description: "function to apply, given the pattern and the iteration index",
            },
        ],
        examples: &[
            "\"<0 [2 4]>\"\n.plyForEach(4, (p,n) => p.add(n*2))\n.scale(\"C:minor\").note()",
        ],
        tags: &["temporal"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    };

const LOOP_AT: rustel_core::reference::ReferenceEntry = rustel_core::reference::ReferenceEntry {
    name: "loopAt",
    synonyms: &["loopat"],
    summary: "Makes the sample fit the given number of cycles by changing the speed.",
    description: "Makes the sample fit the given number of cycles by changing the speed.",
    params: &[],
    examples: &[
        "samples({ rhodes: 'https://cdn.freesound.org/previews/132/132051_316502-lq.mp3' })\ns(\"rhodes\").loopAt(2)",
    ],
    tags: &["samples", "pitch"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const FIT: rustel_core::reference::ReferenceEntry = rustel_core::reference::ReferenceEntry {
    name: "fit",
    synonyms: &[],
    summary: "Makes the sample fit its event duration.",
    description: "Makes the sample fit its event duration. Good for rhythmical loops like drum breaks.\nSimilar to `loopAt`.",
    params: &[],
    examples: &[
        "samples({ rhodes: 'https://cdn.freesound.org/previews/132/132051_316502-lq.mp3' })\ns(\"rhodes/2\").fit()",
    ],
    tags: &["samples", "pitch"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const CHUNK_INTO: rustel_core::reference::ReferenceEntry = rustel_core::reference::ReferenceEntry {
    name: "chunkInto",
    synonyms: &["chunkinto"],
    summary: "Like `chunk`, but the function is applied to a looped subcycle of the source pattern.",
    description: "Like `chunk`, but the function is applied to a looped subcycle of the source pattern.",
    params: &[],
    examples: &["sound(\"bd sd ht lt bd - cp lt\").chunkInto(4, hurry(2))\n  .bank(\"tr909\")"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

fn registration(
    names: &[&str],
    reference: rustel_core::reference::ReferenceEntry,
    arity: usize,
    takes_function: bool,
    func: CombinatorFn,
) -> Registration {
    Registration {
        names: names.iter().map(|name| Arc::from(*name)).collect(),
        reference,
        declared_in: DeclaredIn::PatternModule,
        takes_function,
        arity,
        patternify: true,
        preserve_steps: false,
        join: JoinKind::Inner,
        func,
    }
}

fn passthrough<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    _args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let (pattern, sidecar) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), Sidecar::of(&wrapper))
    };
    derive_wrapper(ctx, pattern, &[sidecar])
}

#[derive(Trace, JsLifetime)]
struct NativeLogMapper<'js> {
    formatter: Option<rquickjs::Value<'js>>,
    #[qjs(skip_trace)]
    values_only: bool,
}

fn call_method<'js>(
    ctx: &Ctx<'js>,
    receiver: rquickjs::Value<'js>,
    name: &str,
    values: impl IntoIterator<Item = rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let object = receiver
        .as_object()
        .ok_or_else(|| rquickjs::Exception::throw_type(ctx, "receiver is not an object"))?;
    let function: Function = object.get(name)?;
    let values = values.into_iter().collect::<Vec<_>>();
    let mut args = Args::new(ctx.clone(), values.len());
    args.this(receiver)?;
    args.push_args(values)?;
    args.apply(&function)
}

fn default_log_value<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<String> {
    if value.is_null() || !value.is_object() || value.is_function() {
        return Ok(rquickjs::Coerced::<String>::from_js(ctx, value)?.0);
    }
    let object = value.as_object().expect("object value");
    let mut parts = Vec::new();
    for property in object.props::<String, rquickjs::Value>() {
        let (key, value) = property?;
        let value = rquickjs::Coerced::<String>::from_js(ctx, value)?.0;
        parts.push(format!("{key}:{value}"));
    }
    Ok(parts.join(" "))
}

impl<'js> JsClass<'js> for NativeLogMapper<'js> {
    const NAME: &'static str = "NativeLogMapper";
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
        let (formatter, values_only) = {
            let state = this.borrow();
            (state.formatter.clone(), state.values_only)
        };
        let hap = params
            .arg(0)
            .unwrap_or_else(|| rquickjs::Value::new_undefined(params.ctx().clone()));
        let hap_object = hap
            .as_object()
            .ok_or_else(|| rquickjs::Exception::throw_type(params.ctx(), "hap is not an object"))?;
        let value: rquickjs::Value = hap_object.get("value")?;
        let line = match formatter {
            Some(formatter) => {
                let formatter = formatter.into_function().ok_or_else(|| {
                    rquickjs::Exception::throw_type(params.ctx(), "func is not a function")
                })?;
                let input = if values_only { value } else { hap.clone() };
                let output: rquickjs::Value = formatter.call((input,))?;
                rquickjs::Coerced::<String>::from_js(params.ctx(), output)?.0
            }
            None if values_only => {
                format!("[hap] {}", default_log_value(params.ctx(), value)?)
            }
            None => {
                let shown = call_method(
                    params.ctx(),
                    hap.clone(),
                    "showWhole",
                    [rquickjs::Value::new_bool(params.ctx().clone(), true)],
                )?;
                format!(
                    "[hap] {}",
                    rquickjs::Coerced::<String>::from_js(params.ctx(), shown)?.0
                )
            }
        };

        let context: rquickjs::Object = hap_object.get("context")?;
        let next = rquickjs::Object::new(params.ctx().clone())?;
        for property in context.props::<String, rquickjs::Value>() {
            let (key, value) = property?;
            next.set(key, value)?;
        }
        next.set("logLine", line)?;
        call_method(params.ctx(), hap, "setContext", [next.into_value()])
    }
}

fn log_pattern<'js>(
    ctx: Ctx<'js>,
    This(pattern): This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
    values_only: bool,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let formatter = args
        .0
        .first()
        .filter(|value| !value.is_undefined())
        .cloned();
    let mapper = rquickjs::Class::instance(
        ctx.clone(),
        NativeLogMapper {
            formatter,
            values_only,
        },
    )?
    .into_value()
    .into_function()
    .expect("a callable class is a function");
    mapper.set_name("")?;
    mapper.set_length(1)?;
    mapper.set_constructor(false);
    call_method(&ctx, pattern, "withHap", [mapper.into_value()])
}

fn collect<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
) -> NativeResult<'js> {
    let (pattern, sidecar) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.collect(), Sidecar::of(&wrapper))
    };
    derive_wrapper(ctx, pattern, &[sidecar])
}

fn unjoin<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    flatten: bool,
) -> NativeResult<'js> {
    let (receiver, mut sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    let pieces_value = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let (pieces, sidecar) = reify_bridged(&ctx, &pieces_value)?;
    sidecars.push(sidecar);
    let transform = match args.0.get(1) {
        None => None,
        Some(value) if value.is_undefined() => None,
        Some(value) => {
            let Some(callable) = value.as_function() else {
                let pattern = rustel_core::query_error_pattern("func is not a function");
                return derive_wrapper(ctx, pattern, &sidecars);
            };
            let (id, sidecar) = bridge_callable(&ctx, callable.clone())?;
            sidecars.push(sidecar);
            Some(FunctionRef::js(id))
        }
    };
    let pattern = rustel_core::unjoin(receiver, pieces, transform.as_ref());
    derive_wrapper(
        ctx,
        if flatten {
            pattern.inner_join()
        } else {
            pattern
        },
        &sidecars,
    )
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    define_method(
        proto,
        "onTrigger",
        Function::new(ctx.clone(), passthrough).map_err(|error| error.to_string())?,
        "",
        1,
    )?;
    for (name, values_only) in [("log", false), ("logValues", true)] {
        let function = Function::new(ctx.clone(), move |ctx, this, args| {
            log_pattern(ctx, this, args, values_only)
        });
        define_method(
            proto,
            name,
            function.map_err(|error| error.to_string())?,
            "",
            0,
        )?;
    }

    let ply_with = registration(
        &["plyWith", "plywith"],
        PLY_WITH,
        3,
        true,
        rustel_core::native_combinator!(|args, pat| rustel_core::combinators::ply_with(
            &pat,
            fraction(args, 0),
            function(args, 1)
        )),
    );
    for name in ["plyWith", "plywith"] {
        install_registration(
            ctx,
            proto,
            globals,
            name,
            ply_with.clone(),
            UpstreamExport::NameListObject,
            false,
        )?;
    }
    let ply_for_each = registration(
        &["plyForEach", "plyforeach"],
        PLY_FOR_EACH,
        3,
        true,
        rustel_core::native_combinator!(|args, pat| rustel_core::combinators::ply_for_each(
            &pat,
            fraction(args, 0),
            function(args, 1)
        )),
    );
    for name in ["plyForEach", "plyforeach"] {
        install_registration(
            ctx,
            proto,
            globals,
            name,
            ply_for_each.clone(),
            UpstreamExport::NameListObject,
            true,
        )?;
    }

    install_pattern_arguments(
        ctx,
        proto,
        globals,
        "slice",
        3,
        Arc::new(|patterns| {
            rustel_core::combinators::slice(&patterns[2], &patterns[0], &patterns[1])
        }),
        UpstreamExport::Callable,
    )?;
    install_pattern_arguments(
        ctx,
        proto,
        globals,
        "splice",
        3,
        Arc::new(|patterns| {
            rustel_core::combinators::slice(&patterns[2], &patterns[0], &patterns[1]).splice()
        }),
        UpstreamExport::Callable,
    )?;

    let loop_at = registration(
        &["loopAt", "loopat"],
        LOOP_AT,
        2,
        false,
        rustel_core::native_combinator!(|args, pat| pat.loop_at(fraction(args, 0))),
    );
    for name in ["loopAt", "loopat"] {
        install_registration(
            ctx,
            proto,
            globals,
            name,
            loop_at.clone(),
            UpstreamExport::Callable,
            false,
        )?;
    }
    let fit = registration(
        &["fit"],
        FIT,
        1,
        false,
        rustel_core::native_combinator!(|_args, pat| pat.fit()),
    );
    install_registration(
        ctx,
        proto,
        globals,
        "fit",
        fit,
        UpstreamExport::Callable,
        false,
    )?;

    install_pattern_arguments(
        ctx,
        proto,
        globals,
        "scrub",
        2,
        Arc::new(|patterns| rustel_core::combinators::scrub(&patterns[1], &patterns[0])),
        UpstreamExport::Callable,
    )?;

    define_method(
        proto,
        "onTriggerTime",
        Function::new(ctx.clone(), passthrough).map_err(|error| error.to_string())?,
        "",
        1,
    )?;
    define_method(
        proto,
        "unjoin",
        Function::new(ctx.clone(), |ctx, this, args| {
            unjoin(ctx, this, args, false)
        })
        .map_err(|error| error.to_string())?,
        "",
        1,
    )?;
    define_method(
        proto,
        "into",
        Function::new(ctx.clone(), |ctx, this, args| unjoin(ctx, this, args, true))
            .map_err(|error| error.to_string())?,
        "",
        2,
    )?;

    let chunk_into = registration(
        &["chunkInto", "chunkinto"],
        CHUNK_INTO,
        3,
        true,
        CombinatorFn::Dynamic(Arc::new(|args, pat| {
            rustel_core::chunk_into(pat, number(args, 0), function(args, 1))
        })),
    );
    for name in ["chunkInto", "chunkinto"] {
        install_registration(
            ctx,
            proto,
            globals,
            name,
            chunk_into.clone(),
            UpstreamExport::Callable,
            false,
        )?;
    }

    define_method(
        proto,
        "collect",
        Function::new(ctx.clone(), collect).map_err(|error| error.to_string())?,
        "value",
        0,
    )?;

    for (name, arity) in [
        ("mask", 2),
        ("struct", 2),
        ("superimpose", 2),
        ("bite", 3),
        ("set", 2),
        ("keep", 2),
        ("keepif", 2),
        ("withValue", 2),
        ("add", 2),
        ("sub", 2),
        ("mul", 2),
        ("div", 2),
    ] {
        install_method_fallback(ctx, proto, globals, name, arity)?;
    }

    let scope: rquickjs::Object = globals
        .get("rustelScope")
        .map_err(|error| error.to_string())?;
    for name in [
        "slice",
        "splice",
        "loopAt",
        "loopat",
        "fit",
        "scrub",
        "plyWith",
        "plywith",
        "plyForEach",
        "plyforeach",
        "chunkInto",
        "chunkinto",
        "timeline",
    ] {
        scope.remove(name).map_err(|error| error.to_string())?;
    }
    Ok(())
}
