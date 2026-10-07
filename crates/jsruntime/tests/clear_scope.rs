use rustel_jsruntime::JsRuntime;

#[test]
fn clear_scope_removes_user_keys_without_deleting_host_bindings() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");

    runtime
        .eval(
            r#"
              const hostNames = Object.getOwnPropertyNames(globalThis);
              const originalHost = new Map(hostNames.map(name => [name, globalThis[name]]));
              globalThis.scoreScratch = 42;
              userDefinedKeys.add('scoreScratch');
              for (const name of hostNames) userDefinedKeys.add(name);
              userDefinedKeys.add({ toString() { return 'silence'; } });
              clearScope();
              clearScope();
              if ('scoreScratch' in globalThis) throw new Error('user global survived');
              for (const name of hostNames) {
                if (!(name in globalThis) || !Object.is(globalThis[name], originalHost.get(name))) {
                  throw new Error(name + ' was removed or replaced');
                }
              }
              if (!(userDefinedKeys instanceof Set)) throw new Error('registry was removed');
              if (userDefinedKeys.size !== 0) throw new Error('registry was not cleared');
            "#,
        )
        .expect("clearScope preserves host bindings and removes user keys");
}
