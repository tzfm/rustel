//! Globals that a setup script written for the browser calls.

use super::*;
use rquickjs::IntoJs;
use rquickjs::function::Rest;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

/// The most values one `getRandsAtTime` call returns.
const MAX_RANDS: f64 = 4096.0;

pub(crate) const REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "getTime",
        synonyms: &[],
        summary: "the cycle that plays now",
        description: "Returns the cycle position of the newest scheduling window as a number. It is 0 before the first window. A bounce to a file has one window, at its first cycle. A setup script reads it to start a pattern on the next bar.",
        params: &[],
        examples: &["s(\"hh*8\").late(Math.ceil(getTime()))"],
        tags: &["internals"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "getRandsAtTime",
        synonyms: &[],
        summary: "the random numbers that `rand` gives at a time",
        description: "Returns one number from 0 to 1 for n = 1, or an array of n numbers. The same time and seed always give the same numbers, so a helper stays in step with `rand`.",
        params: &[
            ReferenceParam {
                name: "time",
                r#type: "number",
                description: "the cycle position.",
            },
            ReferenceParam {
                name: "n",
                r#type: "number",
                description: "how many numbers, 1 by default and 4096 at most.",
            },
            ReferenceParam {
                name: "seed",
                r#type: "number",
                description: "the seed, 0 by default.",
            },
        ],
        examples: &[
            "s(\"hh*8\").withHap((hap) => hap.withValue((v) => ({ ...v, gain: getRandsAtTime(Number(hap.whole.begin)) })))",
        ],
        tags: &["internals"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "getFrequencyFromValue",
        synonyms: &[],
        summary: "the frequency in Hz of an event value",
        description: "Reads `freq`, or converts `note`, then applies `octave`. A value with neither uses the default note.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "object",
                description: "the event value, such as { note: 'c3' }.",
            },
            ReferenceParam {
                name: "defaultNote",
                r#type: "number",
                description: "the MIDI note of a value with no note, 36 by default.",
            },
        ],
        examples: &["note(\"a4\").s(\"sine\").lpf(getFrequencyFromValue({ note: 'a4' }) * 2)"],
        tags: &["internals"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "addEventListener",
        synonyms: &["removeEventListener"],
        summary: "Rustel has no browser events.",
        description: "Accepted so a setup script with keyboard shortcuts loads. The listener is never called.",
        params: &[],
        examples: &[],
        tags: &["rustel"],
        no_autocomplete: true,
        deprecated: false,
        origin: "rustel",
    },
];

fn argument<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    index: usize,
) -> rquickjs::Value<'js> {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()))
}

fn number_or<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
    default: f64,
) -> rquickjs::Result<f64> {
    if value.is_undefined() {
        return Ok(default);
    }
    Ok(rquickjs::Coerced::<f64>::from_js(ctx, value)?.0)
}

fn truthy<'js>(ctx: &Ctx<'js>, value: &rquickjs::Value<'js>) -> rquickjs::Result<bool> {
    Ok(rquickjs::Coerced::<bool>::from_js(ctx, value.clone())?.0)
}

fn rands_at_time<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let time = number_or(&ctx, argument(&ctx, &args.0, 0), f64::NAN)?;
    let count = number_or(&ctx, argument(&ctx, &args.0, 1), 1.0)?;
    let seed = number_or(&ctx, argument(&ctx, &args.0, 2), 0.0)?;
    if !(0.0..=MAX_RANDS).contains(&count) {
        return Err(rquickjs::Exception::throw_range(
            &ctx,
            &format!("getRandsAtTime gives 0 to {MAX_RANDS} numbers, got {count}"),
        ));
    }
    let rands = rustel_core::rng::rands_at_time(time, count as usize, seed);
    if count == 1.0 {
        return rands[0].into_js(&ctx);
    }
    rands.into_js(&ctx)
}

/// `getFrequencyFromValue(value, defaultNote = 36)`: `freq`, or `note` as a
/// frequency, times `2^octave`.
fn frequency_from_value<'js>(
    ctx: Ctx<'js>,
    args: Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let value = argument(&ctx, &args.0, 0);
    if value.is_null() || value.is_undefined() {
        return Err(rquickjs::Exception::throw_type(
            &ctx,
            "getFrequencyFromValue needs an event value",
        ));
    }
    let field = |key: &str| -> rquickjs::Result<rquickjs::Value<'js>> {
        match value.as_object() {
            Some(object) => object.get(key),
            None => Ok(rquickjs::Value::new_undefined(ctx.clone())),
        }
    };
    let mut note = field("note")?;
    if !truthy(&ctx, &note)? {
        note = number_or(&ctx, argument(&ctx, &args.0, 1), 36.0)?.into_js(&ctx)?;
    }
    let midi = match note.as_string() {
        Some(text) => Some(
            rustel_core::util::note_to_midi(&text.to_string()?, 3)
                .map_err(|message| rquickjs::Exception::throw_message(&ctx, &message))?,
        ),
        None => note.as_number(),
    };
    let freq = field("freq")?;
    let freq = match midi {
        Some(midi) if !truthy(&ctx, &freq)? => rustel_core::util::midi_to_freq(midi),
        _ => number_or(&ctx, freq, f64::NAN)?,
    };
    let octave = number_or(&ctx, field("octave")?, 0.0)?;
    (freq * 2f64.powf(octave)).into_js(&ctx)
}

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let install = |name: &str, length: usize, function: rquickjs::Result<Function<'js>>| {
        let function = function.map_err(|error| error.to_string())?;
        configure_function(&function, name, length, false).map_err(|error| error.to_string())?;
        globals
            .set(name, function)
            .map_err(|error| error.to_string())
    };
    let cycle_time = runtime.cycle_time.clone();
    install(
        "getTime",
        0,
        Function::new(ctx.clone(), move || cycle_time.get()),
    )?;
    install(
        "getRandsAtTime",
        3,
        Function::new(ctx.clone(), rands_at_time),
    )?;
    install(
        "getFrequencyFromValue",
        2,
        Function::new(ctx.clone(), frequency_from_value),
    )?;
    for name in ["addEventListener", "removeEventListener"] {
        install(name, 2, Function::new(ctx.clone(), || {}))?;
    }
    Ok(())
}
