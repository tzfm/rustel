use super::*;
use rustel_core::reference::ReferenceEntry;

mod common;
mod extras;
mod midimap;
mod pattern_surface;
mod registration;

pub(crate) fn install_midimap_surface<'js>(ctx: &Ctx<'js>) -> Result<(), String> {
    midimap::install(ctx)
}

/// Give the score surface back the names the last score's `register()`
/// calls took from it.
pub(crate) fn restore_registration_surface(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    registration::restore_surface(ctx)
}

/// Keep what has been registered so far: it is the surface the scores that
/// follow start from, not something one of them borrowed.
pub(crate) fn keep_registration_surface(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    registration::forget_surface(ctx)
}

pub(crate) struct RegistrationSeal {
    names: Rc<RefCell<HashSet<String>>>,
}

impl RegistrationSeal {
    pub(crate) fn finish(&self, proto: &rquickjs::Object<'_>) -> Result<(), String> {
        let extensions: HashSet<_> = registry()
            .names()
            .into_iter()
            .filter(|name| {
                registry()
                    .get(name)
                    .is_some_and(|entry| entry.declared_in.is_extension())
            })
            .collect();
        let mut names = self.names.borrow_mut();
        for name in proto.own_keys::<String>(rquickjs::object::Filter::new().string()) {
            let name = name.map_err(|error| error.to_string())?;
            if !extensions.contains(name.as_str()) {
                names.insert(name);
            }
        }
        Ok(())
    }
}

fn install_value_methods<'js>(ctx: &Ctx<'js>, proto: &rquickjs::Object<'js>) -> Result<(), String> {
    let with_value = Function::new(
        ctx.clone(),
        hr_this_arg(move |ctx, this, callback| {
            if callback.0.is_none() {
                return Err(refuse_empty_call(&ctx, ".withValue", 1));
            }
            let receiver = pattern_with_own_steps(&ctx, &this.0)?;
            let mut sidecars = vec![Sidecar::of(&this.0.borrow())];
            let Some(callable) = callback.0.as_ref().and_then(rquickjs::Value::as_function) else {
                return derive_wrapper(
                    ctx,
                    rustel_core::query_error_pattern("func is not a function"),
                    &sidecars,
                );
            };
            let (id, sidecar) = bridge_callable(&ctx, callable.clone())?;
            sidecars.push(sidecar);
            derive_wrapper(ctx, receiver.fmap_js(id), &sidecars)
        }),
    )
    .map_err(|error| error.to_string())?;
    with_value
        .set_length(1)
        .map_err(|error| error.to_string())?;
    with_value
        .set_name("withValue")
        .map_err(|error| error.to_string())?;
    proto
        .prop(
            "withValue",
            rquickjs::object::Property::from(with_value)
                .writable()
                .configurable(),
        )
        .map_err(|error| error.to_string())?;

    for (name, mode) in [
        ("innerJoin", rustel_core::JoinMode::Inner),
        ("outerJoin", rustel_core::JoinMode::Outer),
        ("join", rustel_core::JoinMode::Mix),
        ("squeezeJoin", rustel_core::JoinMode::Squeeze),
        ("resetJoin", rustel_core::JoinMode::Reset),
        ("restartJoin", rustel_core::JoinMode::Restart),
    ] {
        let join = Function::new(
            ctx.clone(),
            hr_this_nullary(move |ctx, this| {
                let (receiver, sidecar) = {
                    let borrowed = this.0.borrow();
                    (borrowed.pattern.clone(), Sidecar::of(&borrowed))
                };
                let pattern = match mode {
                    rustel_core::JoinMode::Inner => receiver.inner_join(),
                    rustel_core::JoinMode::Outer => receiver.outer_join(),
                    rustel_core::JoinMode::Mix => receiver.mix_join(),
                    rustel_core::JoinMode::Squeeze => receiver.squeeze_join(),
                    rustel_core::JoinMode::Reset => receiver.reset_join(),
                    rustel_core::JoinMode::Restart => receiver.restart_join(),
                };
                derive_wrapper(ctx, pattern, &[sidecar])
            }),
        )
        .map_err(|error| error.to_string())?;
        join.set_name(name).map_err(|error| error.to_string())?;
        proto
            .prop(
                name,
                rquickjs::object::Property::from(join)
                    .writable()
                    .configurable(),
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Reference entries for methods that flatten a pattern of patterns.
pub(crate) const JOIN_REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "innerJoin",
        synonyms: &[],
        summary: "flatten a pattern of patterns, keeping the inner patterns' timing",
        description: "Flattens a pattern of patterns using the inner events' full spans. This is the default join for register() combinators.",
        params: &[],
        examples: &["note(\"c3 e3 g3\").fmap(x => pure(x).fast(2)).innerJoin()"],
        tags: &["combiners", "bind"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "join",
        synonyms: &[],
        summary: "flatten a pattern of patterns, keeping timing from both sides",
        description: "Flattens a pattern of patterns using the intersection of the inner and outer events' full spans. bind(func) combines mapping and this join.",
        params: &[],
        examples: &["note(\"c3 e3 g3\").fmap(x => pure(x).fast(2)).join()"],
        tags: &["combiners", "bind"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "outerJoin",
        synonyms: &[],
        summary: "flatten a pattern of patterns, keeping the outer pattern's timing",
        description: "Flattens a pattern of patterns using the outer events' full spans.",
        params: &[],
        examples: &["note(\"c3 e3 g3\").fmap(x => pure(x).fast(2)).outerJoin()"],
        tags: &["combiners", "bind"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "resetJoin",
        synonyms: &[],
        summary: "flatten a pattern of patterns, realigning each inner cycle to the outer onsets",
        description: "Flattens a pattern whose values are patterns, re-aligning the start of the inner pattern's current cycle to every onset of the outer pattern.",
        params: &[],
        examples: &["note(\"c3 e3 g3\").fmap(x => pure(x).fast(2)).resetJoin()"],
        tags: &["combiners", "bind"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "restartJoin",
        synonyms: &[],
        summary: "flatten a pattern of patterns, restarting each inner pattern at the outer onsets",
        description: "Flattens a pattern whose values are patterns, re-aligning the inner pattern's time zero to every onset of the outer pattern - the inner pattern restarts from its beginning rather than continuing its cycle.",
        params: &[],
        examples: &["note(\"c3 e3 g3\").fmap(x => pure(x).fast(2)).restartJoin()"],
        tags: &["combiners", "bind"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "squeezeJoin",
        synonyms: &[],
        summary: "squeeze a whole cycle of each inner pattern into its outer event",
        description: "Fits one cycle of each inner pattern into its corresponding outer event. Used by ply and chop.",
        params: &[],
        examples: &["note(\"c3 e3 g3\").fmap(x => pure(x).fast(2)).squeezeJoin()"],
        tags: &["combiners", "bind"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
];

pub(crate) fn install_user_setup_surface<'js>(
    ctx: &Ctx<'js>,
    proto: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
    install_canonical_shrink_grow: &rquickjs::Function<'js>,
) -> Result<RegistrationSeal, String> {
    install_value_methods(ctx, proto)?;
    let pattern = pattern_surface::install(ctx, proto)?;
    let pure: Function = globals.get("pure").map_err(|error| error.to_string())?;
    let fastcat: Function = globals.get("fastcat").map_err(|error| error.to_string())?;
    let registration = registration::install(ctx, proto, pattern.clone(), pure, fastcat)?;

    let canonical: rquickjs::Array = install_canonical_shrink_grow
        .call((
            registration.register_canonical.clone(),
            registration.register_raw_only.clone(),
            registration.finalize_registered_pure.clone(),
        ))
        .map_err(|error| describe_js_error(ctx, error))?;
    let shrink: rquickjs::Value = canonical.get(0).map_err(|error| error.to_string())?;
    let grow: rquickjs::Value = canonical.get(1).map_err(|error| error.to_string())?;
    let shrink_method: rquickjs::Value = canonical.get(2).map_err(|error| error.to_string())?;
    let grow_method: rquickjs::Value = canonical.get(3).map_err(|error| error.to_string())?;
    let extras = extras::install(ctx, registration.reify.clone())?;
    // Capture the intrinsic before setup or score code can replace it. It
    // lives only in host userdata and Rust later supplies the callback as the
    // exact `this`, so recognition trusts neither a mutable global nor a
    // user-defined property getter.
    let function: Function = globals.get("Function").map_err(|error| error.to_string())?;
    let function_prototype: rquickjs::Object = function
        .as_inner()
        .get("prototype")
        .map_err(|error| error.to_string())?;
    let callback_source: Function = function_prototype
        .get("toString")
        .map_err(|error| error.to_string())?;

    globals
        .set("Pattern", pattern)
        .map_err(|error| error.to_string())?;
    globals
        .set("shrink", shrink.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("grow", grow.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("s_taper", shrink)
        .map_err(|error| error.to_string())?;
    proto
        .set("shrink", shrink_method.clone())
        .map_err(|error| error.to_string())?;
    proto
        .set("grow", grow_method.clone())
        .map_err(|error| error.to_string())?;
    proto
        .set("s_taper", shrink_method)
        .map_err(|error| error.to_string())?;
    globals
        .set("register", registration.register)
        .map_err(|error| error.to_string())?;
    globals
        .set("reify", registration.reify.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("setStringParser", registration.set_string_parser.clone())
        .map_err(|error| error.to_string())?;
    globals
        .set("noteToMidi", extras.note_to_midi)
        .map_err(|error| error.to_string())?;
    globals
        .set("rustelScope", registration.scope.clone())
        .map_err(|error| error.to_string())?;

    let stack = host_stack(ctx).map_err(|error| error.to_string())?;
    common::define_reserved(
        stack.as_object(),
        RAW_ARP_SELECTOR_FACTORY,
        extras.raw_arp_selector.into_value(),
    )
    .map_err(|error| error.to_string())?;
    common::define_reserved(
        stack.as_object(),
        LEXICAL_REIFY,
        registration.reify.into_value(),
    )
    .map_err(|error| error.to_string())?;
    common::define_reserved(
        stack.as_object(),
        CALLBACK_SOURCE,
        callback_source.into_value(),
    )
    .map_err(|error| error.to_string())?;
    common::define_reserved(
        stack.as_object(),
        LEXICAL_SET_STRING_PARSER,
        registration.set_string_parser.into_value(),
    )
    .map_err(|error| error.to_string())?;

    globals
        .set("window", globals.clone())
        .map_err(|error| error.to_string())?;
    populate_projected_rustel_scope(&registration.scope, globals)?;
    Ok(RegistrationSeal {
        names: registration.sealed,
    })
}

pub(crate) fn populate_projected_rustel_scope<'js>(
    scope: &rquickjs::Object<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let mut count = 0usize;
    let mut seen = std::collections::BTreeSet::new();
    for name in SUPPORTED_GLOBAL_NAMES
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        if !seen.insert(name) {
            return Err(format!("native rustelScope manifest repeats {name}"));
        }
        let value: rquickjs::Value = globals
            .get(name)
            .map_err(|error| format!("read native rustelScope global {name}: {error}"))?;
        if value.is_undefined() {
            return Err(format!(
                "native rustelScope manifest names missing global {name}"
            ));
        }
        scope
            .set(name, value)
            .map_err(|error| format!("install native rustelScope entry {name}: {error}"))?;
        count += 1;
    }
    if count != SUPPORTED_GLOBAL_COUNT {
        return Err(format!(
            "native rustelScope manifest has {count} entries, expected {SUPPORTED_GLOBAL_COUNT}"
        ));
    }
    Ok(())
}
