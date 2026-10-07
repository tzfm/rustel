//! `gamepad(index)`: the pad as an object of patterns, the way strudel.cc
//! hands it over - `gp.a` is a pattern that reads the button, `gp.x1`
//! the stick, `gp.tglA` the toggle, `gp.btnSequence('dra')` the combo.
//!
//! Every reading is a discrete pattern with a whole, like `midin()`'s
//! controls and unlike a signal: the scheduler drops anything without an
//! onset, so a signal would make `note(gp.x1.range(40, 52))` silent.
//! Read at query time, so `.mask(gp.a)` follows the thumb.

use super::*;

const MAX_PADS: usize = rustel_core::gamepad::MAX_PADS;

/// A pattern that reads the pad when it is queried.
///
/// Volatile: the same query answers with whatever the pad says now, so no
/// result cache may keep it.
fn reading<'js>(
    ctx: Ctx<'js>,
    read: impl Fn() -> f64 + Send + Sync + 'static,
) -> NativeResult<'js> {
    let pattern = rustel_core::pure(Value::F64(1.0))
        .fmap(move |_| Value::F64(read()))
        .mark_volatile();
    derive_wrapper(ctx, pattern, &[])
}

/// `gamepad(i)`: which pad. Nothing given is the first; a name for a pad
/// is not a thing here.
fn pad_index<'js>(ctx: &Ctx<'js>, value: Option<&rquickjs::Value<'js>>) -> rquickjs::Result<usize> {
    let Some(value) = value.filter(|value| !value.is_null() && !value.is_undefined()) else {
        return Ok(0);
    };
    let number: Function = ctx.globals().get("Number")?;
    let index: f64 = number.call((value.clone(),))?;
    if !index.is_finite() || index < 0.0 {
        return Ok(0);
    }
    let index = index as usize;
    if index >= MAX_PADS {
        return Err(rquickjs::Exception::throw_range(
            ctx,
            &format!(
                "gamepad({index}): at most {MAX_PADS} pads, gamepad(0) to gamepad({})",
                MAX_PADS - 1
            ),
        ));
    }
    Ok(index)
}

/// The buttons a sequence names: `'dra'` one letter a button, or
/// `['d', 'r', 'a']` one name a button.
fn sequence_buttons<'js>(
    ctx: &Ctx<'js>,
    value: Option<&rquickjs::Value<'js>>,
) -> rquickjs::Result<Vec<u8>> {
    let names: Vec<String> = match value.filter(|value| !value.is_undefined()) {
        Some(value) if value.is_string() => {
            let text: String = value.get()?;
            text.chars().map(|letter| letter.to_string()).collect()
        }
        Some(value) if value.is_array() => {
            let array = value.as_array().expect("an array");
            let mut names = reserve_js_array(ctx, js_array_len(array)?)?;
            for item in array.iter::<rquickjs::Value<'js>>() {
                let item = item?;
                let stringify: Function = ctx.globals().get("String")?;
                names.push(stringify.call((item,))?);
            }
            names
        }
        _ => {
            return Err(rquickjs::Exception::throw_type(
                ctx,
                "btnSequence() takes the buttons as letters, 'dra', or names, ['down', 'right', 'a']",
            ));
        }
    };
    if names.is_empty() {
        return Err(rquickjs::Exception::throw_range(
            ctx,
            "btnSequence() needs at least one button",
        ));
    }
    names
        .iter()
        .map(|name| {
            rustel_core::gamepad::button_index(name).ok_or_else(|| {
                rquickjs::Exception::throw_range(
                    ctx,
                    &format!(
                        "btnSequence(): no button is called '{name}' - a b x y lb rb lt rt back start l3 r3 up down left right, or u d l r"
                    ),
                )
            })
        })
        .collect()
}

fn hr_pad<F>(f: F) -> F
where
    F: for<'js> Fn(
        Ctx<'js>,
        rquickjs::function::Opt<rquickjs::Value<'js>>,
    ) -> rquickjs::Result<rquickjs::Object<'js>>,
{
    f
}

fn hr_sequence<F>(f: F) -> F
where
    F: for<'js> Fn(Ctx<'js>, rquickjs::function::Opt<rquickjs::Value<'js>>) -> NativeResult<'js>,
{
    f
}

/// `Up` from `up`, for `tglUp`.
fn capitalised(name: &str) -> String {
    let mut letters = name.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + letters.as_str(),
        None => String::new(),
    }
}

/// The pad object: a reading under every name the page lists, in both
/// cases, and the toggles under `tgl`.
fn pad_object<'js>(ctx: &Ctx<'js>, index: usize) -> rquickjs::Result<rquickjs::Object<'js>> {
    let pad = rustel_core::gamepad::pad(index).expect("an index under MAX_PADS");
    let object = rquickjs::Object::new(ctx.clone())?;
    for (names, button) in rustel_core::gamepad::BUTTON_NAMES {
        let button = usize::from(*button);
        for name in *names {
            let upper = name.to_uppercase();
            object.set(*name, reading(ctx.clone(), move || pad.button(button))?)?;
            object.set(
                upper.as_str(),
                reading(ctx.clone(), move || pad.button(button))?,
            )?;
            object.set(
                format!("tgl{}", capitalised(name)).as_str(),
                reading(ctx.clone(), move || pad.toggle(button))?,
            )?;
            object.set(
                format!("tgl{upper}").as_str(),
                reading(ctx.clone(), move || pad.toggle(button))?,
            )?;
        }
    }
    // Sticks: 0 to 1 under the plain name, -1 to 1 under `_2`.
    for (axis, name) in ["x1", "y1", "x2", "y2"].into_iter().enumerate() {
        object.set(
            name,
            reading(ctx.clone(), move || (pad.axis(axis) + 1.0) / 2.0)?,
        )?;
        object.set(
            format!("{name}_2").as_str(),
            reading(ctx.clone(), move || pad.axis(axis))?,
        )?;
    }
    let sequence = Function::new(
        ctx.clone(),
        hr_sequence(move |ctx, buttons| {
            let buttons = sequence_buttons(&ctx, buttons.0.as_ref())?;
            reading(ctx, move || pad.sequence(&buttons))
        }),
    )?;
    sequence.set_length(1)?;
    for name in ["btnSequence", "btnSeq", "btnseq", "checkSequence"] {
        object.set(name, sequence.clone())?;
    }
    Ok(object)
}

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let transaction = runtime.effect_transaction.clone();
    let policy = runtime.effect_policy.clone();
    let gamepad = Function::new(
        ctx.clone(),
        hr_pad(move |ctx, index| {
            let index = pad_index(&ctx, index.0.as_ref())?;
            // A score's request waits until the score is accepted, so a
            // refused one never starts the device poller. Setup, raw code
            // and pattern functions start it when they run.
            let staged = policy.get().allows(EffectPolicy::GAMEPAD)
                && transaction
                    .borrow_mut()
                    .as_mut()
                    .map(|effects| effects.gamepad = true)
                    .is_some();
            if !staged {
                rustel_core::gamepad::request();
            }
            pad_object(&ctx, index)
        }),
    )
    .map_err(|error| error.to_string())?;
    gamepad.set_length(1).map_err(|error| error.to_string())?;
    globals
        .set("gamepad", gamepad)
        .map_err(|error| error.to_string())
}
