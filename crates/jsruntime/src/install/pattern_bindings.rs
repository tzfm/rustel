use super::*;
use rquickjs::function::Args;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

/// Reference entries for keep-based composition methods and step-counted joins.
pub(crate) const REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "maskAll",
        synonyms: &[],
        summary: "keep the pattern wherever the mask has an event, whatever its value",
        description: "Like mask, but keeps events wherever the mask has an event, including a value of 0. Rests leave gaps. The mask's control values merge into the result, with the original pattern's values taking precedence. Unlike mask, a mask such as gain(\"<1 0.5>\") can therefore supply gain values.",
        params: &[ReferenceParam {
            name: "mask",
            r#type: "Pattern",
            description: "keeps the pattern wherever it has an event, of any value; its control values merge in.",
        }],
        examples: &[
            "s(\"bd*4\").maskAll(\"1 0 ~ 1\")",
            "s(\"bd*4\").maskAll(gain(\"<1 0.5>\"))",
        ],
        tags: &["temporal"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "polyBind",
        synonyms: &[],
        summary: "map each value through a function that returns patterns, joined polyrhythmically",
        description: "Applies fmap, then polyJoin. Each returned pattern is extended to the outer pattern's step count before joining. A non-function argument produces a pattern whose queries return no events.",
        params: super::native_surface::bindings::BIND_FUNC,
        examples: &[],
        tags: &["functional", "bind"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "resetAll",
        synonyms: &[],
        summary: "reset the pattern at every onset of the given pattern, whatever its value",
        description: "Like reset, but resets at every onset of the supplied pattern, including events with a value of 0. The supplied pattern's control values merge into the result, with the original pattern's values taking precedence.",
        params: &[ReferenceParam {
            name: "pattern",
            r#type: "Pattern",
            description: "the reset onsets and control values to merge into the result.",
        }],
        examples: &["note(\"c3 e3 g3 b3\").resetAll(\"1 0\")"],
        tags: &["temporal"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "restartAll",
        synonyms: &[],
        summary: "restart the pattern from its beginning at every onset of the given pattern, whatever its value",
        description: "Like restart, but restarts from the beginning at every onset of the supplied pattern, including events with a value of 0. The supplied pattern's control values merge into the result, with the original pattern's values taking precedence.",
        params: &[ReferenceParam {
            name: "pattern",
            r#type: "Pattern",
            description: "the restart onsets and control values to merge into the result.",
        }],
        examples: &["note(\"c3 e3 g3 b3\").restartAll(\"1 0\")"],
        tags: &["temporal"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "stepBind",
        synonyms: &[],
        summary: "map each value through a function that returns patterns, joined by steps",
        description: "Applies the callback and joins the resulting patterns with stepJoin. The callback runs during pattern construction. Returning a non-pattern value causes a construction error; an exception thrown inside the callback is caught and produces an empty join.",
        params: super::native_surface::bindings::BIND_FUNC,
        examples: &[],
        tags: &["functional", "bind"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "stepJoin",
        synonyms: &[],
        summary: "flatten a pattern of patterns by the outer pattern's steps",
        description: "Flattens a pattern of patterns, aligned by the outer pattern's step count. It queries cycle zero during construction; non-pattern event values cause an error at that stage.",
        params: &[],
        examples: &[],
        tags: &["combiners", "bind"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "structAll",
        synonyms: &[],
        summary: "shape the pattern's timing to every event of the given pattern, whatever its value",
        description: "Like struct, but every event in the supplied pattern creates an event, including a value of 0; only rests leave gaps. Control values from the supplied pattern merge into the result, with the original pattern's values taking precedence.",
        params: &[ReferenceParam {
            name: "pattern",
            r#type: "Pattern",
            description: "the event timing and control values to merge into the result.",
        }],
        examples: &[
            "s(\"hh\").structAll(\"1 0 1 [0 1]\")",
            "s(\"bd*4\").structAll(gain(\"<1 [1 0.5]>\"))",
        ],
        tags: &["temporal"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
];

pub(super) fn install<'js>(ctx: Ctx<'js>, surface: &SemanticSurface<'js>) -> Result<(), String> {
    let globals = surface.globals.clone();
    let proto = surface.proto.clone();
    let set_canonical_polymeter_pace = surface.set_canonical_polymeter_pace.clone();
    let set_canonical_zoom = surface.set_canonical_zoom.clone();
    // -- prototype installation order -------------------------------
    //
    // Assignments onto `Pattern.prototype` land in a fixed order, and
    // the later assignment wins:
    //
    //   0. extension registrations
    //   1. pattern-module register()  - fast, slow, density, rev, …
    //   2. control accessors          - SHADOW `density`, which is
    //                                   a real control, not `fast`
    //   3. controls-module register() - SHADOWS `ds`, the envelope
    //                                   shorthand, over `delaysync`
    //
    // Installing combinators wholesale after controls loses step 2 and
    // makes `s("crackle*4").density(0.01)` a speed change instead of a
    // control.
    for name in registry().names() {
        let registration = registry().get(name).expect("name from registry");
        if !registration.declared_in.is_extension() {
            continue;
        }
        install_combinator(&ctx, &proto, &globals, name, registration.clone())?;
    }

    for name in registry().names() {
        let registration = registry().get(name).expect("name from registry");
        if registration.declared_in != rustel_core::register::DeclaredIn::PatternModule {
            continue;
        }
        install_combinator(&ctx, &proto, &globals, name, registration.clone())?;
        // Raw swing, range/range2, apply/when/never/always, the stepwise
        // pairs, and canonical shrink/grow are installed later through
        // private JavaScript register() calls. Reserve their
        // slots beside the initial public methods so replacement
        // preserves the interleaved prototype order without
        // publishing any raw spelling globally or in rustelScope.
        // `_rangex` remains an explicit uninstalled residual.
        if matches!(
            name,
            "swing"
                | "range"
                | "range2"
                | "apply"
                | "when"
                | "often"
                | "rarely"
                | "almostNever"
                | "almostAlways"
                | "never"
                | "always"
                | "take"
                | "drop"
                | "expand"
                | "extend"
                | "replicate"
                | "contract"
                | "shrink"
                | "grow"
        ) {
            proto
                .set(
                    format!("_{name}"),
                    rquickjs::Value::new_undefined(ctx.clone()),
                )
                .map_err(|error| error.to_string())?;
        }
    }
    let canonical_polymeter_pace: rquickjs::Value =
        proto.get("pace").map_err(|error| error.to_string())?;
    set_canonical_polymeter_pace
        .call::<_, ()>((canonical_polymeter_pace,))
        .map_err(|error| describe_js_error(&ctx, error))?;
    let canonical_zoom: rquickjs::Value = proto.get("zoom").map_err(|error| error.to_string())?;
    set_canonical_zoom
        .call::<_, ()>((canonical_zoom,))
        .map_err(|error| describe_js_error(&ctx, error))?;

    // -- controls ---------------------------------------------------
    // Controls whose value is CODE, not a pattern.
    //
    // `bbexpr('t*(t>>15^t>>66)')` is a bytebeat expression. Each of those
    // operators means something else to the mini parser, so reifying the
    // string either fails or changes the expression. The string stays
    // whole: the compatibility snapshot records
    // `byteBeatExpression:t*(t>>15^t>>66)` as a single value, so a literal
    // string reaches the hap unparsed. A double-quoted string is already a
    // pattern when it arrives (the transpiler rewrote it), so this sees only
    // the single-quoted form the docs use.
    const LITERAL_STRING_CONTROLS: &[&str] = &["byteBeatExpression", "bbexpr", "bb"];

    for name in control_registry().names() {
        let spec = control_registry()
            .get(name)
            .expect("name came from the registry")
            .clone();
        let literal = LITERAL_STRING_CONTROLS.contains(&name);
        let method_spec = spec.clone();
        let method = Function::new(
            ctx.clone(),
            hr_this_rest(move |ctx, this, args| {
                let (receiver, mut sidecars) = {
                    let borrowed = this.0.borrow();
                    (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
                };
                // A compound control's names are positional, so its
                // arguments are too: `limit(-6, "hard")` is the ceiling and
                // the character, the way `"-6:hard"` is in mini-notation.
                // Spelling them separately is what lets a pattern go on one
                // of them - `limit(slider(-6, -24, 0), "hard")` - which a
                // string glued together cannot do.
                let mut given = args.0.into_iter();
                let first = given.next().filter(|v| !v.is_undefined());
                let rest: Vec<_> = given.take_while(|v| !v.is_undefined()).collect();
                if !rest.is_empty() && method_spec.names.len() > 1 {
                    let mut parts = Vec::with_capacity(rest.len() + 1);
                    for value in first.into_iter().chain(rest) {
                        let (pattern, sidecar) = reify_bridged(&ctx, &value)?;
                        sidecars.push(sidecar);
                        parts.push(pattern);
                    }
                    let pattern = method_spec.apply_positional(&receiver, &parts);
                    return derive_wrapper(ctx, pattern, &sidecars);
                }
                // `if (typeof value === 'undefined') return pat.fmap(withVal);`
                let argument = first;
                let value = if let Some(value) = argument {
                    match value.as_string().filter(|_| literal) {
                        // A code expression: keep it whole.
                        Some(text) => Some(rustel_core::pure(rustel_core::Value::Str(
                            text.to_string()?,
                        ))),
                        None => {
                            let (pattern, sidecar) = reify_bridged(&ctx, &value)?;
                            sidecars.push(sidecar);
                            Some(pattern)
                        }
                    }
                } else {
                    None
                };
                let pattern = method_spec.apply(&receiver, value);
                derive_wrapper(ctx, pattern, &sidecars)
            }),
        )
        .map_err(|e| e.to_string())?;
        // `cps` is both an OSC control name and the absolute-tempo pattern
        // method. The pattern combinator installed above owns the
        // method; the free form is still installed below and the REPL later
        // replaces it with the session-tempo setter.
        if name != "cps" {
            proto.set(name, method).map_err(|e| e.to_string())?;
        }

        // `if (!pat) return reify(value).withValue(withVal);`
        let free = Function::new(
            ctx.clone(),
            hr_value(move |ctx, value| {
                let (value, sidecar) = reify_bridged(&ctx, &value)?;
                let pattern = spec.standalone(&value);
                derive_wrapper(ctx, pattern, &[sidecar])
            }),
        )
        .map_err(|e| e.to_string())?;
        globals.set(name, free).map_err(|e| e.to_string())?;
    }

    // -- combinators declared in the controls module (installed LAST) --
    for name in registry().names() {
        let registration = registry().get(name).expect("name from registry");
        if registration.declared_in != rustel_core::register::DeclaredIn::ControlsModule {
            continue;
        }
        install_combinator(&ctx, &proto, &globals, name, registration.clone())?;
    }

    // These are exact object aliases, not fresh
    // register() calls. Copy the final canonical values so aliases
    // share identity and do not acquire private `_alias` methods.
    // In particular, registering `steps` would overwrite the real
    // `_steps` accessor with a raw method.
    for (alias, canonical) in [
        ("timeCat", "stepcat"),
        ("s_cat", "stepcat"),
        ("s_taper", "shrink"),
        ("s_taperlist", "shrinklist"),
        ("s_add", "take"),
        ("s_alt", "stepalt"),
        ("s_polymeter", "polymeter"),
        ("s_sub", "drop"),
        ("s_expand", "expand"),
        ("s_extend", "extend"),
        ("s_contract", "contract"),
        ("s_tour", "tour"),
        ("s_zip", "zip"),
        ("steps", "pace"),
    ] {
        let value: rquickjs::Value = globals.get(canonical).map_err(|error| error.to_string())?;
        globals
            .set(alias, value)
            .map_err(|error| error.to_string())?;
    }
    // The canonical free `polymeter` has no Pattern method. The
    // alias-copy assignment nevertheless creates this ordinary own
    // undefined prototype slot before the callable taper aliases.
    proto
        .set("s_polymeter", rquickjs::Value::new_undefined(ctx.clone()))
        .map_err(|error| error.to_string())?;
    for (alias, canonical) in [
        ("s_taper", "shrink"),
        ("s_taperlist", "shrinklist"),
        ("s_add", "take"),
        ("s_sub", "drop"),
        ("s_expand", "expand"),
        ("s_extend", "extend"),
        ("s_contract", "contract"),
        ("s_tour", "tour"),
    ] {
        let value: rquickjs::Value = proto.get(canonical).map_err(|error| error.to_string())?;
        proto.set(alias, value).map_err(|error| error.to_string())?;
    }
    // `s_zip` is produced by the alias-copy loop even though
    // `zip` is a free function only. Reading the absent canonical
    // prototype property creates an own ordinary undefined data slot
    // in this exact position; it is not a Pattern method.
    proto
        .set("s_zip", rquickjs::Value::new_undefined(ctx.clone()))
        .map_err(|error| error.to_string())?;
    let pace: rquickjs::Value = proto.get("pace").map_err(|error| error.to_string())?;
    proto
        .set("steps", pace)
        .map_err(|error| error.to_string())?;

    // -- the COMPOSERS x ALIGNMENTS matrix --------------------------
    //
    // `pat.add` is a getter returning a callable that
    // also carries `.in`/`.out`/`.squeeze`/… . The shape
    // matters because both `pat.add(x)` and `pat.add.squeeze(x)` are
    // supported syntax.
    for op in rustel_core::compose::ComposeOp::ALL {
        let op = *op;
        if op == rustel_core::compose::ComposeOp::Set {
            install_raw_set(&ctx, &proto)?;
        }
        if op == rustel_core::compose::ComposeOp::Keep {
            install_raw_keep(&ctx, &proto)?;
        }
        if op == rustel_core::compose::ComposeOp::KeepIf {
            install_raw_keepif(&ctx, &proto)?;
        }
        if op == rustel_core::compose::ComposeOp::Eqt {
            install_raw_eqt(&ctx, &proto)?;
        }
        if op == rustel_core::compose::ComposeOp::Net {
            install_raw_net(&ctx, &proto)?;
        }
        if op == rustel_core::compose::ComposeOp::And {
            install_raw_and(&ctx, &proto)?;
        }
        if op == rustel_core::compose::ComposeOp::Or {
            install_raw_or(&ctx, &proto)?;
        }
        let wrapper = rquickjs::Object::new(ctx.clone()).map_err(|e| e.to_string())?;
        for how in rustel_core::compose::Alignment::ALL {
            let how = *how;
            let f = Function::new(
                ctx.clone(),
                hr_thisval_rest(move |ctx, this, args| {
                    compose_call(ctx, &this.0, &args.0, op, how)
                }),
            )
            .map_err(|e| e.to_string())?;
            wrapper.set(how.name(), f).map_err(|e| e.to_string())?;
        }
        // `wrapper.squeezein = wrapper.squeeze;`
        let squeezein: rquickjs::Value = wrapper.get("squeeze").map_err(|e| e.to_string())?;
        wrapper
            .set("squeezein", squeezein)
            .map_err(|e| e.to_string())?;
        let default_apply = Function::new(
            ctx.clone(),
            hr_thisval_rest(move |ctx, this, args| {
                compose_call(
                    ctx,
                    &this.0,
                    &args.0,
                    op,
                    rustel_core::compose::default_alignment(),
                )
            }),
        )
        .map_err(|error| error.to_string())?;
        install_composer(&ctx, &proto, op, wrapper, default_apply)?;
    }

    // The named shortcuts defined on top of the matrix:
    //
    // ```js
    // Pattern.prototype.struct     = (...a) => this.keepif.out(...a);
    // Pattern.prototype.structAll  = (...a) => this.keep.out(...a);
    // Pattern.prototype.mask       = (...a) => this.keepif.in(...a);
    // Pattern.prototype.maskAll    = (...a) => this.keep.in(...a);
    // Pattern.prototype.reset      = (...a) => this.keepif.reset(...a);
    // Pattern.prototype.resetAll   = (...a) => this.keep.reset(...a);
    // Pattern.prototype.restart    = (...a) => this.keepif.restart(...a);
    // Pattern.prototype.restartAll = (...a) => this.keep.restart(...a);
    // for (const how of ALIGNMENTS)
    //   Pattern.prototype[how.toLowerCase()] = (...a) => this.set[how.toLowerCase()](a);
    // ```
    use rustel_core::compose::{Alignment, ComposeOp};
    let shortcuts: Vec<(&str, ComposeOp, Alignment)> = vec![
        ("struct", ComposeOp::KeepIf, Alignment::Out),
        ("structAll", ComposeOp::Keep, Alignment::Out),
        ("mask", ComposeOp::KeepIf, Alignment::In),
        ("maskAll", ComposeOp::Keep, Alignment::In),
        ("reset", ComposeOp::KeepIf, Alignment::Reset),
        ("resetAll", ComposeOp::Keep, Alignment::Reset),
        ("restart", ComposeOp::KeepIf, Alignment::Restart),
        ("restartAll", ComposeOp::Keep, Alignment::Restart),
    ];
    // ...and the bare alignment names, which default their operator to
    // `set`: `pat.squeeze(x)` is `pat.set.squeeze(x)`.
    let bare: Vec<(&str, ComposeOp, Alignment)> = Alignment::ALL
        .iter()
        .map(|how| (how.name(), ComposeOp::Set, *how))
        .collect();

    // Order matters: the bare alignment methods go
    // FIRST and the named shortcuts after, so `reset`/`restart`
    // (which are both alignment names and keepif shortcuts) resolve
    // to the keepif forms.
    for (name, op, how) in bare.into_iter().chain(shortcuts) {
        let f = Function::new(
            ctx.clone(),
            hr_this_rest(move |ctx, this, args| {
                if args.0.is_empty() {
                    return Err(refuse_empty_call(&ctx, &format!(".{name}"), 1));
                }
                let (receiver, mut sidecars) = {
                    let borrowed = this.0.borrow();
                    (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
                };
                let (other, nested) = sequence_args_bridged(&ctx, &args.0)?;
                sidecars.extend(nested);
                let pattern = rustel_core::compose::compose(&receiver, &other, op, how);
                derive_wrapper(ctx, pattern, &sidecars)
            }),
        )
        .map_err(|e| e.to_string())?;
        proto.set(name, f).map_err(|e| e.to_string())?;
    }

    // -- prototype methods that are not `register()`ed --------------
    //
    // These take PATTERNS or a variadic list of transformers rather
    // than the reified scalar arguments `register()` folds, so they
    // cannot go through the registry: its non-patternified path
    // collapses each argument to `__pure`, which would discard the
    // pattern entirely.

    // ```js
    // export const bite = register('bite', (npat, ipat, pat) => …, false);
    // ```
    let bite = Function::new(
        ctx.clone(),
        hr_this_rest(move |ctx, this, args| {
            let (receiver, mut sidecars) = {
                let borrowed = this.0.borrow();
                (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
            };
            if args.0.is_empty() {
                return Err(refuse_empty_call(&ctx, ".bite", 2));
            }
            let npat = if let Some(value) = args.0.first() {
                let (pattern, sidecar) = reify_bridged(&ctx, value)?;
                sidecars.push(sidecar);
                pattern
            } else {
                rustel_core::silence()
            };
            let ipat = if let Some(value) = args.0.get(1) {
                let (pattern, sidecar) = reify_bridged(&ctx, value)?;
                sidecars.push(sidecar);
                pattern
            } else {
                rustel_core::silence()
            };
            let pattern = rustel_core::combinators::bite(&receiver, &npat, &ipat);
            derive_wrapper(ctx, pattern, &sidecars)
        }),
    )
    .map_err(|e| e.to_string())?;
    proto.set("bite", bite).map_err(|e| e.to_string())?;

    // ```js
    // export const arpWith = register('arpWith', (func, pat) =>
    //   pat.collect().fmap((v) => reify(func(v))).innerJoin()
    //      .withHap((h) => new Hap(h.whole, h.part,
    //                             h.value.value, h.combineContext(h.value))));
    // ```
    //
    // The callback receives `Hap[]`, not a scalar `Value`, so this is
    // a host route rather than a registry body. The one-function fast
    // path is direct; the general register path accepts a callback
    // pattern, including the sequence made by `.arpWith(f, g)`.
    let arp_with = Function::new(
        ctx.clone(),
        hr_this_rest(move |ctx, this, args| {
            let (receiver, mut sidecars) = {
                let borrowed = this.0.borrow();
                (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
            };
            if args.0.is_empty() {
                return Err(refuse_empty_call(&ctx, ".arpWith", 1));
            }
            if let [only] = args.0.as_slice()
                && let Some(callable) = only.as_function()
            {
                let (id, sidecar) = bridge_callable(&ctx, callable.clone())?;
                sidecars.push(sidecar);
                return derive_wrapper(ctx, rustel_core::arp_with(receiver, id), &sidecars);
            }

            let (callback_parts, callback_sidecars) = reify_args(&ctx, &args.0)?;
            sidecars.extend(callback_sidecars);
            let callbacks = match callback_parts.len() {
                0 => rustel_core::silence(),
                1 => callback_parts.into_iter().next().unwrap(),
                _ => rustel_core::fastcat(callback_parts),
            };
            derive_wrapper(
                ctx,
                rustel_core::arp_with_pattern(receiver, callbacks),
                &sidecars,
            )
        }),
    )
    .map_err(|e| e.to_string())?;
    proto.set("arpWith", arp_with).map_err(|e| e.to_string())?;

    // Raw `_arpWith` does NOT patternify or sequence its arguments.
    // With one argument the receiver is the trailing `this`; with two
    // or more, the second positional argument becomes `pat` and the
    // receiver is ignored, exactly as `func(...args, this)` does.
    let raw_arp_with_method = Function::new(
        ctx.clone(),
        hr_this_rest(move |ctx, this, args| {
            let (receiver, mut sidecars) = if args.0.len() == 1 {
                let borrowed = this.0.borrow();
                (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
            } else {
                let Some(raw_receiver) = args.0.get(1) else {
                    return Err(throw_type_error(
                        &ctx,
                        "Cannot read properties of undefined (reading 'collect')",
                    ));
                };
                if raw_receiver.is_undefined() || raw_receiver.is_null() {
                    let kind = if raw_receiver.is_null() {
                        "null"
                    } else {
                        "undefined"
                    };
                    return Err(throw_type_error(
                        &ctx,
                        &format!("Cannot read properties of {kind} (reading 'collect')"),
                    ));
                }
                let Some((pattern, sidecar)) = unwrap_pattern(raw_receiver) else {
                    return Err(throw_type_error(&ctx, "pat.collect is not a function"));
                };
                (pattern, vec![sidecar])
            };
            let Some(callback) = args.0.first().and_then(rquickjs::Value::as_function) else {
                return derive_wrapper(
                    ctx,
                    rustel_core::query_error_pattern("func is not a function"),
                    &sidecars,
                );
            };
            let (id, sidecar) = bridge_callable(&ctx, callback.clone())?;
            sidecars.push(sidecar);
            derive_wrapper(ctx, rustel_core::arp_with(receiver, id), &sidecars)
        }),
    )
    .map_err(|e| e.to_string())?;
    proto
        .set("_arpWith", raw_arp_with_method)
        .map_err(|e| e.to_string())?;

    // The exported form is `curry(pfunc, null, 2)`: accept both
    // `arpWith(f, pat)` and `arpWith(f)(pat)`. It goes through a raw
    // host function rather than calling `pat.arpWith` in JavaScript,
    // because pfunc REIFIES the trailing receiver too.
    let raw_arp_with = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let receiver_value = args.0.get(1);
            let (receiver, mut sidecars) = if let Some(value) = receiver_value {
                let (pattern, sidecar) = reify_bridged(&ctx, value)?;
                (pattern, vec![sidecar])
            } else {
                (rustel_core::silence(), Vec::new())
            };
            let callback_value = args
                .0
                .first()
                .cloned()
                .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
            if let Some(callback) = callback_value.as_function() {
                let (id, sidecar) = bridge_callable(&ctx, callback.clone())?;
                sidecars.push(sidecar);
                derive_wrapper(ctx, rustel_core::arp_with(receiver, id), &sidecars)
            } else {
                let (callbacks, sidecar) = reify_bridged(&ctx, &callback_value)?;
                sidecars.push(sidecar);
                derive_wrapper(
                    ctx,
                    rustel_core::arp_with_pattern(receiver, callbacks),
                    &sidecars,
                )
            }
        }),
    )
    .map_err(|e| e.to_string())?;
    let free_arp_with = native_named_curry(&ctx, raw_arp_with, 2, "curried", "partial", true)
        .map_err(|error| error.to_string())?;
    globals
        .set("arpWith", free_arp_with)
        .map_err(|e| e.to_string())?;

    // ```js
    // export const arp = register('arp',
    //   (indices, pat) => pat.arpWith((haps) => reify(indices).fmap((i) => haps[_mod(i, haps.length)])),
    //   false);
    // ```
    // Installed here rather than through the registry because the
    // argument is a PATTERN, and `register()`'s bodies take scalar
    // `Value`s - the same reason `bite` and `superimpose` live here.
    // The `false` is `patternify`, so the indices stay one pattern
    // instead of being lifted hap-by-hap.
    let arp = Function::new(
        ctx.clone(),
        hr_this_rest(move |ctx, this, args| {
            let (receiver, mut sidecars) = {
                let borrowed = this.0.borrow();
                (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
            };
            // A registered one-input method sequences zero or multiple
            // arguments as well as accepting one pattern unchanged - but a
            // sequence of nothing is silence, so no argument is refused.
            if args.0.is_empty() {
                return Err(refuse_empty_call(&ctx, ".arp", 1));
            }
            let (indices, nested) = if args.0.len() == 1 {
                let (pattern, sidecar) = reify_bridged(&ctx, &args.0[0])?;
                (pattern, vec![sidecar])
            } else {
                sequence_args_bridged(&ctx, &args.0)?
            };
            sidecars.extend(nested);
            derive_wrapper(ctx, rustel_core::arp(receiver, indices), &sidecars)
        }),
    )
    .map_err(|e| e.to_string())?;
    proto.set("arp", arp).map_err(|e| e.to_string())?;

    let raw_arp_method = Function::new(
        ctx.clone(),
        hr_this_rest(move |ctx, this, args| {
            let Some(indices_value) = args.0.first() else {
                return Err(throw_type_error(
                    &ctx,
                    "Cannot read properties of undefined (reading 'arpWith')",
                ));
            };
            let (receiver, mut sidecars) = if args.0.len() == 1 {
                let borrowed = this.0.borrow();
                (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
            } else {
                let raw_receiver = &args.0[1];
                if raw_receiver.is_undefined() || raw_receiver.is_null() {
                    let kind = if raw_receiver.is_null() {
                        "null"
                    } else {
                        "undefined"
                    };
                    return Err(throw_type_error(
                        &ctx,
                        &format!("Cannot read properties of {kind} (reading 'arpWith')"),
                    ));
                }
                let Some((pattern, sidecar)) = unwrap_pattern(raw_receiver) else {
                    return Err(throw_type_error(&ctx, "pat.arpWith is not a function"));
                };
                (pattern, vec![sidecar])
            };
            // Raw `_arp` bypasses register's pfunc. Its body calls
            // lexical `reify(indices)` from inside the `arpWith`
            // callback, so parser choice and failure happen at query
            // time rather than while this graph is constructed.
            let stack = host_stack(&ctx)?;
            let selector_factory: Function = stack.as_object().get(RAW_ARP_SELECTOR_FACTORY)?;
            let selector: Function = selector_factory.call((indices_value.clone(),))?;
            let (id, sidecar) = bridge_callable(&ctx, selector)?;
            sidecars.push(sidecar);
            derive_wrapper(ctx, rustel_core::arp_with(receiver, id), &sidecars)
        }),
    )
    .map_err(|e| e.to_string())?;
    proto
        .set("_arp", raw_arp_method)
        .map_err(|e| e.to_string())?;

    let raw_arp_free = Function::new(
        ctx.clone(),
        hr_rest(move |ctx, args| {
            let indices_value = args
                .0
                .first()
                .cloned()
                .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
            let receiver_value = args.0.get(1);
            let (indices, indices_sidecar) = reify_bridged(&ctx, &indices_value)?;
            let (receiver, receiver_sidecar) = if let Some(value) = receiver_value {
                reify_bridged(&ctx, value)?
            } else {
                (rustel_core::silence(), Sidecar::default())
            };
            let sidecars = [indices_sidecar, receiver_sidecar];
            derive_wrapper(ctx, rustel_core::arp(receiver, indices), &sidecars)
        }),
    )
    .map_err(|e| e.to_string())?;
    let free_arp = native_named_curry(&ctx, raw_arp_free, 2, "curried", "partial", true)
        .map_err(|error| error.to_string())?;
    globals.set("arp", free_arp).map_err(|e| e.to_string())?;

    // ```js
    // superimpose(...funcs) { return stack(this, ...funcs.map((f) => f(this))); }
    // layer(...funcs)       { return stack(...funcs.map((f) => f(this))); }
    // ```
    for (name, keep_original) in [("superimpose", true), ("layer", false)] {
        let f = Function::new(
            ctx.clone(),
            hr_this_rest(move |ctx, this, args| {
                if args.0.is_empty() {
                    return Err(refuse_empty_call(&ctx, &format!(".{name}"), 1));
                }
                let (receiver, mut sidecars) = {
                    let borrowed = this.0.borrow();
                    (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
                };
                let mut funcs = Vec::with_capacity(args.0.len());
                for arg in &args.0 {
                    let (pattern, sidecar) = reify_bridged(&ctx, arg)?;
                    let function = pattern.as_pure().and_then(|v| match v {
                        Value::Function(f) => Some(f),
                        _ => None,
                    });
                    match function {
                        Some(f) => funcs.push(f),
                        None => {
                            return Err(rquickjs::Error::new_from_js_message(
                                "value",
                                "Pattern",
                                format!("{name}: expects pattern transformers"),
                            ));
                        }
                    }
                    sidecars.push(sidecar);
                }
                let pattern = if keep_original {
                    rustel_core::combinators::superimpose(&receiver, &funcs)
                } else {
                    rustel_core::combinators::layer(&receiver, &funcs)
                };
                // `funcs.map((f) => f(this))` runs every transformer right
                // here, so a throw belongs to this call, as it does for every
                // registered method. `map` stops at it, and so does the host:
                // once an eager throw is held, a later transformer's callback
                // is not run at all (`refuse_eager_call_behind_a_held_throw`).
                rethrow_eager_callback_exception(&ctx)?;
                derive_wrapper(ctx, pattern, &sidecars)
            }),
        )
        .map_err(|e| e.to_string())?;
        proto.set(name, f).map_err(|e| e.to_string())?;
    }

    // -- the direct join surface ------------------------------------
    //
    // The exact shape:
    //   Pattern.prototype.polyJoin  (length 0)   - no global
    //   Pattern.prototype.stepJoin  (length 0)   - no global
    //   Pattern.prototype.polyBind  (length 1)   + curried free fn
    //   Pattern.prototype.stepBind  (length 1)   + curried free fn
    //
    // `polyJoin`/`stepJoin` need the internal pattern-valued
    // representation (`pure(pattern)`) and never call JavaScript.
    // `stepJoin` alone: `polyJoin` is a class FIELD, so it is
    // installed per instance by `new_wrapper` rather than here.
    {
        let name = "stepJoin";
        let f = Function::new(
            ctx.clone(),
            hr_this_rest(move |ctx, this, _args| {
                let (receiver, sidecars) = {
                    let borrowed = this.0.borrow();
                    (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
                };
                // `stepJoin` slices cycle zero eagerly and throws right
                // there on a non-pattern-valued receiver;
                // `polyJoin`'s `fmap` is lazy, so its equivalent
                // failure is deferred to the query.
                let pattern = receiver
                    .try_step_join()
                    .map_err(|message| throw_type_error(&ctx, &message))?;
                derive_wrapper(ctx, pattern, &sidecars)
            }),
        )
        .map_err(|e| e.to_string())?;
        proto.set(name, f).map_err(|e| e.to_string())?;
    }

    // `polyBind`/`stepBind` bridge the callback into a trace-managed
    // cell and reuse the same core joins. The callback is ALWAYS
    // bridged, even for a tagged installed function: this path needs a
    // value->pattern callable, which a native `FunctionRef` is not.
    for (name, step) in [("polyBind", false), ("stepBind", true)] {
        let f = Function::new(
            ctx.clone(),
            hr_this_arg(move |ctx, this, arg| {
                let (receiver, mut sidecars) = {
                    let borrowed = this.0.borrow();
                    (borrowed.pattern.clone(), vec![Sidecar::of(&borrowed)])
                };
                // A non-function argument is NOT a construction error:
                // `fmap(42)` builds fine and only fails when
                // the query calls it. Throwing here would refuse an
                // expression Strudel accepts.
                let Some(callable) = arg.0.as_ref().and_then(|v| v.as_function()) else {
                    return derive_wrapper(
                        ctx,
                        rustel_core::query_error_pattern("func is not a function"),
                        &sidecars,
                    );
                };
                let (id, sidecar) = bridge_callable(&ctx, callable.clone())?;
                sidecars.push(sidecar);
                // `stepBind` invokes the callback during CONSTRUCTION
                // and may hand it the receiver's graph, which the
                // callback can keep.
                publish_owner_cells(&sidecars)?;
                // `stepJoin` computes `first_t` during CONSTRUCTION by
                // querying cycle zero, so this invokes the callback while
                // the evaluation frame and callback host are still open -
                // and again on later queries. That probe is a query (see
                // `rustel_core::query_in_progress`): a callback's throw there
                // is absorbed by it. The construction pass is also where the
                // `x.value.withHap is not a function` throw comes from.
                let pattern = if step {
                    receiver
                        .try_step_bind_js(id)
                        .map_err(|message| throw_type_error(&ctx, &message))?
                } else {
                    receiver.poly_bind_js(id)
                };
                derive_wrapper(ctx, pattern, &sidecars)
            }),
        )
        .map_err(|e| e.to_string())?;
        set_function_length(&f, 1).map_err(|e| e.to_string())?;
        proto.set(name, f).map_err(|e| e.to_string())?;
    }

    for name in ["polyBind", "stepBind"] {
        let raw = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  args: rquickjs::function::Rest<rquickjs::Value<'js>>|
                  -> rquickjs::Result<rquickjs::Value<'js>> {
                let function = args
                    .0
                    .first()
                    .cloned()
                    .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
                let receiver = args
                    .0
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
                if receiver.is_null() || receiver.is_undefined() {
                    let kind = if receiver.is_null() {
                        "null"
                    } else {
                        "undefined"
                    };
                    return Err(throw_type_error(
                        &ctx,
                        &format!("Cannot read properties of {kind} (reading '{name}')"),
                    ));
                }
                let object = if let Some(object) = receiver.as_object() {
                    object.clone()
                } else {
                    let constructor: Function = ctx.globals().get("Object")?;
                    constructor.call((receiver.clone(),))?
                };
                let method: Function = object.get(name)?;
                let mut call = Args::new(ctx.clone(), 1);
                call.this(receiver)?;
                call.push_arg(function)?;
                call.apply(&method)
            },
        )
        .map_err(|error| error.to_string())?;
        let curried =
            native_named_curry(&ctx, raw, 2, "f", "f", false).map_err(|error| error.to_string())?;
        globals.set(name, curried).map_err(|e| e.to_string())?;
    }

    // User setup/prebake runs after this installation and must extend

    Ok(())
}
