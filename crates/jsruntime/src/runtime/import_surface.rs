//! The names that score imports define, and the meaning of each name a score
//! sees.
//!
//! Two scripts can give one name two meanings. A score gets the meaning from
//! the imports in its own text. A name with one meaning stays visible to every
//! score, because a playing score reads some names at query time.
//!
//! ```text
//!              score imports a.js   score imports b.js   no import
//! sc (a, b)    from a.js            from b.js            no value
//! only (b)     from b.js            from b.js            from b.js
//! ```

use super::*;
use std::collections::HashMap;

type Saved = rquickjs::Persistent<rquickjs::Value<'static>>;

fn save<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> Saved {
    rquickjs::Persistent::save(ctx, value)
}

/// Who sees the names an import defines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportScope {
    /// Every score. A prebake names the import.
    Shared,
    /// The scores with the import in their text.
    Score,
}

/// The own data values of each surface object, by name. `None` marks an
/// accessor.
pub(super) type Snapshot<'js> = Vec<HashMap<String, Option<rquickjs::Value<'js>>>>;

#[derive(Default)]
pub(crate) struct ImportSurface {
    /// The imports of scores, in load order. An index names an import.
    imports: Vec<ScoreImport>,
    /// The imports with their meanings on the surface, in text order.
    shown: Vec<usize>,
    /// `shown` at the last score install.
    last_good: Vec<usize>,
    names: Vec<TrackedName>,
}

struct ScoreImport {
    specifier: String,
    /// A prebake named the import after a score loaded it.
    shared: bool,
}

/// One name on one surface object, set by one import or more.
struct TrackedName {
    /// Index into [`score_surface_objects`].
    object: usize,
    name: String,
    /// The value before the first import set the name, if the name had one.
    base: Option<Saved>,
    /// Each import with the value its script set.
    meanings: Vec<(usize, Saved)>,
    /// The meaning on the surface: an import, or `None` for the base.
    showing: Option<usize>,
}

impl TrackedName {
    /// The meaning for a score with the imports `listed` in its text.
    fn winner(&self, listed: &[usize], imports: &[ScoreImport]) -> Option<usize> {
        let sets = |id: usize| self.meanings.iter().any(|(owner, _)| *owner == id);
        if let Some(id) = listed.iter().rev().find(|id| sets(**id)) {
            return Some(*id);
        }
        if let Some((id, _)) = self
            .meanings
            .iter()
            .rev()
            .find(|(id, _)| imports[*id].shared)
        {
            return Some(*id);
        }
        // One meaning and no base: no other value answers to the name.
        match (&self.base, self.meanings.as_slice()) {
            (None, [(id, _)]) => Some(*id),
            _ => None,
        }
    }
}

impl ImportSurface {
    /// The imports of `specifiers` with tracked names, in the same order.
    pub(super) fn ids(&self, specifiers: &[String]) -> Vec<usize> {
        specifiers
            .iter()
            .filter_map(|specifier| self.position(specifier))
            .collect()
    }

    fn position(&self, specifier: &str) -> Option<usize> {
        self.imports
            .iter()
            .position(|import| import.specifier == specifier)
    }

    pub(super) fn shows(&self, listed: &[usize]) -> bool {
        self.shown == listed
    }

    pub(super) fn last_good(&self) -> Vec<usize> {
        self.last_good.clone()
    }

    pub(super) fn keep_last_good(&mut self) {
        self.last_good.clone_from(&self.shown);
    }

    /// Let every score see the names of an import a score loaded.
    pub(super) fn share(&mut self, ctx: &Ctx<'_>, specifier: &str) {
        let Some(id) = self.position(specifier) else {
            return;
        };
        if !self.imports[id].shared {
            self.imports[id].shared = true;
            let listed = std::mem::take(&mut self.shown);
            self.show(ctx, listed);
        }
    }

    /// Track the names the script of `specifier` set since `before`.
    pub(super) fn record<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        specifier: &str,
        before: &Snapshot<'js>,
    ) -> rquickjs::Result<()> {
        let id = self.imports.len();
        self.imports.push(ScoreImport {
            specifier: specifier.to_owned(),
            shared: false,
        });
        self.shown.push(id);
        for (object, target) in score_surface_objects(ctx)?.iter().enumerate() {
            let before = before.get(object);
            for name in own_names(target) {
                let Some(after) = own_data_value(ctx, target, &name) else {
                    continue;
                };
                let base = match before.and_then(|before| before.get(&name)) {
                    Some(None) => continue,
                    Some(Some(value)) if *value == after => continue,
                    Some(Some(value)) => Some(save(ctx, value.clone())),
                    None => None,
                };
                let meaning = (id, save(ctx, after));
                let tracked = self
                    .names
                    .iter_mut()
                    .find(|tracked| tracked.object == object && tracked.name == name);
                match tracked {
                    Some(tracked) => {
                        if tracked.showing.is_none() {
                            tracked.base = base;
                        }
                        tracked.meanings.push(meaning);
                        tracked.showing = Some(id);
                    }
                    None => self.names.push(TrackedName {
                        object,
                        name,
                        base,
                        meanings: vec![meaning],
                        showing: Some(id),
                    }),
                }
            }
        }
        Ok(())
    }

    /// Put on the surface the meanings for a score with the imports `listed`.
    pub(super) fn show(&mut self, ctx: &Ctx<'_>, listed: Vec<usize>) {
        self.shown = listed;
        let Ok(objects) = score_surface_objects(ctx) else {
            return;
        };
        for tracked in &mut self.names {
            let next = tracked.winner(&self.shown, &self.imports);
            let Some(target) = objects.get(tracked.object) else {
                continue;
            };
            if next == tracked.showing {
                continue;
            }
            if tracked.showing.is_none() {
                // A prebake or a score can set the base after the import ran.
                tracked.base =
                    own_data_value(ctx, target, &tracked.name).map(|value| save(ctx, value));
            }
            let saved = match next {
                Some(id) => tracked
                    .meanings
                    .iter()
                    .find_map(|(owner, value)| (*owner == id).then_some(value)),
                None => tracked.base.as_ref(),
            };
            let value = saved
                .and_then(|saved| saved.clone().restore(ctx).ok())
                .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
            if put_back_surface_name(target, &tracked.name, value).is_err() {
                // A name the score froze stays as the score left it.
                let _ = ctx.catch();
            }
            tracked.showing = next;
        }
    }
}

/// The own data values of each surface object.
pub(super) fn snapshot<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Snapshot<'js>> {
    Ok(score_surface_objects(ctx)?
        .iter()
        .map(|target| {
            own_names(target)
                .map(|name| {
                    let value = own_data_value(ctx, target, &name);
                    (name, value)
                })
                .collect()
        })
        .collect())
}

fn own_names<'js>(target: &rquickjs::Object<'js>) -> impl Iterator<Item = String> + use<'js> {
    target
        .own_keys::<String>(rquickjs::object::Filter::new().string())
        .flatten()
}

/// The value of an own data property. `None` for an accessor and for a name
/// the object does not have. Reads the descriptor, so no getter runs.
fn own_data_value<'js>(
    ctx: &Ctx<'js>,
    target: &rquickjs::Object<'js>,
    name: &str,
) -> Option<rquickjs::Value<'js>> {
    use rquickjs::qjs;

    let raw = ctx.as_raw().as_ptr();
    let mut descriptor = std::mem::MaybeUninit::<qjs::JSPropertyDescriptor>::uninit();
    // SAFETY: `ctx` and `target` are live and belong to one runtime. The atom
    // is freed before return. A result of 1 means QuickJS filled the
    // descriptor with three owned values, and each `Value` below takes one.
    unsafe {
        let atom = qjs::JS_NewAtomLen(raw, name.as_ptr().cast(), name.len() as _);
        if atom == qjs::JS_ATOM_NULL {
            let _ = ctx.catch();
            return None;
        }
        let found = qjs::JS_GetOwnProperty(raw, descriptor.as_mut_ptr(), target.as_raw(), atom);
        qjs::JS_FreeAtom(raw, atom);
        if found != 1 {
            if found < 0 {
                let _ = ctx.catch();
            }
            return None;
        }
        let descriptor = descriptor.assume_init();
        let value = rquickjs::Value::from_raw(ctx.clone(), descriptor.value);
        let _getter = rquickjs::Value::from_raw(ctx.clone(), descriptor.getter);
        let _setter = rquickjs::Value::from_raw(ctx.clone(), descriptor.setter);
        (descriptor.flags & qjs::JS_PROP_GETSET as i32 == 0).then_some(value)
    }
}
