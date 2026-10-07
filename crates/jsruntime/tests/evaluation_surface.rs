use rustel_jsruntime::JsRuntime;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime.install_voicings_prebake().expect("voicing surface");
    runtime
}

#[test]
fn edo_and_voicing_setup_are_native_and_keep_their_public_behavior() {
    let runtime = runtime();
    runtime
        .eval(
            r#"
              const scale = edo('5edo');
              globalThis.edoResult = JSON.stringify([
                packageName, edo.length, scale.length, scale[0], scale[2]
              ]);

              const aliases = [{ maj: ['0 4 7'] }, { maj: ['0 3 7'] }];
              voicingAlias('maj', 'M', aliases);
              globalThis.aliasResult = JSON.stringify([
                aliases[0].M, aliases[1].M
              ]);

              const addResult = addVoicings('nativeAdd', { '7': ['0 4 7'] });
              const rangeResult = setVoicingRange('nativeAdd', ['C3', 'C5']);
              const registerResult = registerVoicings(
                'nativeRegister', { '7': ['0 3 7'] }, { range: ['D3', 'D5'] }
              );
              globalThis.voicingResult = JSON.stringify([
                addResult === undefined,
                rangeResult === undefined,
                registerResult === undefined,
                voicingRegistry.nativeAdd.range,
                voicingRegistry.nativeRegister.range
              ]);
            "#,
        )
        .expect("native evaluation surface");

    assert_eq!(
        runtime.get_string("edoResult").as_deref(),
        Some("[\"@strudel/edo\",1,5,1,1.3195079107728942]")
    );
    assert_eq!(
        runtime.get_string("aliasResult").as_deref(),
        Some("[[\"0 4 7\"],[\"0 3 7\"]]")
    );
    assert_eq!(
        runtime.get_string("voicingResult").as_deref(),
        Some("[true,true,true,[\"C3\",\"C5\"],[\"D3\",\"D5\"]]")
    );

    let invalid = runtime
        .eval("edo('0edo')")
        .expect_err("invalid edo accepted");
    assert!(invalid.contains("not an edo scale"), "{invalid}");
    let oversized = runtime
        .eval("edo('65537edo')")
        .expect_err("oversized edo accepted");
    assert!(oversized.contains("past the 65536"), "{oversized}");
}
