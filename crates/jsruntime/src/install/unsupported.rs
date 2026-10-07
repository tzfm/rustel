use super::*;
use rquickjs::function::Rest;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

/// Reference entries for visual helpers and unsupported score calls.
pub(crate) const REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "H",
        synonyms: &[],
        summary: "use a pattern as a Hydra parameter",
        description: "Uses a pattern to supply a Hydra parameter at the current transport position. The pattern must be queryable without JavaScript: mini-notation, arithmetic and signals are supported; JavaScript callbacks are not.",
        params: &[ReferenceParam {
            name: "pattern",
            r#type: "string | Pattern",
            description: "the pattern to read, in mini-notation or already built.",
        }],
        examples: &["await initHydra()\nshape(H(\"3 4 5 [6 7]*2\")).out(o0)"],
        tags: &["rustel", "visuals", "hydra"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
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
        synonyms: &["worklet"],
        summary: "unsupported Kabelsalat DSP call",
        description: "Rustel does not implement Strudel's Kabelsalat custom-DSP language. Calling K() or worklet() causes a score evaluation error; during live playback, the last successful score keeps playing. Use Rustel's built-in synths for synthesis.",
        params: &[],
        examples: &[],
        tags: &["rustel", "audio"],
        no_autocomplete: true,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "S",
        synonyms: &[],
        summary: "unsupported Kabelsalat DSP call",
        description: "An unsupported Kabelsalat call. Calling S() causes a score evaluation error; during live playback, the last successful score keeps playing.",
        params: &[],
        examples: &[],
        tags: &["rustel", "audio"],
        no_autocomplete: true,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "audioin",
        synonyms: &[],
        summary: "unsupported; use Rustel's in input",
        description: "An unsupported Kabelsalat call. Calling audioin() causes a score evaluation error; during live playback, the last successful score keeps playing. Use s(\"in\") for Rustel audio input.",
        params: &[],
        examples: &[],
        tags: &["rustel", "external_io"],
        no_autocomplete: true,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "loadOrc",
        synonyms: &[],
        summary: "Csound orchestra loading is not supported",
        description: "Rustel does not support loading Csound orchestras. Calling loadOrc() causes a score evaluation error; during live playback, the last successful score keeps playing.",
        params: &[],
        examples: &[],
        tags: &["rustel", "audio"],
        no_autocomplete: true,
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
    if matches!(name, "loadOrc" | "getDuration" | "getDur") {
        return Err(rquickjs::Exception::throw_message(
            &ctx,
            &format!("{name}() is not supported."),
        ));
    }
    let public_name = if name == "worklet" { "K" } else { name };
    Err(rquickjs::Exception::throw_message(
        &ctx,
        &format!(
            "{public_name}() needs kabelsalat, strudel.cc's custom-DSP language, which this engine does not implement. The score will not run until that chain is removed. Live, the last good score keeps playing."
        ),
    ))
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

    for name in [
        "K",
        "worklet",
        "audioin",
        "S",
        "loadOrc",
        "getDuration",
        "getDur",
    ] {
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
        for name in ["loadOrc", "getDuration", "getDur"] {
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
