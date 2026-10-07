use rustel_jsruntime::JsRuntime;

const CORE_SEMANTIC_OBJECT_BUDGET: usize = 3_500;

fn semantic_object_budget() -> usize {
    let mut budget = CORE_SEMANTIC_OBJECT_BUDGET;
    #[cfg(feature = "extensions")]
    {
        let registry = rustel_ext::default_registry();
        let registered = registry
            .names()
            .into_iter()
            .filter(|name| {
                registry
                    .get(name)
                    .is_some_and(|entry| entry.declared_in.is_extension())
            })
            .count();
        let direct_patterns = rustel_ext::pattern_callables()
            .map(|callable| {
                callable.names.len()
                    * (usize::from(callable.surface.global) + usize::from(callable.surface.method))
            })
            .sum::<usize>();
        let direct_values = rustel_ext::value_callables()
            .map(|callable| callable.names.len())
            .sum::<usize>();
        let states = rustel_ext::pattern_states().count();

        // A native callable retains a QuickJS function and a few descriptor /
        // prototype objects. Pattern state additionally owns one wrapper.
        // Scale the allowance with the declared surface so adding one native
        // function cannot look like an installation leak, while an unrelated
        // hundreds-of-objects regression still fails.
        budget += 4 * (registered + direct_patterns + direct_values);
        budget += 8 * states;
    }
    budget
}

#[test]
fn semantic_surface_keeps_its_quickjs_object_budget() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime.run_gc();

    let objects = runtime.js_object_count();
    // A new binding is a handful more objects; a leak is hundreds.
    assert!(
        objects <= semantic_object_budget(),
        "semantic binding installation retained {objects} QuickJS objects (budget {})",
        semantic_object_budget()
    );
}
