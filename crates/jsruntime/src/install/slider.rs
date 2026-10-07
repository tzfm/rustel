//! Terminal slider cells.

use super::*;
use rquickjs::{
    IntoJs,
    class::{JsCell, JsClass, Readable},
    function::{Args, Params, Rest},
    object::{Accessor, Property},
};
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use std::sync::atomic::{AtomicU64, Ordering};

// Never reused, including after failed evaluations or between JS runtimes.
static NEXT_SLIDER_BINDING: AtomicU64 = AtomicU64::new(1);

/// Reference entries for live slider controls.
pub(crate) const REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "slider",
        synonyms: &[],
        summary: "edit a numeric value with a live slider",
        description: "Displays a live control for a numeric literal in Studio. Dragging the slider rewrites the value in the score and updates a matching control in the running score without reevaluation. Controls appear before the score is evaluated. Bounds must be numeric literals; calls with expressions keep their normal runtime meaning and are not drawn as controls. Invalid literal bounds, an out-of-range value, or a zero step cause an error.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number",
                description: "the starting value, and the literal the control rewrites.",
            },
            ReferenceParam {
                name: "min",
                r#type: "number",
                description: "lower bound; 0 when left out.",
            },
            ReferenceParam {
                name: "max",
                r#type: "number",
                description: "upper bound; 1 when left out.",
            },
            ReferenceParam {
                name: "step",
                r#type: "number",
                description: "drag resolution; a thousandth of the range when left out.",
            },
        ],
        examples: &[
            "$: s(\"bd*4\").gain(slider(0.5, 0, 1, 0.05))",
            "$: note(\"c2 eb2\").s(\"sawtooth\").lpf(slider(800, 100, 4000))",
        ],
        tags: &["rustel", "control", "live"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "sliderValues",
        synonyms: &[],
        summary: "the live value of every named slider",
        description: "An object containing current slider values, keyed by ID.",
        params: &[],
        examples: &[],
        tags: &["rustel", "control"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "sliderWithID",
        synonyms: &[],
        summary: "read a slider value by ID",
        description: "Returns a pattern that reads the current slider value for the given ID. Calls with the same ID share that value, and each call replaces it with the supplied value. The transpiler uses this function for slider(…) calls, deriving an ID from the value's source position. A handwritten call does not create an editor control. Arguments after the value are ignored.",
        params: &[
            ReferenceParam {
                name: "id",
                r#type: "string",
                description: "the slider ID; calls sharing an ID share one value.",
            },
            ReferenceParam {
                name: "value",
                r#type: "number",
                description: "the value assigned to this ID; later calls replace it.",
            },
        ],
        examples: &["$: s(\"bd*4\").gain(sliderWithID(\"mix\", 0.5))"],
        tags: &["rustel", "control"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
];

#[derive(Trace, JsLifetime)]
struct NativeSliderCell<'js> {
    values: rquickjs::Object<'js>,
    #[qjs(skip_trace)]
    id: String,
}

impl<'js> JsClass<'js> for NativeSliderCell<'js> {
    const NAME: &'static str = "NativeSliderCell";
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
        _params: Params<'a, 'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let state = this.borrow();
        state.values.get(state.id.as_str())
    }
}

fn argument<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    index: usize,
) -> rquickjs::Value<'js> {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()))
}

fn slider_with_id<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let id = rquickjs::Coerced::<String>::from_js(&ctx, argument(&ctx, &args.0, 0))?.0;
    let value = argument(&ctx, &args.0, 1);
    let sets = host_slider_sets(&ctx)?;
    let values: rquickjs::Object = sets.get(1)?;
    let bindings: rquickjs::Object = sets.get(4)?;
    let binding = if has_own_property(&ctx, &bindings, &id)? {
        bindings
            .get::<_, String>(id.as_str())?
            .parse::<u64>()
            .map_err(|_| throw_type_error(&ctx, "invalid native slider binding"))?
    } else {
        let binding = NEXT_SLIDER_BINDING
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| throw_type_error(&ctx, "native slider binding capacity exhausted"))?;
        bindings.set(id.as_str(), binding.to_string())?;
        binding
    };
    if has_own_property(&ctx, &values, &id)? {
        values.set(id.as_str(), value)?;
    } else {
        values.prop(id.as_str(), Property::from(value).writable().enumerable())?;
    }

    let cell = rquickjs::Class::instance(ctx.clone(), NativeSliderCell { values, id })?
        .into_value()
        .into_function()
        .expect("a callable slider cell is a function");
    configure_function(&cell, "", 0, false)?;
    let (accessor, sidecar) = bridge_callable(&ctx, cell)?;
    Ok(derive_wrapper(
        ctx,
        rustel_core::ref_pattern(accessor).with_slider_binding(binding),
        &[sidecar],
    )?
    .into_value())
}

fn slider_values<'js>(ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    host_slider_sets(&ctx)?.get(1)
}

pub(super) fn install<'js>(ctx: &Ctx<'js>) -> Result<(), String> {
    let globals = ctx.globals();
    globals
        .prop(
            "sliderValues",
            Accessor::new_get(slider_values).configurable(),
        )
        .map_err(|error| describe_js_error(ctx, error))?;

    let with_id = Function::new(ctx.clone(), slider_with_id).map_err(|error| error.to_string())?;
    configure_function(&with_id, "", 4, false).map_err(|error| error.to_string())?;
    globals
        .set("sliderWithID", with_id)
        .map_err(|error| error.to_string())?;

    let next = Rc::new(Cell::new(0_u64));
    let slider = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>,
              args: Rest<rquickjs::Value<'js>>|
              -> rquickjs::Result<rquickjs::Value<'js>> {
            let id = next.get();
            next.set(id.saturating_add(1));
            let with_id: Function = ctx.globals().get("sliderWithID")?;
            let mut call = Args::new(ctx.clone(), 4);
            call.push_arg(format!("slider_auto_{id}").into_js(&ctx)?)?;
            call.push_arg(argument(&ctx, &args.0, 0))?;
            call.push_arg(argument(&ctx, &args.0, 1))?;
            call.push_arg(argument(&ctx, &args.0, 2))?;
            call.apply(&with_id)
        },
    )
    .map_err(|error| error.to_string())?;
    configure_function(&slider, "", 4, false).map_err(|error| error.to_string())?;
    globals
        .set("slider", slider)
        .map_err(|error| error.to_string())?;

    Ok(())
}
