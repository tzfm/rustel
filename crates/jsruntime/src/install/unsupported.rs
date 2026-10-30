use super::*;
use rquickjs::function::Rest;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

/// Reference entries for visual helpers and unsupported score calls. `H` is
/// written inside Hydra code, so its entry lives with the Hydra names.
pub(crate) const REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "clearHydra",
        synonyms: &[],
        summary: "stop Hydra visuals",
        description: "Clears Hydra visuals and releases the renderer. Updating to a score that does not use Hydra also clears the visuals.",
        params: &[],
        examples: &["clearHydra()"],
        tags: &["rustel", "visuals", "hydra"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "initHydra",
        synonyms: &[],
        summary: "open Hydra and draw its visuals behind the code",
        description: "Starts Hydra visuals for the score. Await this call before using Hydra functions such as osc, noise, shape, kaleid, modulate and out. Studio draws the visuals behind the code and clears them when playback stops. The examples tab of the Studio reference column has a HYDRA section with sketches to copy.\n\nOptions: feedStrudel supplies the terminal image as a texture; detectAudio supplies the engine's audio output; pixelated selects nearest-neighbour scaling. Width and height are fallback dimensions for headless or custom hosts; Studio chooses the display size. Strength is accepted for compatibility, but Studio's visuals opacity setting controls the blend.\n\nThis is Rustel's binding for hydra-synth. Builds without visuals support accept the call and log a notice once; the score continues without visuals.",
        params: &[ReferenceParam {
            name: "options",
            r#type: "object",
            description: "{ feedStrudel, detectAudio, pixelated, width, height, strength } - all optional; Studio overrides the fallback size and ignores strength.",
        }],
        examples: &["await initHydra()\nosc(10, 0.1, 0.8).kaleid(5).out(o0)\n$: s(\"bd*4\")"],
        tags: &["rustel", "visuals", "hydra"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "K",
        synonyms: &[],
        summary: "build a KabelSalat audio graph",
        description: "Runs a KabelSalat graph for each event. Pass a graph expression or a callback containing graph code. Use sFreq for the event frequency and sGate for its gate. End each output with .out(). Use audioin() to process the sound entering this stage. K serializes the graph code for worklet; worklet itself accepts source text, not a callback. The graph runs in the native audio engine. Mondo syntax and external MIDI or mouse graph controls are not supported.\n\nMethods installed through register() have an underscore bypass. For example, ._abs() passes the signal through unchanged instead of applying .abs(). The standalone _ helper is an alias for n, the constant node constructor.",
        params: &[ReferenceParam {
            name: "graph",
            r#type: "expression | function",
            description: "KabelSalat graph code. Use S(pattern) for pattern inputs.",
        }],
        examples: &["note(\"c3 e3 g3\").K(sine(sFreq).mul(sGate).out())"],
        tags: &["kabelsalat", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "worklet",
        synonyms: &[],
        summary: "run KabelSalat graph source text",
        description: "Runs KabelSalat source text for each event. This is the source-text form emitted by K. Use K(expression) or K(() => { ... }) to write graph code directly; worklet does not accept a callback. Inside the source text, sFreq and sGate describe the current event, and pat contains the values of any additional input patterns in argument order. S(pattern) extraction belongs to K and is not performed inside a worklet source string. The graph runs in the native audio engine.",
        params: &[
            ReferenceParam {
                name: "source",
                r#type: "string",
                description: "KabelSalat graph statements as a single-quoted string; double quotes denote mini-notation. Call .out() to connect each output.",
            },
            ReferenceParam {
                name: "...patterns",
                r#type: "Pattern | string | number",
                description: "Optional input patterns. Their event values are available as pat[0], pat[1], and so on.",
            },
        ],
        examples: &["note(\"c3 e3 g3\").worklet('sine(sFreq).mul(sGate).out()')"],
        tags: &["kabelsalat", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "getDuration",
        synonyms: &["getDur"],
        summary: "sample duration lookup is not supported",
        description: "Rustel does not provide this sample-duration lookup. Calling getDuration() or getDur() causes a score evaluation error.",
        params: &[],
        examples: &[],
        tags: &["rustel", "audio"],
        no_autocomplete: true,
        deprecated: false,
        origin: "rustel",
    },
];

fn refuse<'js>(
    ctx: Ctx<'js>,
    _args: Rest<rquickjs::Value<'js>>,
    name: &'static str,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    if matches!(name, "getDuration" | "getDur") {
        return Err(rquickjs::Exception::throw_message(
            &ctx,
            &format!("{name}() is not supported."),
        ));
    }
    let message = if name == "K" {
        "K() expects graph code, for example K(sine(220).out())".to_string()
    } else {
        format!("{name}() is available inside K() graph code")
    };
    Err(rquickjs::Exception::throw_message(&ctx, &message))
}

/// The visuals names, in a build with no visuals.
///
/// These do not throw. The rest of this module refuses chains whose sound
/// cannot be produced, because playing without that sound would be wrong. A
/// missing picture does not change the sound, so a score carried over from a
/// build with visuals keeps running and logs once why nothing is drawn.
#[cfg(not(feature = "hydra"))]
fn install_absent_visuals<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    const NOTICE: &str = "[hydra] this build has no visuals window; initHydra() did nothing. Rebuild with `--features hydra` for one.";

    let sink = runtime.logs.clone();
    let open = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, _args: Rest<rquickjs::Value<'js>>| {
            {
                let mut sink = sink.borrow_mut();
                if !sink.iter().any(|line| line == NOTICE) && sink.len() < MAX_BUFFERED_LOGS {
                    sink.push(NOTICE.to_string());
                }
            }
            // `await initHydra()` must settle from the job queue like any other
            // awaited host call, so this is a Promise and not `undefined`.
            let promise: rquickjs::Object = ctx.globals().get("Promise")?;
            let resolve: Function = promise.get("resolve")?;
            resolve.call::<_, rquickjs::Value>((rquickjs::function::This(promise),))
        },
    )
    .map_err(|error| error.to_string())?;
    open.set_name("initHydra")
        .map_err(|error| error.to_string())?;
    globals
        .set("initHydra", open)
        .map_err(|error| error.to_string())?;

    let clear = Function::new(ctx.clone(), |_args: Rest<rquickjs::Value<'js>>| {})
        .map_err(|error| error.to_string())?;
    clear
        .set_name("clearHydra")
        .map_err(|error| error.to_string())?;
    globals
        .set("clearHydra", clear)
        .map_err(|error| error.to_string())?;

    // Upstream's `H` is a thunk Hydra calls each frame. With no window to call
    // it, the honest stand-in is a thunk that answers zero.
    let signal = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, _args: Rest<rquickjs::Value<'js>>| Function::new(ctx, || 0.0_f64),
    )
    .map_err(|error| error.to_string())?;
    signal.set_name("H").map_err(|error| error.to_string())?;
    globals
        .set("H", signal)
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    #[cfg(not(feature = "hydra"))]
    install_absent_visuals(runtime, ctx, globals)?;
    #[cfg(feature = "hydra")]
    let _ = runtime;

    for name in ["K", "audioin", "S", "getDuration", "getDur"] {
        let current: rquickjs::Value = globals.get(name).map_err(|error| error.to_string())?;
        if !current.is_undefined() {
            continue;
        }
        let function = Function::new(ctx.clone(), move |ctx, args| refuse(ctx, args, name))
            .map_err(|error| error.to_string())?;
        function.set_name(name).map_err(|error| error.to_string())?;
        globals
            .set(name, function)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_native_calls_are_refused_instead_of_silently_accepted() {
        let runtime = JsRuntime::new().expect("runtime");
        runtime.install_semantic_bindings().expect("bindings");
        for name in ["getDuration", "getDur"] {
            let error = runtime.ctx.with(|ctx| {
                let function: Function = ctx.globals().get(name).expect("unsupported binding");
                let result: rquickjs::Result<rquickjs::Value> = function.call(("value",));
                crate::surface::effects::describe_js_error(
                    &ctx,
                    result.expect_err("call must be unsupported"),
                )
            });
            assert!(error.contains(&format!("{name}()")), "{error}");
            assert!(error.contains("not supported"), "{error}");
        }
    }
}
