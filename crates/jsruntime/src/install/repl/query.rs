use super::*;
use rquickjs::{
    IntoJs,
    function::{Rest, This},
    object::Property,
};

fn caught_message<'js>(ctx: &Ctx<'js>, error: rquickjs::Error) -> rquickjs::Result<String> {
    if !matches!(error, rquickjs::Error::Exception) {
        return Err(error);
    }
    let caught = ctx.catch();
    let message = common::get_property(ctx, caught, "message")?;
    Ok(rquickjs::Coerced::<String>::from_js(ctx, message)?.0)
}

fn run_query<'js>(
    ctx: &Ctx<'js>,
    receiver: rquickjs::Value<'js>,
    begin: rquickjs::Value<'js>,
    end: rquickjs::Value<'js>,
    controls: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let span_constructor: rquickjs::Value = ctx.globals().get("TimeSpan")?;
    let span_constructor = span_constructor
        .into_constructor()
        .ok_or_else(|| throw_type_error(ctx, "TimeSpan is not a constructor"))?;
    let span: rquickjs::Value = span_constructor.construct((begin, end))?;
    let state_constructor: rquickjs::Value = ctx.globals().get("State")?;
    let state_constructor = state_constructor
        .into_constructor()
        .ok_or_else(|| throw_type_error(ctx, "State is not a constructor"))?;
    let state: rquickjs::Value = state_constructor.construct((span, controls))?;
    let query = common::get_property(ctx, receiver.clone(), "query")?
        .into_function()
        .ok_or_else(|| throw_type_error(ctx, "receiver.query is not a function"))?;
    common::call(&query, receiver, [state])
}

fn query_arc<'js>(
    ctx: Ctx<'js>,
    this: This<rquickjs::Value<'js>>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let effects = host_effect_boundary(&ctx)?;
    let refusal_on_entry = effects.refusal.borrow().is_some();
    let _policy = EffectPolicyScope::enter_state(&effects, EffectPolicy::NONE);
    let controls = match args.0.get(2) {
        Some(value) if !value.is_undefined() => value.clone(),
        _ => rquickjs::Object::new(ctx.clone())?.into_value(),
    };
    let result = run_query(
        &ctx,
        this.0,
        common::argument(&ctx, &args.0, 0),
        common::argument(&ctx, &args.0, 1),
        controls,
    );
    let result = match result {
        Ok(value) => Ok(value),
        Err(error) => caught_message(&ctx, error).and_then(|message| {
            let logger: rquickjs::Value = ctx.globals().get("logger")?;
            let logger = logger
                .into_function()
                .ok_or_else(|| throw_type_error(&ctx, "logger is not a function"))?;
            common::call(
                &logger,
                common::undefined(&ctx),
                [
                    format!("[query] error: {message}").into_js(&ctx)?,
                    "error".into_js(&ctx)?,
                ],
            )?;
            Ok(rquickjs::Array::new(ctx.clone())?.into_value())
        }),
    };
    if !refusal_on_entry && let Some(message) = effects.refusal.borrow().clone() {
        return Err(rquickjs::Exception::throw_range(&ctx, &message));
    }
    result
}

pub(super) fn install<'js>(ctx: &Ctx<'js>, proto: &rquickjs::Object<'js>) -> Result<(), String> {
    let function = Function::new(ctx.clone(), query_arc).map_err(|error| error.to_string())?;
    configure_function(&function, "", 2, true).map_err(|error| error.to_string())?;
    proto
        .prop(
            "queryArc",
            Property::from(function).writable().configurable(),
        )
        .map_err(|error| describe_js_error(ctx, error))
}
