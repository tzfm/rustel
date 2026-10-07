use super::*;
use rquickjs::{FromJs, IntoJs};

pub(super) fn coerced_number<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<f64> {
    Ok(rquickjs::Coerced::<f64>::from_js(ctx, value)?.0)
}

pub(super) fn coerced_string<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<String> {
    Ok(rquickjs::Coerced::<String>::from_js(ctx, value)?.0)
}

pub(super) fn freq_to_midi<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<f64> {
    Ok(rustel_core::util::freq_to_midi(coerced_number(
        &ctx, value,
    )?))
}

pub(super) fn midi_to_freq<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<f64> {
    Ok(rustel_core::util::midi_to_freq(coerced_number(
        &ctx, value,
    )?))
}

pub(super) fn get_freq<'js>(ctx: Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<f64> {
    let midi = if value.is_number() {
        coerced_number(&ctx, value)?
    } else if let Some(note) = value.as_string() {
        rustel_core::util::note_to_midi(&note.to_string()?, 3)
            .map_err(|message| rquickjs::Exception::throw_message(&ctx, &message))?
    } else {
        return Err(rquickjs::Exception::throw_message(&ctx, "not a note"));
    };
    Ok(rustel_core::util::midi_to_freq(midi))
}

pub(super) fn is_note<'js>(ctx: Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<bool> {
    Ok(rustel_core::util::is_note(&coerced_string(&ctx, value)?))
}

pub(super) fn is_note_with_octave<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<bool> {
    Ok(rustel_core::util::is_note_with_octave(&coerced_string(
        &ctx, value,
    )?))
}

pub(super) fn midi_to_note<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let midi = coerced_number(&ctx, value)?;
    let remainder = midi % 12.0;
    if !midi.is_finite() || midi.fract() != 0.0 || remainder < 0.0 {
        return f64::NAN.into_js(&ctx);
    }
    let names = [
        "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
    ];
    format!(
        "{}{}",
        names[remainder as usize],
        (midi / 12.0).floor() - 1.0
    )
    .into_js(&ctx)
}

pub(super) fn tokenize_note<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Array<'js>> {
    let result = rquickjs::Array::new(ctx.clone())?;
    let Some(note) = value.as_string() else {
        return Ok(result);
    };
    let Some(token) = rustel_core::util::tokenize_note(&note.to_string()?) else {
        return Ok(result);
    };
    result.set(0, token.pitch_class.to_string())?;
    result.set(1, token.accidentals)?;
    match token.octave {
        Some(octave) => result.set(2, octave)?,
        None => result.set(2, rquickjs::Value::new_undefined(ctx))?,
    }
    Ok(result)
}

pub(super) fn value_to_midi<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<f64> {
    let value = args
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
    let fallback = match args.0.get(1) {
        Some(value) => coerced_number(&ctx, value.clone())?,
        None => 36.0,
    };
    let (value, _) = materialize_js_value(&ctx, &value)?;
    rustel_core::util::value_to_midi(&value, Some(fallback))
        .map_err(|message| rquickjs::Exception::throw_message(&ctx, &message))
}

pub(super) fn use_rng<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<()> {
    let mode = match args.0.first() {
        None => "legacy".to_owned(),
        Some(value) if value.is_undefined() => "legacy".to_owned(),
        Some(value) => coerced_string(&ctx, value.clone())?,
    };
    let mode = match mode.as_str() {
        "legacy" => rustel_core::rng::RngMode::Legacy,
        "precise" => rustel_core::rng::RngMode::Precise,
        _ => {
            return Err(rquickjs::Exception::throw_message(
                &ctx,
                &format!("useRNG expects 'legacy' or 'precise', got '{mode}'"),
            ));
        }
    };
    rustel_core::rng::use_rng(mode);
    Ok(())
}

pub(super) fn set_default_join<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<()> {
    let value = if value.is_null() || value.is_undefined() {
        String::new()
    } else {
        coerced_string(&ctx, value)?.to_lowercase()
    };
    let value = if value == "squeezein" {
        "squeeze"
    } else {
        value.as_str()
    };
    if let Some(alignment) = rustel_core::compose::Alignment::from_name(value) {
        rustel_core::compose::set_default_alignment(alignment);
    }
    Ok(())
}

pub(super) fn get_control_name<'js>(
    ctx: Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let key = coerced_string(&ctx, value.clone())?;
    match control_registry().canonical_name(&key) {
        Some(name) if name != key => name.into_js(&ctx),
        _ => Ok(value),
    }
}
