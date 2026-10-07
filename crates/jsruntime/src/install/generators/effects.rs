use super::*;
use rquickjs::function::{Rest, This};
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

/// Reference entries for sample setup calls.
pub(crate) const REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "samples",
        synonyms: &[],
        summary: "register sample banks for the score",
        description: "Registers named sample banks. Pass a map of bank names to sample URLs, with an optional base URL for relative paths, or a source string such as 'local:', an allowed HTTP origin, or 'github:user/repo'. Local paths must be under the folder granted with --allow-local-samples. Custom HTTP origins can be allowed with --allow-sample-origin. Imported names take precedence over the built-in banks and General MIDI sounds. Studio's samples reference lists imported banks and fetches source manifests during background checks.",
        params: &[
            ReferenceParam {
                name: "map",
                r#type: "Object | string",
                description: "bank names mapped to sample URLs, or a source string.",
            },
            ReferenceParam {
                name: "base",
                r#type: "string",
                description: "base URL the map's relative paths resolve against.",
            },
        ],
        examples: &[
            "samples({ rhodes: 'https://cdn.freesound.org/previews/132/132051_316502-lq.mp3' })\ns(\"rhodes\")",
        ],
        tags: &["samples", "rustel"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "preload",
        synonyms: &[],
        summary: "fetch and decode sounds before playback",
        description: "Requests sample fetching and decoding before the sounds are played. Accepts space-separated names such as 'bd sd hh:4', an array, or a pattern. Place it with the score's setup calls. Studio's Settings ▸ Samples can preload whole sample sets.",
        params: &[ReferenceParam {
            name: "names",
            r#type: "string | Array | Pattern",
            description: "sample names, optionally with an index such as hh:4.",
        }],
        examples: &["preload(\"bd hh:4\")"],
        tags: &["samples", "rustel"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
];

const MAX_PRELOAD_VALUES: usize = MAX_PRELOAD_EFFECT_BYTES;
const MAX_PRELOAD_DEPTH: usize = 256;

fn hr_effect<F>(f: F) -> F
where
    F: for<'js> Fn(Ctx<'js>, Rest<rquickjs::Value<'js>>) -> rquickjs::Result<rquickjs::Value<'js>>,
{
    f
}

fn argument<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    index: usize,
) -> rquickjs::Value<'js> {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()))
}

fn resolved<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let promise: rquickjs::Object = ctx.globals().get("Promise")?;
    let resolve: Function = promise.get("resolve")?;
    resolve.call((This(promise),))
}

fn string_value<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> rquickjs::Result<String> {
    let string: Function = ctx.globals().get("String")?;
    let value: rquickjs::String = string.call((value,))?;
    value.to_string()
}

fn stage_samples<'js>(
    ctx: &Ctx<'js>,
    args: &[rquickjs::Value<'js>],
    policy: &Cell<EffectPolicy>,
    transaction: &RefCell<Option<ScoreEffects>>,
    refusal: &RefCell<Option<String>>,
) -> rquickjs::Result<()> {
    let map = argument(ctx, args, 0);
    let map = if map.is_undefined() {
        rquickjs::Value::new_null(ctx.clone())
    } else {
        map
    };
    let json: rquickjs::Object = ctx.globals().get("JSON")?;
    let stringify: Function = json.get("stringify")?;
    let map: rquickjs::String = stringify.call((This(json), map))?;
    let map = map.to_cstring()?;

    let base = argument(ctx, args, 1);
    let base = if base.is_null() || base.is_undefined() {
        None
    } else {
        Some(string_value(ctx, base)?)
    };

    stage_effect(
        ctx,
        policy,
        EffectPolicy::SAMPLES,
        transaction,
        refusal,
        SAMPLES_SCOPE_POLICY,
        move |ctx, effects| {
            let added = map
                .len()
                .saturating_add(base.as_ref().map_or(0, String::len));
            let used = effects.samples.iter().fold(0usize, |used, (map, base)| {
                used.saturating_add(map.len())
                    .saturating_add(base.as_ref().map_or(0, String::len))
            });
            if effects.samples.len() >= MAX_SCORE_SAMPLE_EFFECTS
                || used.saturating_add(added) > MAX_SCORE_SAMPLE_EFFECT_BYTES
            {
                return Err(rquickjs::Exception::throw_range(
                    ctx,
                    "setup/score samples() registrations exceed the host effect limit",
                ));
            }
            effects
                .samples
                .try_reserve(1)
                .map_err(|_| rquickjs::Error::Allocation)?;
            effects.samples.push((map.as_str().to_owned(), base));
            Ok(())
        },
    )
}

struct PreloadCollector {
    pieces: Vec<String>,
    bytes: usize,
    visited: usize,
}

impl PreloadCollector {
    fn new() -> Self {
        Self {
            pieces: Vec::new(),
            bytes: 0,
            visited: 0,
        }
    }

    fn visit(&mut self, ctx: &Ctx<'_>, depth: usize) -> rquickjs::Result<()> {
        if depth > MAX_PRELOAD_DEPTH || self.visited == MAX_PRELOAD_VALUES {
            return Err(rquickjs::Exception::throw_range(
                ctx,
                "preload() arguments exceed the host traversal limit",
            ));
        }
        self.visited += 1;
        Ok(())
    }

    fn push(&mut self, ctx: &Ctx<'_>, value: String) -> rquickjs::Result<()> {
        let added = value.len() + usize::from(!self.pieces.is_empty());
        if self.bytes.saturating_add(added) > MAX_PRELOAD_EFFECT_BYTES {
            return Err(rquickjs::Exception::throw_range(
                ctx,
                "preload() arguments exceed the host effect limit",
            ));
        }
        self.pieces
            .try_reserve(1)
            .map_err(|_| rquickjs::Error::Allocation)?;
        self.bytes += added;
        self.pieces.push(value);
        Ok(())
    }

    fn collect_array<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        value: &rquickjs::Value<'js>,
        depth: usize,
    ) -> rquickjs::Result<()> {
        let object = value
            .as_object()
            .ok_or_else(|| rquickjs::Error::new_from_js("value", "Array"))?;
        let length: usize = object.get("length")?;
        if length > MAX_PRELOAD_VALUES {
            return Err(rquickjs::Exception::throw_range(
                ctx,
                "preload() array exceeds the host traversal limit",
            ));
        }
        if length == 2 {
            let first: rquickjs::Value = object.get(0_u32)?;
            let second: rquickjs::Value = object.get(1_u32)?;
            if first.is_string() && second.is_number() {
                let first: rquickjs::Value = object.get(0_u32)?;
                let second: rquickjs::Value = object.get(1_u32)?;
                let value = format!(
                    "{}:{}",
                    string_value(ctx, first)?,
                    string_value(ctx, second)?
                );
                return self.push(ctx, value);
            }
        }
        for index in 0..length {
            let index = index as u32;
            if object.contains_key(index)? {
                let item: rquickjs::Value = object.get(index)?;
                self.collect(ctx, item, depth + 1)?;
            }
        }
        Ok(())
    }

    fn collect_query<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        result: rquickjs::Value<'js>,
        depth: usize,
    ) -> rquickjs::Result<()> {
        if !is_array(ctx, &result)? {
            return Err(rquickjs::Error::new_from_js("value", "Array"));
        }
        let array = result
            .as_object()
            .ok_or_else(|| rquickjs::Error::new_from_js("value", "Array"))?;
        let length: usize = array.get("length")?;
        if length > MAX_PRELOAD_VALUES {
            return Err(rquickjs::Exception::throw_range(
                ctx,
                "preload() query result exceeds the host traversal limit",
            ));
        }
        for index in 0..length {
            let index = index as u32;
            if !array.contains_key(index)? {
                continue;
            }
            let hap: rquickjs::Object = array.get(index)?;
            let value: rquickjs::Value = hap.get("value")?;
            self.collect(ctx, value, depth + 1)?;
        }
        Ok(())
    }

    fn collect_object<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        object: &rquickjs::Object<'js>,
        depth: usize,
    ) -> rquickjs::Result<()> {
        let query: rquickjs::Value = object.get("queryArc")?;
        if query.is_function() {
            let result = (|| {
                let query: Function = object.get("queryArc")?;
                let result: rquickjs::Value = query.call((This(object.clone()), 0.0, 1.0))?;
                self.collect_query(ctx, result, depth)
            })();
            if let Err(error) = result
                && matches!(error, rquickjs::Error::Exception)
            {
                let _ = ctx.catch();
            }
            return Ok(());
        }

        let sound: rquickjs::Value = object.get("s")?;
        if sound.is_null() || sound.is_undefined() {
            return Ok(());
        }
        let number: rquickjs::Value = object.get("n")?;
        let sound: rquickjs::Value = object.get("s")?;
        let mut value = string_value(ctx, sound)?;
        if !number.is_null() && !number.is_undefined() {
            let number: rquickjs::Value = object.get("n")?;
            value.push(':');
            value.push_str(&string_value(ctx, number)?);
        }
        self.push(ctx, value)
    }

    fn collect<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        value: rquickjs::Value<'js>,
        depth: usize,
    ) -> rquickjs::Result<()> {
        self.visit(ctx, depth)?;
        if value.is_null() || value.is_undefined() {
            return Ok(());
        }
        if is_array(ctx, &value)? {
            return self.collect_array(ctx, &value, depth);
        }
        if let Some(object) = value.as_object() {
            return self.collect_object(ctx, object, depth);
        }
        self.push(ctx, string_value(ctx, value)?)
    }

    fn finish(self) -> String {
        self.pieces.join(" ")
    }
}

fn stage_preload<'js>(
    ctx: &Ctx<'js>,
    args: Vec<rquickjs::Value<'js>>,
    policy: &Cell<EffectPolicy>,
    transaction: &RefCell<Option<ScoreEffects>>,
    refusal: &RefCell<Option<String>>,
) -> rquickjs::Result<()> {
    let outer = rquickjs::Array::new(ctx.clone())?;
    for (index, value) in args.into_iter().enumerate() {
        outer.set(index, value)?;
    }
    let mut collector = PreloadCollector::new();
    collector.collect(ctx, outer.into_value(), 0)?;
    let names = collector.finish();
    if names.trim().is_empty() {
        return Ok(());
    }

    stage_effect(
        ctx,
        policy,
        EffectPolicy::PRELOAD,
        transaction,
        refusal,
        PRELOAD_SCOPE_POLICY,
        move |ctx, effects| {
            let used = effects
                .preload
                .iter()
                .fold(0usize, |used, names| used.saturating_add(names.len()));
            if effects.preload.len() >= MAX_PRELOAD_EFFECTS
                || used.saturating_add(names.len()) > MAX_PRELOAD_EFFECT_BYTES
            {
                return Err(rquickjs::Exception::throw_range(
                    ctx,
                    "preload() registrations exceed the host effect limit",
                ));
            }
            effects
                .preload
                .try_reserve(1)
                .map_err(|_| rquickjs::Error::Allocation)?;
            effects.preload.push(names);
            Ok(())
        },
    )
}

pub(super) fn install<'js>(runtime: &JsRuntime, ctx: &Ctx<'js>) -> Result<(), String> {
    let globals = ctx.globals();
    let samples_transaction = runtime.effect_transaction.clone();
    let samples_policy = runtime.effect_policy.clone();
    let samples_refusal = runtime.effect_policy_refusal.clone();
    let samples = Function::new(
        ctx.clone(),
        hr_effect(move |ctx, args| {
            stage_samples(
                &ctx,
                &args.0,
                &samples_policy,
                &samples_transaction,
                &samples_refusal,
            )?;
            resolved(&ctx)
        }),
    )
    .map_err(|error| error.to_string())?;
    configure_function(&samples, "", 2, false).map_err(|error| error.to_string())?;
    globals
        .set("samples", samples)
        .map_err(|error| error.to_string())?;

    let preload_transaction = runtime.effect_transaction.clone();
    let preload_policy = runtime.effect_policy.clone();
    let preload_refusal = runtime.effect_policy_refusal.clone();
    let preload = Function::new(
        ctx.clone(),
        hr_effect(move |ctx, args| {
            stage_preload(
                &ctx,
                args.0,
                &preload_policy,
                &preload_transaction,
                &preload_refusal,
            )?;
            resolved(&ctx)
        }),
    )
    .map_err(|error| error.to_string())?;
    configure_function(&preload, "", 0, false).map_err(|error| error.to_string())?;
    globals
        .set("preload", preload)
        .map_err(|error| error.to_string())?;

    Ok(())
}
