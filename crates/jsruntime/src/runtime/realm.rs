//! The script engine of a runtime: the QuickJS heap, its context and the
//! private roots of the host.
//!
//! A runtime starts its realm at the first use that needs one. A host whose
//! scores the native tier evaluates never starts it, and holds no script
//! heap.

use super::*;

/// The script engine of one [`JsRuntime`].
pub(crate) struct Realm {
    pub(crate) rt: Runtime,
    pub(crate) ctx: Context,
}

impl Realm {
    /// Start the engine of `runtime`: its heap under the budget of the
    /// runtime, its refusal of modules, the private roots of the host and
    /// the interrupt handler.
    pub(crate) fn start(runtime: &JsRuntime) -> rquickjs::Result<Self> {
        // The budget is active from CONSTRUCTION, so no allocation is ever
        // unbounded. QuickJS's own `set_memory_limit` is deliberately NOT used:
        // it rejects before calling the allocator, so a refusal would never
        // reach the structural flag and would be indistinguishable from a user
        // throw once it surfaced as an exception.
        let budget = runtime.heap.clone();
        let rt = Runtime::new_with_alloc(alloc::BudgetedAllocator::new(budget.clone()))?;
        // QuickJS anchored its stack budget on the pointer it recorded in the
        // line above and never moves it again, so the size has to be set here,
        // before any query can recurse beneath it. See `MAX_JS_STACK_BYTES`.
        rt.set_max_stack_size(MAX_JS_STACK_BYTES);
        // This runtime never reads a module. The explicit resolver is the
        // enforcement boundary for dynamically constructed imports
        // (`eval`/`Function`) that cannot be rejected by source inspection.
        // It also keeps a later QuickJS default change from silently turning
        // score text into filesystem or network access. A script that the
        // host has run resolves to an empty module. See `LoadedImports`.
        let loaded_imports = runtime.loaded_imports.clone();
        rt.set_loader(
            RejectModuleResolver(loaded_imports.clone()),
            RejectModuleLoader(loaded_imports.clone()),
        );
        let ctx = Context::full(&rt)?;
        let effect_policy = runtime.effect_policy.clone();
        let effect_policy_refusal = runtime.effect_policy_refusal.clone();
        let callback_ir = Rc::clone(runtime.callback_ir());
        ctx.with(|ctx| -> rquickjs::Result<()> {
            let held = rquickjs::Array::new(ctx.clone())?;
            held.as_object().set_prototype(None)?;
            let active = rquickjs::Array::new(ctx.clone())?;
            active.as_object().set_prototype(None)?;
            active.set(0, rquickjs::Value::new_null(ctx.clone()))?;
            let stack = rquickjs::Array::new(ctx.clone())?;
            // This is host-only storage, not a JavaScript Array surface. A
            // normal Array prototype lets score code install indexed setters
            // that observe or swallow the temporary owner pushed while a
            // cross-turn `__pure` value is rematerialised. Keep Array's own
            // length/index mechanics while removing that mutable prototype
            // chain before any user code can run.
            stack.as_object().set_prototype(None)?;
            let score_candidate = rquickjs::Array::new(ctx.clone())?;
            score_candidate.as_object().set_prototype(None)?;
            let repl_tempo_surface = rquickjs::Array::new(ctx.clone())?;
            repl_tempo_surface.as_object().set_prototype(None)?;
            let slider_sets = rquickjs::Array::new(ctx.clone())?;
            slider_sets.as_object().set_prototype(None)?;
            let slider_values = rquickjs::Object::new(ctx.clone())?;
            slider_values.set_prototype(None)?;
            slider_sets.set(0, slider_values.clone())?;
            slider_sets.set(1, slider_values)?;
            slider_sets.set(2, rquickjs::Value::new_null(ctx.clone()))?;
            // Parallel host-only binding maps: active, candidate, last-good.
            // Decimal strings retain all 64 token bits without JS Number rounding.
            let bindings = rquickjs::Object::new(ctx.clone())?;
            bindings.set_prototype(None)?;
            slider_sets.set(3, bindings.clone())?;
            slider_sets.set(4, bindings)?;
            slider_sets.set(5, rquickjs::Value::new_null(ctx.clone()))?;
            let voicing_sets = rquickjs::Array::new(ctx.clone())?;
            voicing_sets.as_object().set_prototype(None)?;
            let previous = ctx.store_userdata(HostRoots {
                held,
                active,
                stack,
                score_candidate,
                repl_tempo_surface,
                slider_sets,
                voicing_sets,
                protected_globals: Rc::new(RefCell::new(BTreeSet::new())),
                callback_ir: callback_ir.clone(),
                builtin_methods: Rc::new(RefCell::new(BuiltinMethods::default())),
                effects: EffectBoundaryState {
                    policy: effect_policy.clone(),
                    refusal: effect_policy_refusal.clone(),
                },
                heap: budget.clone(),
            })?;
            debug_assert!(previous.is_none(), "new runtime already had host roots");
            Ok(())
        })?;

        // One evaluation deadline spans the initial setup call
        // and every runnable QuickJS job it queues. JS_ExecutePendingJob may be
        // interrupted safely only if its borrowed error-context pointer is
        // duplicated before Rust owns it and its pending exception is consumed
        // immediately. rquickjs 0.14 fixes borrowed-context ownership, but
        // its bool helper consumes job exceptions without reporting them.
        // A pending root Promise with an empty job
        // queue still is unsupported: it needs an external host-I/O source,
        // not more polling.
        {
            let d = runtime.deadline.clone();
            let i = runtime.interrupted.clone();
            let flag = runtime.cancel_flag.clone();
            let c = runtime.cancelled.clone();
            rt.set_interrupt_handler(Some(Box::new(move || {
                let cancellation_requested = flag.get().is_some_and(|pointer| {
                    // SAFETY: installed only for the duration of the call that
                    // borrowed the AtomicBool, and cleared by an RAII guard
                    // before that borrow can end.
                    unsafe { &*pointer }.load(std::sync::atomic::Ordering::Relaxed)
                });
                if cancellation_requested {
                    c.set(true);
                    return true;
                }
                match d.get() {
                    Some(t) if std::time::Instant::now() >= t => {
                        i.set(true);
                        true // interrupt
                    }
                    _ => false,
                }
            })));
        }
        Ok(Self { rt, ctx })
    }
}

impl JsRuntime {
    /// The script engine, which starts here at its first use.
    ///
    /// # Panics
    ///
    /// When the engine has no memory to start with, as any allocation that
    /// fails.
    pub(crate) fn realm(&self) -> &Realm {
        self.realm
            .get_or_init(|| Realm::start(self).expect("the script engine has the memory to start"))
    }
}
