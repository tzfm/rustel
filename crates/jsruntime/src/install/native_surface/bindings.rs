use super::{callbacks::*, composition::*, controls::*, modulation::*, patterns::*, values::*, *};
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

/// The one parameter of every callback form of a join.
pub(crate) const BIND_FUNC: &[ReferenceParam] = &[ReferenceParam {
    name: "func",
    r#type: "Function",
    description: "receives each value, returns a value or a pattern.",
}];

/// Compatibility methods that leave the pattern unchanged.
const OUTPUT_PASSTHROUGHS: ReferenceEntry = ReferenceEntry {
    name: "webaudio",
    synonyms: &[
        "csound",
        "tone",
        "webdirt",
        "speak",
        "wave",
        "soundfont",
        "dough",
        "fscope",
    ],
    summary: "These browser output and scope methods are not supported in Rustel.",
    description: "Each method returns the pattern unchanged. It does not select an output, enable speech, load a soundfont, or open a frequency scope.",
    params: &[],
    examples: &[],
    tags: &["rustel", "external_io"],
    no_autocomplete: true,
    deprecated: false,
    origin: "rustel",
};

/// Reference entries for the methods and setup calls installed below.
pub(crate) const REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "bind",
        synonyms: &[],
        summary: "map each value through a function, and join the patterns it returns",
        description: "Applies fmap, then join. The callback receives each value and may return a pattern. The result is flattened using the intersection of the inner and outer events' full spans. innerBind, outerBind and squeezeBind use their corresponding join methods.",
        params: BIND_FUNC,
        examples: &[],
        tags: &["functional"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "innerBind",
        synonyms: &[],
        summary: "map each value through a function, keeping the returned patterns' timing",
        description: "fmap followed by innerJoin: the function may return a pattern, and the patterns it returns set the timing of the flattened result.",
        params: BIND_FUNC,
        examples: &[],
        tags: &["functional"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "outerBind",
        synonyms: &[],
        summary: "map each value through a function, keeping the receiver's timing",
        description: "fmap followed by outerJoin: the function may return a pattern, and the outer pattern sets the timing of the flattened result.",
        params: BIND_FUNC,
        examples: &[],
        tags: &["functional"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "squeezeBind",
        synonyms: &[],
        summary: "map each value through a function, squeezing each returned pattern into its event",
        description: "Applies fmap, then squeezeJoin: one cycle of each returned pattern fits inside its corresponding outer event.",
        params: BIND_FUNC,
        examples: &[],
        tags: &["functional"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    OUTPUT_PASSTHROUGHS,
    ReferenceEntry {
        name: "registerSynthSounds",
        synonyms: &[],
        summary: "Native synths are available without registration.",
        description: "Accepted for compatibility; this call does not register additional synths.",
        params: &[],
        examples: &[],
        tags: &["rustel", "samples"],
        no_autocomplete: true,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "loadSoundfont",
        synonyms: &[],
        summary: "Loading soundfonts through this call is not supported in Rustel.",
        description: "Returns an empty object without loading a soundfont.",
        params: &[],
        examples: &[],
        tags: &["rustel", "samples"],
        no_autocomplete: true,
        deprecated: false,
        origin: "rustel",
    },
];

fn define_method<'js>(
    proto: &rquickjs::Object<'js>,
    name: &str,
    function: Function<'js>,
) -> Result<(), String> {
    let (function_name, length) = match name {
        "hush" => ("", 0),
        "splitQueries" => ("value", 0),
        "bind" | "outerBind" | "innerBind" | "squeezeBind" | "appLeft" | "appBoth" => ("", 1),
        "piano" | "choose" | "choose2" | "sortHapsByPart" => ("value", 0),
        "soft" | "hard" | "cubic" | "diode" | "asym" | "fold" | "sinefold" | "chebyshev" => {
            ("value", 1)
        }
        "partials" | "phases" => ("", 1),
        "FX" => ("", 0),
        "xfade" => ("", 2),
        "filterHaps" => ("", 1),
        "filterValues" | "withQuerySpan" => ("value", 1),
        "modulate" => ("value", 3),
        "lfo" | "env" | "bmod" => ("value", 2),
        _ => ("value", 0),
    };
    function
        .set_name(function_name)
        .map_err(|error| error.to_string())?;
    function
        .set_length(length)
        .map_err(|error| error.to_string())?;
    function.set_constructor(true);
    let constructor_prototype =
        rquickjs::Object::new(function.ctx().clone()).map_err(|error| error.to_string())?;
    constructor_prototype
        .prop(
            "constructor",
            rquickjs::object::Property::from(function.clone())
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())?;
    function
        .as_inner()
        .prop(
            "prototype",
            rquickjs::object::Property::from(constructor_prototype).writable(),
        )
        .map_err(|error| error.to_string())?;
    proto
        .prop(
            name,
            rquickjs::object::Property::from(function)
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())
}

fn set_function_length(function: &Function<'_>, length: usize) -> Result<(), String> {
    function
        .set_length(length)
        .map_err(|error| error.to_string())
}

pub(super) fn install<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let value_to_midi_function =
        Function::new(ctx.clone(), value_to_midi).map_err(|error| error.to_string())?;
    set_function_length(&value_to_midi_function, 1)?;
    globals
        .set("valueToMidi", value_to_midi_function)
        .map_err(|error| error.to_string())?;
    globals
        .set("useRNG", Function::new(ctx.clone(), use_rng))
        .map_err(|error| error.to_string())?;
    globals
        .set(
            "setDefaultJoin",
            Function::new(ctx.clone(), set_default_join),
        )
        .map_err(|error| error.to_string())?;
    globals
        .set(
            "getControlName",
            Function::new(ctx.clone(), get_control_name),
        )
        .map_err(|error| error.to_string())?;
    globals
        .set("freqToMidi", Function::new(ctx.clone(), freq_to_midi))
        .map_err(|error| error.to_string())?;
    globals
        .set("midiToFreq", Function::new(ctx.clone(), midi_to_freq))
        .map_err(|error| error.to_string())?;
    globals
        .set("getFreq", Function::new(ctx.clone(), get_freq))
        .map_err(|error| error.to_string())?;
    globals
        .set("isNote", Function::new(ctx.clone(), is_note))
        .map_err(|error| error.to_string())?;
    globals
        .set(
            "isNoteWithOctave",
            Function::new(ctx.clone(), is_note_with_octave),
        )
        .map_err(|error| error.to_string())?;
    globals
        .set("midi2note", Function::new(ctx.clone(), midi_to_note))
        .map_err(|error| error.to_string())?;
    globals
        .set("tokenizeNote", Function::new(ctx.clone(), tokenize_note))
        .map_err(|error| error.to_string())?;
    globals
        .set("id", Function::new(ctx.clone(), identity))
        .map_err(|error| error.to_string())?;
    let set_max_polyphony =
        Function::new(ctx.clone(), set_max_polyphony).map_err(|error| error.to_string())?;
    set_function_length(&set_max_polyphony, 1)?;
    globals
        .set("setMaxPolyphony", set_max_polyphony)
        .map_err(|error| error.to_string())?;
    let set_gain_curve =
        Function::new(ctx.clone(), set_gain_curve).map_err(|error| error.to_string())?;
    set_function_length(&set_gain_curve, 1)?;
    globals
        .set("setGainCurve", set_gain_curve)
        .map_err(|error| error.to_string())?;
    globals
        .set("parray", Function::new(ctx.clone(), parray))
        .map_err(|error| error.to_string())?;
    let xfade = Function::new(ctx.clone(), xfade_free).map_err(|error| error.to_string())?;
    set_function_length(&xfade, 3)?;
    globals
        .set("xfade", xfade)
        .map_err(|error| error.to_string())?;
    let morph = Function::new(ctx.clone(), morph).map_err(|error| error.to_string())?;
    set_function_length(&morph, 3)?;
    globals
        .set("morph", morph)
        .map_err(|error| error.to_string())?;
    globals
        .set("seqPLoop", Function::new(ctx.clone(), seq_p_loop))
        .map_err(|error| error.to_string())?;
    let draw_line = Function::new(ctx.clone(), draw_line).map_err(|error| error.to_string())?;
    set_function_length(&draw_line, 1)?;
    globals
        .set("drawLine", draw_line)
        .map_err(|error| error.to_string())?;

    let console: rquickjs::Value = globals.get("console").map_err(|error| error.to_string())?;
    if console.is_null() || console.is_undefined() {
        let console = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
        console
            .set(
                "log",
                Function::new(ctx.clone(), |ctx, args| console_call(ctx, args, None)),
            )
            .map_err(|error| error.to_string())?;
        console
            .set(
                "warn",
                Function::new(ctx.clone(), |ctx, args| {
                    console_call(ctx, args, Some("warn"))
                }),
            )
            .map_err(|error| error.to_string())?;
        console
            .set(
                "error",
                Function::new(ctx.clone(), |ctx, args| {
                    console_call(ctx, args, Some("error"))
                }),
            )
            .map_err(|error| error.to_string())?;
        globals
            .set("console", console)
            .map_err(|error| error.to_string())?;
    }

    let alias_bank: rquickjs::Value = globals
        .get("aliasBank")
        .map_err(|error| error.to_string())?;
    if alias_bank.is_undefined() {
        globals
            .set("aliasBank", Function::new(ctx.clone(), resolved_undefined))
            .map_err(|error| error.to_string())?;
        globals
            .set("loadSoundfont", Function::new(ctx.clone(), empty_object))
            .map_err(|error| error.to_string())?;
        globals
            .set(
                "registerSynthSounds",
                Function::new(
                    ctx.clone(),
                    |_args: rquickjs::function::Rest<rquickjs::Value<'_>>| {},
                ),
            )
            .map_err(|error| error.to_string())?;
    }
    globals
        .set("arrange", Function::new(ctx.clone(), arrange))
        .map_err(|error| error.to_string())?;

    let choose_with_outer = Function::new(ctx.clone(), |ctx, args| {
        choose_with(ctx, args, rustel_core::JoinMode::Outer)
    })
    .map_err(|error| error.to_string())?;
    let choose_with_inner = Function::new(ctx.clone(), |ctx, args| {
        choose_with(ctx, args, rustel_core::JoinMode::Inner)
    })
    .map_err(|error| error.to_string())?;
    set_function_length(&choose_with_outer, 2)?;
    set_function_length(&choose_with_inner, 2)?;
    let choose = Function::new(ctx.clone(), |ctx, args| choose_random(ctx, args, false))
        .map_err(|error| error.to_string())?;
    let choose_in =
        Function::new(ctx.clone(), choose_inner_random).map_err(|error| error.to_string())?;
    let choose_cycles = Function::new(ctx.clone(), |ctx, args| choose_random(ctx, args, true))
        .map_err(|error| error.to_string())?;
    globals
        .set("chooseWith", choose_with_outer)
        .map_err(|error| error.to_string())?;
    globals
        .set("chooseInWith", choose_with_inner)
        .map_err(|error| error.to_string())?;
    globals
        .set("choose", choose.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("chooseIn", choose_in)
        .map_err(|error| error.to_string())?;
    globals
        .set("chooseOut", choose)
        .map_err(|error| error.to_string())?;
    globals
        .set("chooseCycles", choose_cycles.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("randcat", choose_cycles)
        .map_err(|error| error.to_string())?;

    for name in ["cyclesPer", "per", "perCycle", "perx"] {
        let wrapper = new_wrapper(ctx, NativePatternWrapper::plain(span_signal(name)))
            .map_err(|error| error.to_string())?;
        globals
            .set(name, wrapper)
            .map_err(|error| error.to_string())?;
    }

    define_method(
        proto,
        "hush",
        Function::new(ctx.clone(), hush).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "splitQueries",
        Function::new(ctx.clone(), split_queries).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "innerBind",
        Function::new(ctx.clone(), inner_bind).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "outerBind",
        Function::new(ctx.clone(), outer_bind).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "bind",
        Function::new(ctx.clone(), mix_bind).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "squeezeBind",
        Function::new(ctx.clone(), squeeze_bind).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "appLeft",
        Function::new(ctx.clone(), app_left).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "appBoth",
        Function::new(ctx.clone(), app_both).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "modulate",
        Function::new(ctx.clone(), modulate).map_err(|error| error.to_string())?,
    )?;
    for kind in ["lfo", "env", "bmod"] {
        define_method(
            proto,
            kind,
            Function::new(ctx.clone(), move |ctx, this, args| {
                modulator_method(ctx, this, args, kind)
            })
            .map_err(|error| error.to_string())?,
        )?;
        let free = Function::new(ctx.clone(), move |ctx, args| {
            modulator_free(ctx, args, kind)
        })
        .map_err(|error| error.to_string())?;
        set_function_length(&free, 1)?;
        globals.set(kind, free).map_err(|error| error.to_string())?;
    }
    define_method(
        proto,
        "piano",
        Function::new(ctx.clone(), piano).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "choose",
        Function::new(ctx.clone(), |ctx, this, args| {
            choose_method(ctx, this, args, false)
        })
        .map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "choose2",
        Function::new(ctx.clone(), |ctx, this, args| {
            choose_method(ctx, this, args, true)
        })
        .map_err(|error| error.to_string())?,
    )?;

    for algorithm in [
        "soft",
        "hard",
        "cubic",
        "diode",
        "asym",
        "fold",
        "sinefold",
        "chebyshev",
    ] {
        define_method(
            proto,
            algorithm,
            Function::new(ctx.clone(), move |ctx, this, args| {
                distort_method(ctx, this, args, algorithm)
            })
            .map_err(|error| error.to_string())?,
        )?;
        let free = Function::new(ctx.clone(), move |ctx, args| {
            distort_free(ctx, args, algorithm)
        })
        .map_err(|error| error.to_string())?;
        set_function_length(&free, 1)?;
        globals
            .set(algorithm, free)
            .map_err(|error| error.to_string())?;
    }

    for name in ["partials", "phases"] {
        define_method(
            proto,
            name,
            Function::new(ctx.clone(), move |ctx, this, args| {
                list_control_method(ctx, this, args, name)
            })
            .map_err(|error| error.to_string())?,
        )?;
        let free = Function::new(ctx.clone(), move |ctx, args| {
            list_control_free(ctx, args, name)
        })
        .map_err(|error| error.to_string())?;
        set_function_length(&free, 1)?;
        globals.set(name, free).map_err(|error| error.to_string())?;
    }
    define_method(
        proto,
        "FX",
        Function::new(ctx.clone(), fx).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "xfade",
        Function::new(ctx.clone(), xfade_method).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "filterHaps",
        Function::new(ctx.clone(), |ctx, this, args| {
            filter_callback(ctx, this, args, true)
        })
        .map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "filterValues",
        Function::new(ctx.clone(), |ctx, this, args| {
            filter_callback(ctx, this, args, false)
        })
        .map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "withQuerySpan",
        Function::new(ctx.clone(), with_query_span).map_err(|error| error.to_string())?,
    )?;
    define_method(
        proto,
        "sortHapsByPart",
        Function::new(ctx.clone(), sort_haps_by_part).map_err(|error| error.to_string())?,
    )?;

    // Preserve existing functions, including the `osc`, `tune` and `midi`
    // registry combinators.
    let passthroughs = std::iter::once(OUTPUT_PASSTHROUGHS.name)
        .chain(OUTPUT_PASSTHROUGHS.synonyms.iter().copied());
    for name in ["osc", "tune", "midi"].into_iter().chain(passthroughs) {
        let current: rquickjs::Value = proto.get(name).map_err(|error| error.to_string())?;
        if current.is_function() {
            continue;
        }
        define_method(
            proto,
            name,
            Function::new(ctx.clone(), identity_method).map_err(|error| error.to_string())?,
        )?;
    }

    Ok(())
}

/// Global slot for `setGainCurve(fn)`, retained across score evaluations until
/// another call replaces it.
pub const GAIN_CURVE: &str = "__rustel_gain_curve";

/// Store the gain curve used by the value transform installed after lane
/// composition. The transform shapes gain and any explicit velocity.
fn set_gain_curve<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<()> {
    let curve = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    ctx.globals().set(GAIN_CURVE, curve)
}

/// Native polyphony is a runtime setting, so a rejected score cannot change
/// the voice budget of the last good graph and independent sessions stay isolated.
fn set_max_polyphony<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<()> {
    let value = args.0.first().and_then(rquickjs::Value::as_number);
    let maximum = rustel_core::settings::MAX_CONFIGURABLE_POLYPHONY;
    let Some(value) = value.filter(|value| {
        value.is_finite() && value.fract() == 0.0 && *value >= 1.0 && *value <= maximum as f64
    }) else {
        return Err(rquickjs::Exception::throw_range(
            &ctx,
            &format!("setMaxPolyphony requires a whole number from 1 to {maximum}"),
        ));
    };
    rustel_core::settings::set_max_polyphony(value as usize);
    Ok(())
}
