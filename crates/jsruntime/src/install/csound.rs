use super::*;
use rquickjs::function::Rest;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

const MAX_SOURCES: usize = 64;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const SCOPE_POLICY: &str = "loadCSound/loadcsound/loadCsound and loadOrc can load orchestras only during a Session-owned score or setup evaluation, not from raw code or query-time callbacks";

pub(crate) const REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "loadCSound",
        synonyms: &["loadcsound", "loadCsound"],
        summary: "compile Csound orchestra code",
        description: "Compiles orchestra code before the score starts. Requires double-precision Csound 6.18 or a later 6.x release and the Csound permission: --allow-csound on the command line, on by default in the studio. Accepts a string or a tagged template with substitutions. Call without code to initialize the built-in instruments. Use this during score or setup evaluation. A compile error leaves the previous score playing.",
        params: &[ReferenceParam {
            name: "code",
            r#type: "string",
            description: "Csound orchestra source. Omit to initialize the built-in instruments.",
        }],
        examples: &[
            "loadCSound`instr Bell\na1 oscili p5, p4\nouts a1, a1\nendin`\nnote(\"c4 e4 g4\").csound('Bell')",
        ],
        tags: &["csound", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "loadOrc",
        synonyms: &[],
        summary: "load a Csound orchestra from a URL or file",
        description: "Loads and compiles an orchestra before the score starts. Requires double-precision Csound 6.18 or a later 6.x release and the Csound permission: --allow-csound on the command line, on by default in the studio. Accepts a trusted HTTPS URL, github:owner/repository/revision/file.orc, or a local path within the host's orchestra grant. Pin remote revisions to a commit for reproducible scores. Use this during score or setup evaluation.",
        params: &[ReferenceParam {
            name: "url",
            r#type: "string",
            description: "Orchestra URL or local path.",
        }],
        examples: &[],
        tags: &["csound", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
];

fn hr_effect<F>(f: F) -> F
where
    F: for<'js> Fn(Ctx<'js>, Rest<rquickjs::Value<'js>>) -> rquickjs::Result<rquickjs::Value<'js>>,
{
    f
}

fn source_limit(ctx: &Ctx<'_>) -> rquickjs::Error {
    rquickjs::Exception::throw_range(
        ctx,
        "Csound orchestra requests exceed the host effect limit",
    )
}

fn source_text<'js>(ctx: &Ctx<'js>, args: &[rquickjs::Value<'js>]) -> rquickjs::Result<String> {
    let Some(first) = args.first() else {
        return Ok(String::new());
    };
    if let Some(parts) = first.as_array()
        && parts.as_object().contains_key("raw")?
    {
        if parts.len() != args.len() {
            return Err(throw_type_error(
                ctx,
                "loadCSound template substitutions do not match",
            ));
        }
        let mut source = String::new();
        for (index, argument) in args.iter().enumerate() {
            let part: String = parts.get(index)?;
            let substitution = if index == 0 {
                String::new()
            } else {
                rquickjs::Coerced::<String>::from_js(ctx, argument.clone())?.0
            };
            let added = part.len().saturating_add(substitution.len());
            if source.len().saturating_add(added) > MAX_SOURCE_BYTES {
                return Err(source_limit(ctx));
            }
            source
                .try_reserve(added)
                .map_err(|_| rquickjs::Error::Allocation)?;
            source.push_str(&substitution);
            source.push_str(&part);
        }
        return Ok(source);
    }
    if first.is_undefined() || first.is_null() {
        return Ok(String::new());
    }
    let Some(source) = first.as_string() else {
        return Err(throw_type_error(
            ctx,
            "loadCSound expects orchestra code as a string or a tagged template",
        ));
    };
    source.to_string()
}

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    for (name, remote) in [("loadCSound", false), ("loadOrc", true)] {
        let policy = runtime.effect_policy.clone();
        let transaction = runtime.effect_transaction.clone();
        let refusal = runtime.effect_policy_refusal.clone();
        let function = Function::new(
            ctx.clone(),
            hr_effect(move |ctx, args| {
                if !policy.get().allows(EffectPolicy::CSOUND) {
                    return Err(throw_effect_policy(&ctx, &refusal, SCOPE_POLICY));
                }
                let source = if remote {
                    let Some(url) = args.0.first().and_then(rquickjs::Value::as_string) else {
                        return Err(throw_type_error(
                            &ctx,
                            "loadOrc expects a URL or local path as a string",
                        ));
                    };
                    CsoundSource::Url(url.to_string()?)
                } else {
                    CsoundSource::Code(source_text(&ctx, &args.0)?)
                };
                stage_effect(
                    &ctx,
                    &policy,
                    EffectPolicy::CSOUND,
                    &transaction,
                    &refusal,
                    SCOPE_POLICY,
                    |ctx, effects| {
                        let bytes = |source: &CsoundSource| match source {
                            CsoundSource::Code(source) | CsoundSource::Url(source) => source.len(),
                        };
                        let used = effects
                            .csound
                            .iter()
                            .fold(0usize, |used, source| used.saturating_add(bytes(source)));
                        if effects.csound.len() >= MAX_SOURCES
                            || used.saturating_add(bytes(&source)) > MAX_SOURCE_BYTES
                        {
                            return Err(source_limit(ctx));
                        }
                        effects
                            .csound
                            .try_reserve(1)
                            .map_err(|_| rquickjs::Error::Allocation)?;
                        effects.csound.push(source);
                        Ok(())
                    },
                )?;
                let (promise, resolve, _) = rquickjs::Promise::new(&ctx)?;
                resolve.call::<_, ()>(())?;
                Ok(promise.into_value())
            }),
        )
        .map_err(|error| error.to_string())?;
        configure_function(&function, name, 1, false).map_err(|error| error.to_string())?;
        globals
            .set(name, function.clone())
            .map_err(|error| error.to_string())?;
        if !remote {
            for alias in ["loadcsound", "loadCsound"] {
                globals
                    .set(alias, function.clone())
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}
