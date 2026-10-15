use super::*;

/// What one argument of a plugin call sets in the plugin object of a hap.
enum Field {
    /// The plugin name. It starts a new object. A second `vst` call adds
    /// its object after the first: the effects of a note are a chain, in
    /// call order. A second `vsti` call replaces the first.
    Name(String),
    /// A key of the object the score wrote. `preset` is the name of a
    /// preset file. Each other key is a parameter of the plugin.
    Key(String),
}

/// Sets one field of a plugin object of a hap. An instrument has one
/// object under `vsti`. The effects are a list under `vst`, and a key goes
/// to the last object of the list:
/// `vsti: { name, preset, params: { key: value } }`, `vst: [{ name }, { name }]`.
fn set_plugin_field(hap: &Value, call: &str, field: &Field, setting: &Value) -> Value {
    let Value::Object(mut controls) = rustel_core::materialize_js_value(hap) else {
        rustel_core::signal_query_error(|| format!("{call} expects object-valued haps"));
        return Value::Undefined;
    };
    let setting = rustel_core::materialize_js_value(setting);
    if matches!(setting, Value::Undefined) {
        return Value::Object(controls);
    }
    let chain = call == "vst";
    let mut effects = match controls.get(call) {
        Some(Value::List(effects)) if chain => effects.clone(),
        _ => Vec::new(),
    };
    let held = if chain {
        effects.pop()
    } else {
        controls.get(call).cloned()
    };
    let mut plugin = match (held, field) {
        (Some(Value::Object(plugin)), Field::Key(_)) => plugin,
        (held, _) => {
            // A new name keeps the effect before it in the chain.
            effects.extend(held.filter(|_| chain));
            rustel_core::OrderedMap::new()
        }
    };
    match field {
        Field::Name(binding) => {
            plugin.insert("name".into(), setting);
            plugin.insert("__rustel_binding".into(), Value::Str(binding.clone()));
        }
        Field::Key(key) if key == "preset" => {
            plugin.insert("preset".into(), setting);
        }
        Field::Key(key) => {
            let mut params = match plugin.get("params") {
                Some(Value::Object(params)) => params.clone(),
                _ => rustel_core::OrderedMap::new(),
            };
            params.insert(key.clone(), setting);
            plugin.insert("params".into(), Value::Object(params));
        }
    }
    let plugin = Value::Object(plugin);
    let value = if chain {
        effects.push(plugin);
        Value::List(effects)
    } else {
        plugin
    };
    controls.insert(call.into(), value);
    Value::Object(controls)
}

/// `.vst(name, { param: value, preset: name })`: the note goes through an
/// effect plugin in its chain. `.vsti(name, { ... })`: an instrument plugin
/// makes the sound of the note. Each field of the object is a
/// pattern, so a slider or a mini-notation string gives one value to each
/// note.
pub(super) fn plugin_method<'js>(
    ctx: Ctx<'js>,
    this: rquickjs::function::This<rquickjs::Class<'js, NativePatternWrapper<'js>>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    call: &'static str,
) -> NativeResult<'js> {
    let (receiver, sidecars) = {
        let wrapper = this.0.borrow();
        (wrapper.pattern.clone(), vec![Sidecar::of(&wrapper)])
    };
    install_plugin(ctx, receiver, sidecars, args, call)
}

/// `vsti(name, { ... })` and `vst(name, { ... })` at the start of a chain:
/// one note for each cycle, with the plugin.
pub(super) fn plugin_free<'js>(
    ctx: Ctx<'js>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    call: &'static str,
) -> NativeResult<'js> {
    let receiver = rustel_core::pure(Value::Object(rustel_core::OrderedMap::new()));
    install_plugin(ctx, receiver, Vec::new(), args, call)
}

fn install_plugin<'js>(
    ctx: Ctx<'js>,
    receiver: Pattern,
    mut sidecars: Vec<Sidecar<'js>>,
    args: rquickjs::function::Rest<rquickjs::Value<'js>>,
    call: &'static str,
) -> NativeResult<'js> {
    let name = args
        .0
        .first()
        .and_then(rquickjs::Value::as_string)
        .and_then(|name| name.to_string().ok())
        .filter(|name| !name.trim().is_empty());
    let Some(name) = name else {
        // Text in double quotes in a variable turns into a pattern before
        // this code runs, so the message tells the two forms that work.
        let forms = if !args.0.is_empty() {
            format!(
                " Write the name in quotes in the call, as .{call}(\"ott\"), \
                 or in single quotes in a variable: 'ott', not \"ott\"."
            )
        } else {
            String::new()
        };
        return Err(rquickjs::Exception::throw_type(
            &ctx,
            &format!("{call} expects the name of a plugin as its first argument.{forms}"),
        ));
    };
    let binding = with_bridge_frame(BridgeFrame::next_plugin_binding)
        .flatten()
        .ok_or_else(|| {
            rquickjs::Exception::throw_type(
                &ctx,
                &format!("{call} must be built in the score or one of its callbacks"),
            )
        })?;
    let name = Value::Str(name);
    let field = Field::Name(binding);
    let mut output =
        receiver.fmap_keeping_live(move |hap| Some(set_plugin_field(hap, call, &field, &name)));
    if let Some(config) = args.0.get(1).and_then(rquickjs::Value::as_object) {
        for entry in config.props::<String, rquickjs::Value>() {
            let (key, value) = entry?;
            let (value, sidecar) = reify_bridged(&ctx, &value)?;
            sidecars.push(sidecar);
            let field = Field::Key(key);
            // The note keeps the live links of its own controls.
            output = output.app_left_with_lookup(
                value,
                move |hap, setting| set_plugin_field(hap, call, &field, setting),
                rustel_core::LookupFlow::Left,
            );
        }
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

    fn bindings(runtime: &JsRuntime, source: &str) -> Vec<(String, Vec<String>)> {
        runtime
            .evaluate_score(source, &TranspileOptions::default())
            .expect("evaluate");
        query_bindings(runtime, 0)
    }

    fn query_bindings(runtime: &JsRuntime, cycle: i64) -> Vec<(String, Vec<String>)> {
        runtime
            .query(
                Slot::Active,
                0,
                Fraction::from(cycle),
                Fraction::from(cycle + 1),
            )
            .expect("query")
            .into_iter()
            .map(|hap| {
                let Value::Object(controls) = hap.value else {
                    panic!("object hap");
                };
                let lane = match controls.get("__rustel_lane") {
                    Some(Value::Str(lane)) => lane.clone(),
                    None => String::new(),
                    _ => panic!("lane tag"),
                };
                let mut plugins: Vec<_> = controls.get("vsti").into_iter().collect();
                if let Some(Value::List(effects)) = controls.get("vst") {
                    plugins.extend(effects);
                }
                let ids = plugins
                    .into_iter()
                    .map(|plugin| {
                        let Value::Object(plugin) = plugin else {
                            panic!("plugin object");
                        };
                        let Some(Value::Str(binding)) = plugin.get("__rustel_binding") else {
                            panic!("binding");
                        };
                        binding.clone()
                    })
                    .collect();
                (lane, ids)
            })
            .collect()
    }

    #[test]
    fn plugin_bindings_distinguish_constructions_and_survive_control_edits() {
        let runtime = runtime();
        let source = r#"
          const fx = pattern => pattern.vst("effect", {drive: .2});
          lead: stack(fx(note("c3")), fx(note("e3"))).vst("tail").orbit(1);
          bass: note("g2").vsti("synth");
          $: s("bd");
        "#;
        // A labeled statement gives its plugin notes the tag `#<number>`. With
        // no lane numbers from the host, the number is the place of the
        // statement.
        let expected = vec![
            ("#0".into(), vec!["p0".into(), "p2".into()]),
            ("#0".into(), vec!["p1".into(), "p2".into()]),
            ("#1".into(), vec!["p3".into()]),
            (String::new(), vec![]),
        ];
        assert_eq!(bindings(&runtime, source), expected);
        assert_eq!(
            bindings(
                &runtime,
                &source.replace(".2", ".7").replace("orbit(1)", "orbit(2)")
            ),
            expected
        );

        let reused = bindings(
            &runtime,
            r#"const fx = note("c3").vst("effect");
               one: stack(fx, fx); two: fx; $: fx; $: fx;"#,
        );
        assert_eq!(
            reused,
            vec![
                ("#0".into(), vec!["p0".into()]),
                ("#0".into(), vec!["p0".into()]),
                ("#1".into(), vec!["p0".into()]),
                ("#2".into(), vec!["p0".into()]),
                ("#3".into(), vec!["p0".into()])
            ]
        );

        // The lane tag of each note of `lanes`, for the lane numbers of a
        // host.
        let lane_tags = |lanes: &str, numbers: &[u64]| -> Vec<String> {
            let options = TranspileOptions {
                lane_numbers: numbers.to_vec(),
                ..TranspileOptions::default()
            };
            let source = format!(r#"const fx = note("c3").vst("effect"); {lanes}"#);
            runtime.evaluate_score(&source, &options).expect("evaluate");
            let notes = query_bindings(&runtime, 0);
            notes.into_iter().map(|(lane, _)| lane).collect()
        };
        // The tag has the lane number of the host. A lane above changes no
        // tag below.
        assert_eq!(lane_tags("lead: fx; $: fx;", &[4, 1000]), ["#4", "#1000"]);
        assert_eq!(
            lane_tags("$: fx; lead: fx; $: fx;", &[5_000_000_000, 4, 1000]),
            ["#5000000000", "#4", "#1000"]
        );
        // A lane with no number keeps its run-time name, and the `$` count
        // goes on. The last lane of a name gives the tag. The solo rule reads
        // the name.
        for (lanes, tags) in [
            ("$: fx; fx.p('$'); fx.p('hand');", vec!["#0", "$1", "hand"]),
            ("lead: fx; fx.p('lead');", vec!["lead"]),
            ("fx.p('lead'); lead: fx;", vec!["#0"]),
            ("lead: fx; S$: fx;", vec!["#1"]),
        ] {
            assert_eq!(lane_tags(lanes, &[]), tags, "{lanes}");
        }
    }

    #[test]
    fn a_plugin_name_in_a_variable_of_double_quotes_names_the_fix() {
        let runtime = runtime();
        // Text in double quotes in a variable turns into a pattern, so the
        // call says the two forms that work.
        let error = runtime
            .evaluate_score(
                r#"const fx = "ott"; s("bd").vst(fx)"#,
                &TranspileOptions::default(),
            )
            .expect_err("a pattern is no plugin name");
        assert!(error.contains(".vst(\"ott\")"), "{error}");
        assert!(error.contains("'ott', not \"ott\""), "{error}");
        // Single quotes keep the text, so the same call works.
        runtime
            .evaluate_score(
                r#"const fx = 'ott'; s("bd").vst(fx)"#,
                &TranspileOptions::default(),
            )
            .expect("single quotes keep the text");
    }

    #[test]
    fn query_callbacks_can_select_prebuilt_plugin_patterns() {
        let runtime = runtime();
        for source in [
            r#"pure(0).bind(() => fx)"#,
            r#"s("bd").sometimesBy(1, () => fx)"#,
            r#"s("bd").when(true, () => fx)"#,
        ] {
            let source = format!("const fx = s('sd').vst('effect'); {source}");
            assert_eq!(
                bindings(&runtime, &source),
                vec![(String::new(), vec!["p0".into()])]
            );
            let later = runtime
                .query(Slot::Active, 0, Fraction::ONE, Fraction::from(2))
                .expect("later query");
            assert_eq!(later.len(), 1, "{source}");
            assert!(runtime.take_logs().is_empty(), "{source}");
        }
        for source in [
            r#"s("bd").when(true, x => x.vst("effect"))"#,
            r#"s("bd")._when(true, x => x.vst("effect"))"#,
        ] {
            let first = bindings(&runtime, source);
            assert_eq!(first.len(), 1, "{source}");
            assert_eq!(first[0].1.len(), 1, "{source}");
            assert_eq!(query_bindings(&runtime, 1), first, "{source}");
        }
    }

    #[test]
    fn query_callbacks_keep_plugin_bindings_across_queries_and_edits() {
        let runtime = runtime();
        for source in [
            r#"pure(0).bind(() => s("bd").vst("effect", {drive: .2}).orbit(1))"#,
            r#"s("bd").sometimesBy(1, x => x.vst("effect", {drive: .2}).orbit(1))"#,
            r#"pure(0).bind(() => note("c3").vsti("synth", {drive: .2}).orbit(1))"#,
            r#"pure(0).bind(() => pure(0).bind(() => s("bd").vst("effect", {drive: .2}).orbit(1)))"#,
        ] {
            let first = bindings(&runtime, source);
            assert_eq!(first.len(), 1, "{source}");
            assert_eq!(first[0].1.len(), 1, "{source}");
            for cycle in [0, 1, 3, 0] {
                assert_eq!(query_bindings(&runtime, cycle), first, "{source}");
            }
            assert_eq!(
                bindings(
                    &runtime,
                    &source.replace(".2", ".7").replace("orbit(1)", "orbit(2)")
                ),
                first,
                "{source}"
            );
            runtime.snapshot_active_as_last_good();
            bindings(&runtime, r#"s("bd").vst("unrelated")"#);
            runtime.restore_last_good_active().expect("restore graph");
            assert_eq!(query_bindings(&runtime, 2), first, "{source}");
            assert!(runtime.take_logs().is_empty(), "{source}");
        }
        let branches = bindings(
            &runtime,
            r#"stack(
            pure(0).bind(() => s("bd").vst("effect")),
            pure(0).bind(() => s("sd").vst("effect")))"#,
        );
        assert_eq!(branches.len(), 2);
        assert_ne!(branches[0].1, branches[1].1);
        assert_eq!(query_bindings(&runtime, 1), branches);

        runtime
            .evaluate_prelude(
                r#"globalThis.prebuiltFx = pure(0).bind(() => s("sd").vst("effect"));"#,
                &TranspileOptions::default(),
                std::time::Duration::from_secs(1),
            )
            .expect("setup callback");
        let first = bindings(&runtime, "prebuiltFx.orbit(1)");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1.len(), 1);
        assert_eq!(bindings(&runtime, "prebuiltFx.orbit(2)"), first);
        assert_eq!(query_bindings(&runtime, 1), first);
    }
}
