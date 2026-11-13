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

use crate::Value;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub(crate) struct ContextKeys(Option<Arc<Keys>>);

#[derive(Clone, Debug, Default)]
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
}

static NO_KEYS: Keys = Keys {
    scale: None,
    edo_size: None,
    scale_definition: None,
    tags: None,
    log_line: None,
};

impl ContextKeys {
    /// `keys`, sharing nothing when no key is set.
    pub(crate) fn of(keys: Keys) -> Self {
        let empty = keys.scale.is_none()
            && keys.edo_size.is_none()
            && keys.scale_definition.is_none()
            && keys.tags.is_none()
            && keys.log_line.is_none();
        Self((!empty).then(|| Arc::new(keys)))
    }

    /// Whether no key is set, so these share and own nothing.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn get(&self) -> &Keys {
        self.0.as_deref().unwrap_or(&NO_KEYS)
    }

    /// The keys to change, copied first when another hap shares them.
    pub(crate) fn edit(&mut self) -> &mut Keys {
        Arc::make_mut(self.0.get_or_insert_with(Default::default))
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
                        && a.log_line == b.log_line)
            }
            _ => false,
        }
    }

    /// Each key from `first` where it has one, else from `second`. A
    /// context merge keeps the extending side's key, as `Option::or` does.
    pub(crate) fn merge(first: &Self, second: &Self) -> Self {
        match (&first.0, &second.0) {
            (_, None) => first.clone(),
            (None, _) => second.clone(),
            (Some(a), Some(b)) => {
                if a.covers(b) {
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
                })))
            }
        }
    }
}

impl Keys {
    /// Whether every key `other` has is set here, so a merge keeps these.
    fn covers(&self, other: &Self) -> bool {
        (self.scale.is_some() || other.scale.is_none())
            && (self.edo_size.is_some() || other.edo_size.is_none())
            && (self.scale_definition.is_some() || other.scale_definition.is_none())
            && (self.tags.is_some() || other.tags.is_none())
            && (self.log_line.is_some() || other.log_line.is_none())
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
