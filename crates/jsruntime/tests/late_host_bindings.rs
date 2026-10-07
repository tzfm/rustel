use rustel_jsruntime::JsRuntime;

fn runtime_with_user_global() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
        .eval(
            r#"
              globalThis.userBeforeLate = 1;
              userDefinedKeys.add('userBeforeLate');
            "#,
        )
        .expect("user global before late installation");
    runtime
}

fn assert_cleanup_preserves_host_bindings(runtime: &JsRuntime) {
    runtime
        .eval(
            r#"
              const hostNames = Object.getOwnPropertyNames(globalThis)
                .filter(name => name !== 'userBeforeLate');
              const originalHost = new Map(hostNames.map(name => [name, globalThis[name]]));
              globalThis.userAfterLate = 2;
              userDefinedKeys.add('userAfterLate');
              for (const name of hostNames) userDefinedKeys.add(name);
              clearScope();
              clearScope();
              if ('userBeforeLate' in globalThis || 'userAfterLate' in globalThis) {
                throw new Error('unrelated user global survived');
              }
              for (const name of hostNames) {
                if (!(name in globalThis) || !Object.is(globalThis[name], originalHost.get(name))) {
                  throw new Error(name + ' was removed or replaced');
                }
              }
              if (userDefinedKeys.size !== 0) throw new Error('registry was not cleared');
            "#,
        )
        .expect("cleanup preserves late host bindings and removes user globals");
}

#[test]
fn late_voicing_bindings_survive_cleanup_without_protecting_user_globals() {
    let runtime = runtime_with_user_global();
    runtime
        .eval(
            r#"
              globalThis.edo = 'user value';
              userDefinedKeys.add('edo');
            "#,
        )
        .expect("user global replaced by a host binding");
    runtime
        .install_voicings_prebake()
        .expect("voicing bindings");

    assert_cleanup_preserves_host_bindings(&runtime);
    runtime
        .eval(
            r#"
              if (edo('12edo').length !== 12) throw new Error('EDO binding is unavailable');
              registerVoicings('cleanupTest', { '7': ['0 4 7'] });
              if (!voicingRegistry.cleanupTest) throw new Error('voicing binding is unavailable');
            "#,
        )
        .expect("late host bindings remain callable");
}

#[test]
fn optional_late_host_bindings_survive_cleanup() {
    let runtime = runtime_with_user_global();
    runtime.install_query_binding().expect("query binding");
    runtime.install_gc_binding().expect("GC bindings");

    assert_cleanup_preserves_host_bindings(&runtime);
}

#[cfg(feature = "hydra")]
fn evaluate_score(runtime: &JsRuntime, source: &str) {
    runtime
        .evaluate_score_with_effects_cancellable(
            source,
            &rustel_transpiler::TranspileOptions::default(),
            std::time::Duration::from_secs(5),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap_or_else(|error| panic!("could not evaluate `{source}`: {error}"));
}

#[cfg(feature = "hydra")]
#[test]
fn hydra_bindings_survive_cleanup_until_hydra_gives_them_back() {
    let runtime = runtime_with_user_global();
    evaluate_score(&runtime, "await initHydra(); s('bd')");
    assert_cleanup_preserves_host_bindings(&runtime);
    runtime
        .eval(
            r#"
              if (typeof voronoi !== 'function' || typeof render !== 'function') {
                throw new Error('hydra bindings are unavailable');
              }
            "#,
        )
        .expect("hydra bindings remain callable");

    // The next score evaluation gives the Hydra names back to the score.
    evaluate_score(&runtime, "s('bd')");
    runtime
        .eval(
            r#"
              if ('voronoi' in globalThis) throw new Error('the surface kept voronoi');
              globalThis.voronoi = 'user value';
              userDefinedKeys.add('voronoi');
              clearScope();
              if ('voronoi' in globalThis) throw new Error('a returned hydra name stayed protected');
              if (typeof osc !== 'function') throw new Error('the strudel osc was removed');
            "#,
        )
        .expect("returned hydra names are the score's to clear");
}
