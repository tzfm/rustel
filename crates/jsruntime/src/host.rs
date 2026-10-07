use super::*;

pub(super) struct RejectModuleResolver;

impl rquickjs::loader::Resolver for RejectModuleResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        Err(rquickjs::Error::new_resolving_message(
            base,
            name,
            "module loading is disabled in native score code",
        ))
    }
}

pub(super) struct RejectModuleLoader;

impl rquickjs::loader::Loader for RejectModuleLoader {
    fn load<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<rquickjs::Module<'js>> {
        Err(rquickjs::Error::new_loading_message(
            name,
            "module loading is disabled in native score code",
        ))
    }
}

// QuickJS discards one snapshot at a time. Releasing a job's arguments can
// enqueue FinalizationRegistry cleanup jobs, so Rustel must drain subsequent
// snapshots too to keep refused turns from leaking work into the next turn.
// This terminates because our native finalizers only release ownership and
// registry cleanup consumes existing registrations without executing JS.
//
// SAFETY: `runtime` must be live and exclusively locked, outside JavaScript/job
// execution. Native finalizers must not execute JS or reenter this helper.
pub(super) unsafe fn js_discard_pending_jobs(runtime: *mut rquickjs::qjs::JSRuntime) -> usize {
    let mut total: usize = 0;
    loop {
        // SAFETY: the caller guarantees exclusive access outside JS execution.
        let count = unsafe { rquickjs::qjs::JS_DiscardPendingJobs(runtime) };
        let count = usize::try_from(count).expect(rquickjs::qjs::SIZE_T_ERROR);
        if count == 0 {
            return total;
        }
        total = total.saturating_add(count);
    }
}

pub(super) fn discard_pending_jobs(ctx: &Ctx<'_>) -> usize {
    // SAFETY: callers hold the context's runtime lock, outside JavaScript/job
    // execution. Rustel finalizers neither execute JavaScript nor reenter here.
    unsafe { js_discard_pending_jobs(rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr())) }
}

#[derive(Clone)]
pub(super) struct EffectBoundaryState {
    pub(super) policy: Rc<Cell<EffectPolicy>>,
    pub(super) refusal: Rc<RefCell<Option<String>>>,
}

pub(super) struct HostRoots<'js> {
    pub(super) held: rquickjs::Array<'js>,
    pub(super) active: rquickjs::Array<'js>,
    pub(super) stack: rquickjs::Array<'js>,
    pub(super) score_candidate: rquickjs::Array<'js>,
    pub(super) repl_tempo_surface: rquickjs::Array<'js>,
    pub(super) slider_sets: rquickjs::Array<'js>,
    pub(super) voicing_sets: rquickjs::Array<'js>,
    pub(super) protected_globals: Rc<RefCell<BTreeSet<String>>>,
    pub(super) pattern_transform_ir_cache: Rc<RefCell<PatternTransformIrCache>>,
    pub(super) effects: EffectBoundaryState,
    /// The budget the allocator enforces, shared so host-side mirrors of JS
    /// values can bound their eager reservations by the LIVE ceiling rather
    /// than only by the default one. See [`host_heap_ceiling`].
    pub(super) heap: Rc<alloc::HeapBudget>,
}

const MAX_PATTERN_TRANSFORM_IR_CACHE_ENTRIES: usize = 256;
const MAX_PATTERN_TRANSFORM_IR_CACHE_SOURCE_BYTES: usize = 64 * 1024;

#[derive(Clone)]
struct CachedPatternTransform {
    source: Box<str>,
    candidate: rustel_transpiler::PatternTransformCandidate,
}

/// Runtime-local, success-only callback compilation cache.
///
/// The cache dies with the QuickJS realm, so a new binary/IR version cannot
/// observe an entry built by an older implementation. FIFO eviction keeps
/// both entry count and retained source bytes bounded during edit churn.
#[derive(Default)]
pub(super) struct PatternTransformIrCache {
    entries: std::collections::VecDeque<CachedPatternTransform>,
    source_bytes: usize,
}

impl PatternTransformIrCache {
    pub(super) fn get(&self, source: &str) -> Option<rustel_transpiler::PatternTransformCandidate> {
        self.entries
            .iter()
            .find(|entry| entry.source.as_ref() == source)
            .map(|entry| entry.candidate)
    }

    pub(super) fn insert(
        &mut self,
        source: &str,
        candidate: rustel_transpiler::PatternTransformCandidate,
    ) {
        if source.len() > MAX_PATTERN_TRANSFORM_IR_CACHE_SOURCE_BYTES || self.get(source).is_some()
        {
            return;
        }
        let mut owned = String::new();
        if owned.try_reserve_exact(source.len()).is_err() || self.entries.try_reserve(1).is_err() {
            return;
        }
        owned.push_str(source);
        while self.entries.len() >= MAX_PATTERN_TRANSFORM_IR_CACHE_ENTRIES
            || self.source_bytes.saturating_add(owned.len())
                > MAX_PATTERN_TRANSFORM_IR_CACHE_SOURCE_BYTES
        {
            let Some(evicted) = self.entries.pop_front() else {
                return;
            };
            self.source_bytes = self.source_bytes.saturating_sub(evicted.source.len());
        }
        self.source_bytes = self.source_bytes.saturating_add(owned.len());
        self.entries.push_back(CachedPatternTransform {
            source: owned.into_boxed_str(),
            candidate,
        });
    }
}

unsafe impl<'js> JsLifetime<'js> for HostRoots<'js> {
    type Changed<'to> = HostRoots<'to>;
}

fn root_array<'js>(
    ctx: &Ctx<'js>,
    name: &'static str,
    get: impl FnOnce(&HostRoots<'js>) -> rquickjs::Array<'js>,
) -> rquickjs::Result<rquickjs::Array<'js>> {
    ctx.userdata::<HostRoots>()
        .map(|roots| get(&roots))
        .ok_or_else(|| {
            rquickjs::Error::new_from_js_message(
                "runtime userdata",
                name,
                "private host roots are not installed",
            )
        })
}

pub(super) fn host_active<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    root_array(ctx, "active graph", |roots| roots.active.clone())
}

pub(super) fn host_held<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    root_array(ctx, "held graphs", |roots| roots.held.clone())
}

pub(super) fn host_stack<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    root_array(ctx, "host roots", |roots| roots.stack.clone())
}

pub(super) fn host_score_candidate<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    root_array(ctx, "staged score", |roots| roots.score_candidate.clone())
}

pub(super) fn host_repl_tempo_surface<'js>(
    ctx: &Ctx<'js>,
) -> rquickjs::Result<rquickjs::Array<'js>> {
    root_array(ctx, "REPL tempo surface", |roots| {
        roots.repl_tempo_surface.clone()
    })
}

pub(super) fn host_slider_sets<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    root_array(ctx, "slider values", |roots| roots.slider_sets.clone())
}

pub(super) fn host_voicing_sets<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    root_array(ctx, "voicing registries", |roots| {
        roots.voicing_sets.clone()
    })
}

pub(super) fn host_protected_globals(
    ctx: &Ctx<'_>,
) -> rquickjs::Result<Rc<RefCell<BTreeSet<String>>>> {
    ctx.userdata::<HostRoots>()
        .map(|roots| roots.protected_globals.clone())
        .ok_or_else(|| {
            rquickjs::Error::new_from_js_message(
                "runtime userdata",
                "protected globals",
                "private host roots are not installed",
            )
        })
}

/// Register a late host binding without protecting unrelated user globals.
pub(super) fn set_host_global<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    value: impl rquickjs::IntoJs<'js>,
) -> rquickjs::Result<()> {
    ctx.globals().set(name, value)?;
    protect_host_global(ctx, name)?;
    Ok(())
}

/// Protect a late host name from score cleanup. Returns whether the name was
/// unprotected before.
pub(super) fn protect_host_global(ctx: &Ctx<'_>, name: &str) -> rquickjs::Result<bool> {
    Ok(host_protected_globals(ctx)?
        .borrow_mut()
        .insert(name.to_owned()))
}

/// Let score cleanup remove a name the host no longer defines.
#[cfg(feature = "hydra")]
pub(super) fn release_host_global(ctx: &Ctx<'_>, name: &str) -> rquickjs::Result<()> {
    host_protected_globals(ctx)?.borrow_mut().remove(name);
    Ok(())
}

pub(super) fn host_pattern_transform_ir_cache(
    ctx: &Ctx<'_>,
) -> rquickjs::Result<Rc<RefCell<PatternTransformIrCache>>> {
    ctx.userdata::<HostRoots>()
        .map(|roots| roots.pattern_transform_ir_cache.clone())
        .ok_or_else(|| {
            rquickjs::Error::new_from_js_message(
                "runtime userdata",
                "callback IR cache",
                "private host roots are not installed",
            )
        })
}

pub(super) fn has_own_property(
    ctx: &Ctx<'_>,
    object: &rquickjs::Object<'_>,
    name: &str,
) -> rquickjs::Result<bool> {
    let raw = ctx.as_raw().as_ptr();
    let atom = unsafe { rquickjs::qjs::JS_NewAtomLen(raw, name.as_ptr().cast(), name.len() as _) };
    if atom == rquickjs::qjs::JS_ATOM_NULL {
        return Err(rquickjs::Error::Allocation);
    }
    let status = unsafe {
        rquickjs::qjs::JS_GetOwnProperty(raw, std::ptr::null_mut(), object.as_raw(), atom)
    };
    unsafe { rquickjs::qjs::JS_FreeAtom(raw, atom) };
    match status {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(rquickjs::Error::Exception),
    }
}

pub(super) fn host_effect_boundary(ctx: &Ctx<'_>) -> rquickjs::Result<EffectBoundaryState> {
    ctx.userdata::<HostRoots>()
        .map(|roots| roots.effects.clone())
        .ok_or_else(|| {
            rquickjs::Error::new_from_js_message(
                "runtime userdata",
                "effect boundary",
                "private host roots are not installed",
            )
        })
}

/// The QuickJS heap ceiling currently in force, in bytes.
///
/// A host-side mirror of a JavaScript value - a materialized array - is
/// allocated by the GLOBAL allocator, which the budget cannot see. This is
/// where such a mirror derives its own element bound from the same ceiling,
/// so a JS-controlled `length` can never reserve host memory beyond what the
/// sandbox itself is allowed. Reading the LIVE ceiling (not the default
/// constant) keeps an embedder's `set_memory_limit` tightening in force here.
pub(super) fn host_heap_ceiling(ctx: &Ctx<'_>) -> rquickjs::Result<usize> {
    ctx.userdata::<HostRoots>()
        .map(|roots| roots.heap.limit())
        .ok_or_else(|| {
            rquickjs::Error::new_from_js_message(
                "runtime userdata",
                "heap ceiling",
                "private host roots are not installed",
            )
        })
}

pub(super) fn slider_cell_is_plain_number<'js>(
    ctx: &Ctx<'js>,
    values: &rquickjs::Object<'js>,
    id: &str,
) -> Result<bool, String> {
    let raw = ctx.as_raw().as_ptr();
    let atom = unsafe { rquickjs::qjs::JS_NewAtomLen(raw, id.as_ptr().cast(), id.len() as _) };
    if atom == rquickjs::qjs::JS_ATOM_NULL {
        return Err("could not allocate slider property key".into());
    }
    let mut descriptor = std::mem::MaybeUninit::<rquickjs::qjs::JSPropertyDescriptor>::uninit();
    let status = unsafe {
        rquickjs::qjs::JS_GetOwnProperty(raw, descriptor.as_mut_ptr(), values.as_raw(), atom)
    };
    unsafe { rquickjs::qjs::JS_FreeAtom(raw, atom) };
    if status < 0 {
        return Err(describe_js_error(ctx, rquickjs::Error::Exception));
    }
    if status == 0 {
        return Ok(false);
    }

    let descriptor = unsafe { descriptor.assume_init() };
    let plain = descriptor.flags & rquickjs::qjs::JS_PROP_WRITABLE as i32 != 0
        && unsafe { rquickjs::qjs::JS_IsUndefined(descriptor.getter) }
        && unsafe { rquickjs::qjs::JS_IsUndefined(descriptor.setter) }
        && unsafe { rquickjs::qjs::JS_IsNumber(descriptor.value) };
    let mut number = 0.0;
    let converted = if plain {
        unsafe { rquickjs::qjs::JS_ToFloat64(raw, &mut number, descriptor.value) }
    } else {
        0
    };
    unsafe {
        rquickjs::qjs::JS_FreeValue(raw, descriptor.value);
        rquickjs::qjs::JS_FreeValue(raw, descriptor.getter);
        rquickjs::qjs::JS_FreeValue(raw, descriptor.setter);
    }
    if converted < 0 {
        return Err(describe_js_error(ctx, rquickjs::Error::Exception));
    }
    Ok(plain && number.is_finite())
}

#[cfg(test)]
mod callback_ir_cache_tests {
    use super::*;

    const IDENTITY: rustel_transpiler::PatternTransformCandidate =
        rustel_transpiler::PatternTransformCandidate::Identity;

    #[test]
    fn cache_retains_only_successes_inserted_by_the_caller() {
        let mut cache = PatternTransformIrCache::default();
        assert_eq!(cache.get("x => x"), None);
        cache.insert("x => x", IDENTITY);
        assert_eq!(cache.get("x => x"), Some(IDENTITY));
        cache.insert("x => x", IDENTITY);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.source_bytes, "x => x".len());
    }

    #[test]
    fn cache_evicts_fifo_with_bounded_count_and_source_bytes() {
        let first = "p0 => p0";
        for multiplier in [1, 2] {
            let mut cache = PatternTransformIrCache::default();
            for index in 0..=(MAX_PATTERN_TRANSFORM_IR_CACHE_ENTRIES * multiplier) {
                cache.insert(&format!("p{index} => p{index}"), IDENTITY);
                assert!(cache.entries.len() <= MAX_PATTERN_TRANSFORM_IR_CACHE_ENTRIES);
                assert!(cache.source_bytes <= MAX_PATTERN_TRANSFORM_IR_CACHE_SOURCE_BYTES);
            }
            assert_eq!(cache.get(first), None);
            let last = MAX_PATTERN_TRANSFORM_IR_CACHE_ENTRIES * multiplier;
            assert_eq!(cache.get(&format!("p{last} => p{last}")), Some(IDENTITY));
        }

        let mut byte_bounded = PatternTransformIrCache::default();
        for index in 0..32 {
            let source = format!("{index:04}{}", "x".repeat(4_092));
            byte_bounded.insert(&source, IDENTITY);
            assert!(byte_bounded.source_bytes <= MAX_PATTERN_TRANSFORM_IR_CACHE_SOURCE_BYTES);
        }
        assert!(byte_bounded.entries.len() < MAX_PATTERN_TRANSFORM_IR_CACHE_ENTRIES);
        assert_eq!(
            byte_bounded.get(&format!("{:04}{}", 31, "x".repeat(4_092))),
            Some(IDENTITY)
        );
    }
}
