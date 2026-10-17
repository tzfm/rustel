use super::*;

/// What one argument of a talkit call sets in the speech object of a hap.
enum Field {
    /// The text. It replaces the whole speech object, so a second text on
    /// the same hap speaks in place of the first.
    Text,
    /// A key of the object the score wrote: `pitch`, `quality`, …
    Key(String),
}

/// Sets one field of the speech object of a hap:
/// `talkit: { text: "hello", pitch: 120 }`.
fn set_speech_field(hap: &Value, field: &Field, setting: &Value) -> Value {
    let Value::Object(mut controls) = rustel_core::materialize_js_value(hap) else {
        rustel_core::signal_query_error(|| "talkit expects object-valued haps".to_string());
        return Value::Undefined;
    };
    let setting = rustel_core::materialize_js_value(setting);
    if matches!(setting, Value::Undefined) {
        return Value::Object(controls);
    }
    let mut speech = match (controls.get("talkit"), field) {
        (Some(Value::Object(speech)), Field::Key(_)) => speech.clone(),
        _ => rustel_core::OrderedMap::new(),
    };
    match field {
        Field::Text => {
            speech.insert("text".into(), setting);
        }
        Field::Key(key) => {
            speech.insert(key.clone(), setting);
        }
    }
    controls.insert("talkit".into(), Value::Object(speech));
    Value::Object(controls)
}

/// `.talkit(text, { quality: "sung" })`: the hap speaks the text through the
/// bundled speech synthesizer instead of playing a sample or a synth. Each
/// field of the object is a pattern, so a mini-notation string gives one
/// value to each event.
pub(super) fn speech_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let (receiver, sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    install_speech(ctx, receiver, sidecars, args)
}

/// `talkit(text, { ... })` at the start of a chain: one event per cycle,
/// speaking the text.
pub(super) fn speech_free<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let (text, mut sidecars) = speech_text(&ctx, &args)?;
    // The text pattern is the pattern: one phrase is one event, and
    // `talkit(["hello there", "hi"])` plays the phrases in turn.
    let mut output = text.fmap_keeping_live(|value| {
        let mut speech = rustel_core::OrderedMap::new();
        speech.insert("text".into(), rustel_core::materialize_js_value(value));
        Some(Value::object([("talkit".into(), Value::Object(speech))]))
    });
    if let Some(config) = args.0.get(1).and_then(rquickjs::Value::as_object) {
        output = speech_options(&ctx, output, &mut sidecars, config)?;
    }
    derive_wrapper(ctx, output, &sidecars)
}

/// The text argument as a pattern. A plain string stays whole - the
/// transpiler leaves a talkit string alone, so its spaces are spoken - and
/// the strings of an array do too: `talkit(["hello there", "hi"])` plays
/// the whole phrases in turn within the cycle. Anything else reifies, so
/// `talkit(mini("one two three"))` gives one word per event.
fn speech_text<'js>(
    ctx: &Ctx<'js>,
    args: &rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> rquickjs::Result<(Pattern, Vec<Sidecar<'js>>)> {
    let literal = |value: &rquickjs::Value<'js>| -> rquickjs::Result<Pattern> {
        let text = value
            .as_string()
            .expect("caller checked is_string")
            .to_string()?;
        Ok(rustel_core::pure(Value::Str(text)))
    };
    let mut sidecars = Vec::new();
    match args.0.first() {
        Some(value) if value.is_string() => Ok((literal(value)?, sidecars)),
        Some(value) if value.as_array().is_some() => {
            let array = value.as_array().expect("checked");
            let mut parts = Vec::new();
            for element in array.iter::<rquickjs::Value>() {
                let element = element?;
                if element.is_string() {
                    parts.push(literal(&element)?);
                } else {
                    let (pattern, sidecar) = reify_bridged(ctx, &element)?;
                    sidecars.push(sidecar);
                    parts.push(pattern);
                }
            }
            Ok((rustel_core::fastcat(parts), sidecars))
        }
        Some(value) if !value.is_undefined() => {
            let (pattern, sidecar) = reify_bridged(ctx, value)?;
            sidecars.push(sidecar);
            Ok((pattern, sidecars))
        }
        _ => Err(rquickjs::Exception::throw_type(
            ctx,
            "talkit expects the text to speak, as in talkit(\"hello there\")",
        )),
    }
}

/// The options object: each key becomes a field of the speech object, as a
/// pattern looked up per event.
fn speech_options<'js>(
    ctx: &Ctx<'js>,
    output: Pattern,
    sidecars: &mut Vec<Sidecar<'js>>,
    config: &rquickjs::Object<'js>,
) -> rquickjs::Result<Pattern> {
    let mut output = output;
    for entry in config.props::<String, rquickjs::Value>() {
        let (key, value) = entry?;
        let (value, sidecar) = reify_bridged(ctx, &value)?;
        sidecars.push(sidecar);
        let field = Field::Key(key);
        output = output.app_left_with_lookup(
            value,
            move |hap, setting| set_speech_field(hap, &field, setting),
            rustel_core::LookupFlow::Left,
        );
    }
    Ok(output)
}

fn install_speech<'js>(
    ctx: Ctx<'js>,
    receiver: Pattern,
    mut sidecars: Vec<Sidecar<'js>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
) -> NativeResult<'js> {
    let (text, nested) = speech_text(&ctx, &args)?;
    sidecars.extend(nested);
    // The receiver keeps its structure; the text lands on each of its
    // events, looked up by time.
    let mut output = receiver.app_left_with_lookup(
        text,
        move |hap, text| set_speech_field(hap, &Field::Text, text),
        rustel_core::LookupFlow::Left,
    );
    if let Some(config) = args.0.get(1).and_then(rquickjs::Value::as_object) {
        output = speech_options(&ctx, output, &mut sidecars, config)?;
    }
    derive_wrapper(ctx, output, &sidecars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Slot;
    use rustel_fraction::Fraction;
    use rustel_transpiler::TranspileOptions;

    fn runtime() -> JsRuntime {
        let runtime = JsRuntime::new().expect("runtime");
        runtime.install_semantic_bindings().expect("bindings");
        runtime
    }

    /// The speech objects of one cycle, in onset order.
    fn spoken(runtime: &JsRuntime, source: &str) -> Vec<Value> {
        runtime
            .evaluate_score(source, &TranspileOptions::default())
            .expect("evaluate");
        runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("query")
            .into_iter()
            .map(|hap| hap.value.clone())
            .collect()
    }

    fn text_of(value: &Value) -> String {
        value
            .get("talkit")
            .and_then(|speech| speech.get("text"))
            .and_then(Value::as_str)
            .expect("text")
            .to_owned()
    }

    #[test]
    fn a_spoken_string_keeps_its_spaces() {
        let runtime = runtime();
        let haps = spoken(&runtime, r#"$: talkit("hello there, nice to meet you")"#);
        assert_eq!(haps.len(), 1);
        assert_eq!(text_of(&haps[0]), "hello there, nice to meet you");
    }

    #[test]
    fn an_array_plays_whole_phrases_in_turn() {
        let runtime = runtime();
        // Double quotes, which mark mini-notation everywhere else.
        let haps = spoken(
            &runtime,
            r#"$: talkit(["hello there", "hi"], { q: "sung monotone" })"#,
        );
        assert_eq!(haps.len(), 2);
        assert_eq!(text_of(&haps[0]), "hello there");
        assert_eq!(text_of(&haps[1]), "hi");
        // The key lands as the score wrote it, so the shortcut `q` stays `q`;
        // the voice reads both spellings.
        let quality = |value: &Value| {
            value
                .get("talkit")
                .and_then(|speech| speech.get("q"))
                .and_then(Value::as_str)
                .expect("q")
                .to_owned()
        };
        assert_eq!(quality(&haps[0]), "sung");
        assert_eq!(quality(&haps[1]), "monotone");
    }

    #[test]
    fn options_land_on_the_speech_object() {
        let runtime = runtime();
        let haps = spoken(
            &runtime,
            r#"$: talkit("hola mundo", { personality: "robotoid", language: "es", pitch: 250 })"#,
        );
        let speech = haps[0]
            .get("talkit")
            .and_then(Value::as_object)
            .expect("speech");
        assert_eq!(
            speech.get("text").and_then(Value::as_str),
            Some("hola mundo")
        );
        assert_eq!(
            speech.get("personality").and_then(Value::as_str),
            Some("robotoid")
        );
        assert_eq!(speech.get("language").and_then(Value::as_str), Some("es"));
        assert_eq!(speech.get("pitch").and_then(Value::as_f64), Some(250.0));
    }

    #[test]
    fn a_method_speaks_on_the_receiver_s_events() {
        let runtime = runtime();
        let haps = spoken(&runtime, r#"$: s("bd hh").talkit('hello')"#);
        assert_eq!(haps.len(), 2);
        for hap in &haps {
            assert_eq!(text_of(hap), "hello");
            assert!(
                hap.get("s")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name == "bd" || name == "hh")
            );
        }
    }

    #[test]
    fn a_missing_text_is_refused_at_construction() {
        let runtime = runtime();
        let error = runtime
            .evaluate_score(r#"$: talkit()"#, &TranspileOptions::default())
            .expect_err("no text");
        assert!(error.contains("the text to speak"), "{error}");
    }
}
