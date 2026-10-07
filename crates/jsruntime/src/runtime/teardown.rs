use super::*;

/// Empty every private root so nothing survives into `JS_FreeRuntime`.
///
/// Separate from [`Drop`] only because the roots borrow the context's `'js`,
/// which a closure inside `with_ctx` cannot name.
pub(super) fn sever_host_roots<'js>(ctx: &Ctx<'js>, roots: &HostRoots<'js>) {
    // A configured parser may close over a callback-bearing Pattern; sever it
    // before dropping the private root and the final GC. Score code cannot
    // mutate or freeze this Array.
    if let Ok(set_parser) = roots
        .stack
        .as_object()
        .get::<_, Function>(LEXICAL_SET_STRING_PARSER)
    {
        let _ = set_parser.call::<_, rquickjs::Value>((rquickjs::Value::new_null(ctx.clone()),));
    }
    // One engine operation clears every numeric query root. There is no Rust
    // loop over a user-influenced sparse length.
    for root in [
        &roots.active,
        &roots.held,
        &roots.stack,
        &roots.score_candidate,
        &roots.repl_tempo_surface,
        &roots.slider_sets,
        &roots.voicing_sets,
    ] {
        let _ = root.as_object().set("length", 0);
    }
}

impl Drop for JsRuntime {
    fn drop(&mut self) {
        // A runtime may itself be thread-local and outlive the settings stack
        // during thread teardown. Root severing does not execute score
        // setters, so it must remain independent of that stack.
        // Release the roots and collect before QuickJS tears down.
        //
        // `JS_FreeRuntime` asserts `list_empty(&rt->gc_obj_list)`, so any
        // cross-heap cycle still outstanding aborts the process. Those cycles
        // are ordinary garbage - a live-coding session ends with the active
        // graph still installed - so the runtime must clear its own roots and
        // run the collector rather than requiring every caller to remember.
        //
        // This does NOT weaken the leak check: a genuinely uncollectable cycle
        // (one held by an external root) survives the GC and still aborts.
        let last_good = self.last_good_active.borrow_mut().take();
        with_ctx(&self.ctx, |ctx| {
            if let Some(saved) = last_good {
                let _ = saved.restore(&ctx);
            }
            // Clearing the roots must not depend on TAKING them. A root left
            // populated survives into `JS_FreeRuntime`, which is the assert
            // this whole function exists to avoid, so `remove_userdata` is the
            // normal path and borrowing is the fallback. While these roots
            // were public globals the clear ran unconditionally; keep that.
            match ctx.remove_userdata::<HostRoots>() {
                Ok(Some(roots)) => sever_host_roots(&ctx, &roots),
                _ => {
                    if let Some(roots) = ctx.userdata::<HostRoots>() {
                        sever_host_roots(&ctx, &roots);
                    }
                }
            }
        });
        for _ in 0..3 {
            self.rt.run_gc();
        }
    }
}
