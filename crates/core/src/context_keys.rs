/*
context_keys.rs - the context keys a hap carries besides its locations
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! The context keys of a hap besides its locations: `scale`, `edoSize`,
//! `scaleDefinition`, `tags` and `logLine`.
//!
//! Most haps carry none of them, so the keys sit behind one shared pointer
//! that a clone copies. A change copies them first.

use crate::{PickLookup, Value};
use std::fmt;
use std::sync::{Arc, OnceLock};

static UI_VISUAL_MASK_CACHE: [OnceLock<Arc<Keys>>; 64] = [const { OnceLock::new() }; 64];
static UI_VISUAL_SINGLE_CACHE: [OnceLock<Arc<Keys>>; 64] = [const { OnceLock::new() }; 64];

fn cached_ui_visual_mask(mask: u64) -> Option<Arc<Keys>> {
    if mask == 0 {
        return None;
    }
    if mask < 64 {
        let cell = &UI_VISUAL_MASK_CACHE[mask as usize];
        return Some(Arc::clone(cell.get_or_init(|| {
            Arc::new(Keys {
                ui_visuals: mask,
                ..Keys::default()
            })
        })));
    }
    if mask.is_power_of_two() {
        let slot = mask.trailing_zeros() as usize;
        let cell = &UI_VISUAL_SINGLE_CACHE[slot];
        return Some(Arc::clone(cell.get_or_init(|| {
            Arc::new(Keys {
                ui_visuals: mask,
                ..Keys::default()
            })
        })));
    }
    None
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ContextKeys(Option<Arc<Keys>>);

impl ContextKeys {
    /// No key: what a hap holds that nothing gave keys to.
    pub(crate) const NONE: Self = Self(None);
}

#[derive(Clone, Default)]
pub(crate) struct Keys {
    /// `hap.context.scale`: set by `.scale()`, read by `.scaleTranspose()`.
    pub(crate) scale: Option<Arc<str>>,
    /// `hap.context.edoSize`: set by xen's `.xen()`/`.edo()`, read by
    /// `.ftrans()`.
    pub(crate) edo_size: Option<f64>,
    /// `hap.context.scaleDefinition`, as `edoScale` tagged it. The JSON a
    /// callback sees matches the raw argument object.
    pub(crate) scale_definition: Option<Arc<Value>>,
    /// `hap.context.tags`: appended by `.tag()`, read by `hap.hasTag()`
    /// inside a `filter`. A tag list is shared, never changed in place.
    pub(crate) tags: Option<Arc<[Arc<str>]>>,
    /// `hap.context.logLine`: the text `.log()` prints when the hap
    /// triggers.
    ///
    /// It rides the context rather than the value because a value is what a
    /// score means: `"0 1".log()` carries a bare number, and adding a key to
    /// it would make the pattern mean something else. The formatter runs
    /// when the hap is made. Only the printing waits for the onset.
    pub(crate) log_line: Option<Arc<str>>,
    /// Direct live slider bindings: gain, low-pass cutoff, low-pass
    /// resonance, then the direct FM route's index. Zero is unbound.
    /// These are host tokens, never score values or source-location guesses.
    pub(crate) live_controls: [u64; 4],
    pub(crate) slider_binding: u64,
    pub(crate) ui_visuals: u64,
    /// Exact JavaScript lookup shape (holes, length, enumerable keys) when the
    /// visible value alone cannot represent it. Nested Patterns also live in
    /// `value`; this side channel is shape metadata, not their only owner.
    pub(crate) pick_lookup: Option<Arc<PickLookup>>,
}

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keys")
            .field("scale", &self.scale)
            .field("edo_size", &self.edo_size)
            .field("scale_definition", &self.scale_definition)
            .field("tags", &self.tags)
            .field("log_line", &self.log_line)
            .field("live_controls", &self.live_controls)
            .field("slider_binding", &self.slider_binding)
            .field("ui_visuals", &self.ui_visuals)
            .field("pick_lookup", &self.pick_lookup.is_some())
            .finish()
    }
}

static NO_KEYS: Keys = Keys {
    scale: None,
    edo_size: None,
    scale_definition: None,
    tags: None,
    log_line: None,
    live_controls: [0; 4],
    slider_binding: 0,
    ui_visuals: 0,
    pick_lookup: None,
};

fn same_pick_lookup(a: &Option<Arc<PickLookup>>, b: &Option<Arc<PickLookup>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

impl ContextKeys {
    /// `keys`, sharing nothing when no key is set.
    pub(crate) fn of(keys: Keys) -> Self {
        Self((!keys.is_empty()).then(|| Arc::new(keys)))
    }

    /// Whether no key is set, so these share and own nothing.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// Whether no live binding, visual bit, or lookup shape is carried here.
    #[inline]
    pub(crate) fn has_only_context_keys(&self) -> bool {
        self.0.as_deref().is_none_or(Keys::has_only_context_keys)
    }

    #[inline]
    pub(crate) fn get(&self) -> &Keys {
        self.0.as_deref().unwrap_or(&NO_KEYS)
    }

    /// The keys to change, copied first when another hap shares them.
    pub(crate) fn edit(&mut self) -> &mut Keys {
        Arc::make_mut(self.0.get_or_insert_with(Default::default))
    }

    /// Set visual slot bit `slot` on these keys, using static cached `Arc<Keys>`
    /// entries when no other context keys are present.
    #[inline]
    pub(crate) fn add_ui_visual_slot(&mut self, slot: u8) {
        if slot >= 64 {
            return;
        }
        let bit = 1_u64 << slot;
        match &self.0 {
            None => {
                if let Some(cached) = cached_ui_visual_mask(bit) {
                    self.0 = Some(cached);
                    return;
                }
            }
            Some(arc) => {
                if arc.ui_visuals & bit != 0 {
                    return;
                }
                if arc.is_ui_visuals_only()
                    && let Some(cached) = cached_ui_visual_mask(arc.ui_visuals | bit)
                {
                    self.0 = Some(cached);
                    return;
                }
            }
        }
        self.edit().ui_visuals |= bit;
    }

    /// Like [`Self::add_ui_visual_slot`], additionally checking a persistent
    /// node-level `OnceLock` cache first and a per-query local cache second so
    /// haps sharing non-visual keys (such as a layer's `scale` context) allocate
    /// once across the lifetime of the graph instead of once per query.
    #[inline]
    pub(crate) fn add_ui_visual_slot_cached(
        &mut self,
        slot: u8,
        once: &std::sync::OnceLock<Box<(Arc<Keys>, Arc<Keys>)>>,
        local: &mut Option<(Arc<Keys>, Arc<Keys>)>,
    ) {
        if slot >= 64 {
            return;
        }
        let bit = 1_u64 << slot;
        match &self.0 {
            None => {
                if let Some(cached) = cached_ui_visual_mask(bit) {
                    self.0 = Some(cached);
                    return;
                }
            }
            Some(arc) => {
                if arc.ui_visuals & bit != 0 {
                    return;
                }
                if arc.is_ui_visuals_only()
                    && let Some(cached) = cached_ui_visual_mask(arc.ui_visuals | bit)
                {
                    self.0 = Some(cached);
                    return;
                }
                if let Some(pair) = once.get() {
                    if Arc::ptr_eq(arc, &pair.0) {
                        self.0 = Some(Arc::clone(&pair.1));
                        return;
                    }
                    if let Some((cached_in, cached_out)) = local
                        && Arc::ptr_eq(arc, cached_in)
                    {
                        self.0 = Some(Arc::clone(cached_out));
                        return;
                    }
                    let input = Arc::clone(arc);
                    let mut next = (**arc).clone();
                    next.ui_visuals |= bit;
                    let output = Arc::new(next);
                    *local = Some((input, Arc::clone(&output)));
                    self.0 = Some(output);
                    return;
                }
                let input = Arc::clone(arc);
                let mut next = (**arc).clone();
                next.ui_visuals |= bit;
                let output = Arc::new(next);
                let _ = once.set(Box::new((input, Arc::clone(&output))));
                self.0 = Some(output);
                return;
            }
        }
        self.edit().ui_visuals |= bit;
    }

    /// Drop the backing allocation when every field is cleared.
    #[inline]
    pub(crate) fn trim(&mut self) {
        if self.0.as_deref().is_some_and(Keys::is_empty) {
            self.0 = None;
        }
    }

    /// Whether these and `other` hold the same keys, down to the bits of a
    /// number.
    pub(crate) fn same(&self, other: &Self) -> bool {
        let same_definition = |a: &Option<Arc<Value>>, b: &Option<Arc<Value>>| match (a, b) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || crate::same_key(a, b),
            (None, None) => true,
            _ => false,
        };
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                Arc::ptr_eq(a, b)
                    || (a.scale == b.scale
                        && a.edo_size.map(f64::to_bits) == b.edo_size.map(f64::to_bits)
                        && same_definition(&a.scale_definition, &b.scale_definition)
                        && a.tags == b.tags
                        && a.log_line == b.log_line
                        && a.live_controls == b.live_controls
                        && a.slider_binding == b.slider_binding
                        && a.ui_visuals == b.ui_visuals
                        && same_pick_lookup(&a.pick_lookup, &b.pick_lookup))
            }
            _ => false,
        }
    }

    /// Each key from `first` where it has one, else from `second`. Visual bits
    /// combine with bitwise OR, while live bindings and lookup shape follow
    /// `first`.
    #[inline]
    pub(crate) fn merge(first: &Self, second: &Self) -> Self {
        match (&first.0, &second.0) {
            (None, None) => Self::NONE,
            (Some(_), None) => first.clone(),
            (None, Some(b)) => {
                if b.has_only_context_keys()
                    || (b.live_controls == [0; 4]
                        && b.slider_binding == 0
                        && b.pick_lookup.is_none())
                {
                    return second.clone();
                }
                Self::of(Keys {
                    scale: b.scale.clone(),
                    edo_size: b.edo_size,
                    scale_definition: b.scale_definition.clone(),
                    tags: b.tags.clone(),
                    log_line: b.log_line.clone(),
                    live_controls: [0; 4],
                    slider_binding: 0,
                    ui_visuals: b.ui_visuals,
                    pick_lookup: None,
                })
            }
            (Some(a), Some(b)) => {
                if a.covers_context_and_visuals(b) {
                    return first.clone();
                }
                Self(Some(Arc::new(Keys {
                    scale: a.scale.clone().or_else(|| b.scale.clone()),
                    edo_size: a.edo_size.or(b.edo_size),
                    scale_definition: a
                        .scale_definition
                        .clone()
                        .or_else(|| b.scale_definition.clone()),
                    tags: a.tags.clone().or_else(|| b.tags.clone()),
                    log_line: a.log_line.clone().or_else(|| b.log_line.clone()),
                    live_controls: a.live_controls,
                    slider_binding: a.slider_binding,
                    ui_visuals: a.ui_visuals | b.ui_visuals,
                    pick_lookup: a.pick_lookup.clone(),
                })))
            }
        }
    }

    /// Merge context keys (`first` wins) and visual bits (OR), while setting
    /// live bindings and lookup shape explicitly.
    #[inline]
    pub(crate) fn merge_with(
        first: &Self,
        second: &Self,
        live_controls: [u64; 4],
        slider_binding: u64,
        pick_lookup: Option<Arc<PickLookup>>,
    ) -> Self {
        if first.0.is_none()
            && second.0.is_none()
            && live_controls == [0; 4]
            && slider_binding == 0
            && pick_lookup.is_none()
        {
            return Self::NONE;
        }
        let a = first.get();
        let b = second.get();
        if first.0.is_some()
            && a.covers_context_and_visuals(b)
            && a.live_controls == live_controls
            && a.slider_binding == slider_binding
            && same_pick_lookup(&a.pick_lookup, &pick_lookup)
        {
            return first.clone();
        }
        if first.0.is_none()
            && second.0.is_some()
            && b.live_controls == live_controls
            && b.slider_binding == slider_binding
            && same_pick_lookup(&b.pick_lookup, &pick_lookup)
        {
            return second.clone();
        }
        Self::of(Keys {
            scale: a.scale.clone().or_else(|| b.scale.clone()),
            edo_size: a.edo_size.or(b.edo_size),
            scale_definition: a
                .scale_definition
                .clone()
                .or_else(|| b.scale_definition.clone()),
            tags: a.tags.clone().or_else(|| b.tags.clone()),
            log_line: a.log_line.clone().or_else(|| b.log_line.clone()),
            live_controls,
            slider_binding,
            ui_visuals: a.ui_visuals | b.ui_visuals,
            pick_lookup,
        })
    }
}

impl Keys {
    fn is_empty(&self) -> bool {
        self.scale.is_none()
            && self.edo_size.is_none()
            && self.scale_definition.is_none()
            && self.tags.is_none()
            && self.log_line.is_none()
            && self.live_controls == [0; 4]
            && self.slider_binding == 0
            && self.ui_visuals == 0
            && self.pick_lookup.is_none()
    }

    fn has_only_context_keys(&self) -> bool {
        self.live_controls == [0; 4]
            && self.slider_binding == 0
            && self.ui_visuals == 0
            && self.pick_lookup.is_none()
    }

    #[inline]
    fn is_ui_visuals_only(&self) -> bool {
        self.scale.is_none()
            && self.edo_size.is_none()
            && self.scale_definition.is_none()
            && self.tags.is_none()
            && self.log_line.is_none()
            && self.live_controls == [0; 4]
            && self.slider_binding == 0
            && self.pick_lookup.is_none()
    }

    /// Whether every context key and visual bit `other` has is already set here.
    fn covers_context_and_visuals(&self, other: &Self) -> bool {
        (self.scale.is_some() || other.scale.is_none())
            && (self.edo_size.is_some() || other.edo_size.is_none())
            && (self.scale_definition.is_some() || other.scale_definition.is_none())
            && (self.tags.is_some() || other.tags.is_none())
            && (self.log_line.is_some() || other.log_line.is_none())
            && (self.ui_visuals & other.ui_visuals == other.ui_visuals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_scale(scale: &str) -> ContextKeys {
        let mut keys = ContextKeys::default();
        keys.edit().scale = Some(Arc::from(scale));
        keys
    }

    #[test]
    fn a_merge_keeps_the_first_sides_keys_and_fills_the_rest() {
        let mut second = with_scale("b:minor");
        second.edit().edo_size = Some(19.0);
        let merged = ContextKeys::merge(&with_scale("a:major"), &second);
        assert_eq!(merged.get().scale.as_deref(), Some("a:major"));
        assert_eq!(merged.get().edo_size, Some(19.0));
        let empty = ContextKeys::merge(&ContextKeys::default(), &ContextKeys::default());
        assert!(empty.0.is_none(), "two empty sides share nothing");
    }

    #[test]
    fn an_edit_does_not_change_a_hap_that_shares_the_keys() {
        let first = with_scale("c:major");
        let mut copy = first.clone();
        copy.edit().scale = Some(Arc::from("d:dorian"));
        assert_eq!(first.get().scale.as_deref(), Some("c:major"));
        assert_eq!(copy.get().scale.as_deref(), Some("d:dorian"));
    }
}
