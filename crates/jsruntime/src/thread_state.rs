use super::*;

thread_local! {
    pub(super) static CELLS_DROPPED: Cell<u64> = const { Cell::new(0) };
    pub(super) static CELLS_CREATED: Cell<u64> = const { Cell::new(0) };
    pub(super) static WRAPPERS_DROPPED: Cell<u64> = const { Cell::new(0) };
    static OWNERSHIP_REFUSAL:
        RefCell<Option<rustel_core::QueryLimit>> = const { RefCell::new(None) };
    /// The exact JavaScript value an EAGER transformer callback threw - a
    /// transformer invoked synchronously by the method the score is calling
    /// (`register()`'s fast path, `superimpose`/`layer`), not by a query.
    /// The core host can only contain a callback failure (a pending query
    /// error plus a fallback pattern), which is right inside a query and
    /// wrong during construction: there is no `queryArc` boundary to take it,
    /// so the score would install with silence baked in where the throwing
    /// branch was. The registered method rethrows this value instead,
    /// exactly as upstream's synchronous `func(pat)` call would have thrown
    /// it.
    ///
    /// ```text
    /// eager callback throws
    ///   -> callback_host_error  holds the value here, returns a sentinel
    ///   -> core host            pending query error + fallback pattern
    ///   -> later eager calls    get the sentinel; their callbacks do not run
    ///   -> one taker clears the value and the pending error together:
    ///      rethrow_eager_callback_exception        registered method throws
    ///      take_unthrown_eager_callback_exception  evaluation end reports
    ///      discard_eager_callback_exception        evaluation entry and exit
    /// ```
    static EAGER_CALLBACK_EXCEPTION:
        RefCell<Option<rquickjs::Persistent<rquickjs::Value<'static>>>> =
        const { RefCell::new(None) };
    static CURRENT_CTX: Cell<Option<*mut core::ffi::c_void>> = const { Cell::new(None) };
}

pub(super) fn record_ownership_refusal(limit: rustel_core::QueryLimit) {
    let limit = rustel_core::mark_stepwise_refusal(limit);
    OWNERSHIP_REFUSAL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(limit);
        }
    });
}

pub(super) fn take_ownership_refusal() -> Option<rustel_core::QueryLimit> {
    OWNERSHIP_REFUSAL.with(|slot| slot.borrow_mut().take())
}

const EAGER_CALLBACK_EXCEPTION_SENTINEL: &str = "eager callback threw";

/// Describe a transformer callback's failure for the core host - or, when
/// the callback ran EAGERLY and threw, keep the thrown value for
/// [`rethrow_eager_callback_exception`] and hand the core only a sentinel.
pub(super) fn callback_host_error(ctx: &Ctx<'_>, error: rquickjs::Error, eager: bool) -> String {
    if eager && matches!(error, rquickjs::Error::Exception) {
        let exception = ctx.catch();
        // The first throw wins, as it would have stopped upstream's
        // synchronous calls: a later eager callback in the same method body
        // must not replace the value the score is about to see.
        EAGER_CALLBACK_EXCEPTION.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(rquickjs::Persistent::save(ctx, exception));
            }
        });
        EAGER_CALLBACK_EXCEPTION_SENTINEL.into()
    } else {
        describe_js_error(ctx, error)
    }
}

/// Take the held eager exception, if any, together with the sentinel query
/// error the core host signaled for it.
///
/// The two are one unit: `callback_host_error` holds the thrown value and
/// hands the core only the sentinel, which the core signals as a pending
/// query error. Whoever takes the value - to rethrow it, to report it, or to
/// drop it - takes the sentinel with it. Left pending, the sentinel would
/// outlive the value and cut short the next evaluation's callback loops.
fn take_held_eager_callback_exception() -> Option<rquickjs::Persistent<rquickjs::Value<'static>>> {
    let exception = EAGER_CALLBACK_EXCEPTION.with(|slot| slot.borrow_mut().take())?;
    let _ = rustel_core::take_query_error();
    Some(exception)
}

/// Whether an eager callback's throw is held, waiting for the registered
/// method that invoked it to rethrow it.
fn eager_callback_exception_held() -> bool {
    EAGER_CALLBACK_EXCEPTION.with(|slot| slot.borrow().is_some())
}

/// The answer an EAGER transformer call gets without running its callback
/// while an earlier eager throw is held.
///
/// Upstream that throw is already unwinding: no user code runs between it
/// and the method call it unwinds through, so a body that goes on to invoke
/// a transformer again (`applyN`'s loop, `superimpose`'s map) must not run
/// it. The call gets the same sentinel the first throw produced, so the core
/// host contains it identically, and the held value stays the one the method
/// rethrows. A later callback run here instead could observe that value
/// through a registered call of its own - which rethrows it - and swallow
/// it, and the score would install with silence where the first callback
/// threw.
pub(super) fn refuse_eager_call_behind_a_held_throw(eager: bool) -> Result<(), String> {
    if eager && eager_callback_exception_held() {
        Err(EAGER_CALLBACK_EXCEPTION_SENTINEL.into())
    } else {
        Ok(())
    }
}

/// Throw what an eager transformer callback threw from the registered method
/// that invoked it, so the score sees a real exception at that call: its own
/// `try`/`catch` observes the exact value, and an uncaught one stops the
/// score there and fails its evaluation with the callback's own line.
pub(super) fn rethrow_eager_callback_exception(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    let Some(exception) = take_held_eager_callback_exception() else {
        return Ok(());
    };
    let value = exception.restore(ctx)?;
    Err(ctx.throw(value))
}

/// Describe an eager callback's throw that no registered method rethrew -
/// an entry point without the rethrow.
pub(super) fn take_unthrown_eager_callback_exception(ctx: &Ctx<'_>) -> Option<String> {
    let exception = take_held_eager_callback_exception()?;
    Some(match exception.restore(ctx) {
        Ok(value) => describe_caught_js_value(value),
        Err(error) => describe_js_error(ctx, error),
    })
}

/// Drop a stale eager exception: one evaluation's leftover must never be
/// thrown into the next.
pub(super) fn discard_eager_callback_exception() {
    let _ = take_held_eager_callback_exception();
}

pub(super) fn with_ctx<R>(runtime: &JsRuntime, f: impl FnOnce(Ctx<'_>) -> R) -> R {
    runtime.script_may_have_run.set(true);
    enter_ctx(&runtime.realm().ctx, f)
}

/// [`with_ctx`] for host reads that run no score code: reading a host slot,
/// or discarding queued jobs without running them. Score JavaScript cannot
/// run inside, so no native panic it caught can be left behind. See
/// [`JsRuntime::raise_caught_native_panic`].
pub(super) fn with_ctx_running_no_script<R>(
    runtime: &JsRuntime,
    f: impl FnOnce(Ctx<'_>) -> R,
) -> R {
    enter_ctx(&runtime.realm().ctx, f)
}

fn enter_ctx<R>(ctx_owner: &Context, f: impl FnOnce(Ctx<'_>) -> R) -> R {
    let current = CURRENT_CTX.with(Cell::get);
    match current {
        Some(pointer) => {
            // SAFETY: the pointer is installed only for the active
            // `Context::with` call on this thread and cleared on exit.
            f(unsafe { Ctx::from_raw(std::ptr::NonNull::new_unchecked(pointer.cast())) })
        }
        None => ctx_owner.with(|ctx| {
            let raw = ctx.as_raw().as_ptr().cast::<core::ffi::c_void>();
            let previous = CURRENT_CTX.with(|current| current.replace(Some(raw)));
            struct Guard(Option<*mut core::ffi::c_void>);
            impl Drop for Guard {
                fn drop(&mut self) {
                    CURRENT_CTX.with(|current| current.set(self.0));
                }
            }
            let _guard = Guard(previous);
            f(ctx)
        }),
    }
}
