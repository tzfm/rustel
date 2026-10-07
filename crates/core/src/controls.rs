/*
controls.rs - Generic native control construction and composition
Control helpers adapted from Strudel packages/core/controls.mjs.
Copyright (C) 2022 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use crate::ops::PatOps;
use crate::{OrderedMap, Pattern, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlSpec {
    /// Compound controls use each name positionally. The first name is the
    /// canonical control name and method name.
    pub names: Vec<Arc<str>>,
}

impl ControlSpec {
    pub fn new(names: impl IntoIterator<Item = impl Into<Arc<str>>>) -> Self {
        Self {
            names: names.into_iter().map(Into::into).collect(),
        }
    }

    pub fn name(&self) -> &str {
        self.names.first().map(|name| &**name).unwrap_or("")
    }

    /// Wraps a raw value into this control's named form.
    pub fn with_value(&self, mut value: Value) -> Value {
        // Compound controls inspect array elements positionally (`s`, `n`,
        // `gain`), so a script-owned array must be opened at that boundary;
        // otherwise it is stored wholesale under `s`. A single-name control
        // preserves an ordinary array as its value, identity included, for
        // a following callback to observe.
        let single_js_array = self.names.len() == 1
            && matches!(&value, Value::JsValue(reference) if reference.is_array());
        if !single_js_array {
            value = crate::materialize_js_value(&value);
        }
        let mut bag = match &value {
            Value::Object(object) if object.contains_key("value") => {
                let mut object = object.clone();
                value = object.remove("value").unwrap();
                Some(object)
            }
            _ => None,
        };

        if self.names.len() > 1
            && let Value::List(values) = value
        {
            let result = bag.get_or_insert_with(OrderedMap::new);
            for (name, value) in self.names.iter().zip(values) {
                result.insert(name.to_string(), value);
            }
            return Value::Object(result.clone());
        }

        if let Some(mut bag) = bag {
            bag.insert(self.name().into(), value);
            Value::Object(bag)
        } else {
            // An object is a value like any other here: `.dict({…})` hands
            // a chord dictionary that `voicing()` reads back under `dict`.
            Value::object([(self.name().into(), value)])
        }
    }

    fn pattern_for<P: PatOps>(&self, values: &P) -> P {
        values.map_control(self.clone(), false)
    }

    /// The receiver's own values, named: `"c e g".note()`. A value that is
    /// already controls has no unnamed value to name, so `s("bd").gain()`
    /// leaves it unchanged. With the whole event stored under `gain`, no
    /// voice resolves and the track is silent.
    fn pattern_for_unnamed<P: PatOps>(&self, values: &P) -> P {
        values.map_control(self.clone(), true)
    }

    pub fn pattern(&self, values: &Pattern) -> Pattern {
        self.pattern_for(values)
    }

    /// Method form. With no argument, maps the pattern's unnamed values into
    /// the control. With an argument, sets the control while preserving the
    /// receiver's structure.
    ///
    /// `pat.set(x)` is the COMPOSERS `set` at the DEFAULT alignment (`in`),
    /// so it goes through the same matrix as `add`/`keep` - object promotion
    /// included.
    fn apply_for<P: PatOps>(&self, pattern: &P, value: Option<P>) -> P {
        match value {
            None => self.pattern_for_unnamed(pattern),
            Some(value) => crate::compose::compose(
                pattern,
                &self.pattern_for(&value),
                crate::compose::ComposeOp::Set,
                // Controls route through the DEFAULT `set` wrapper, so
                // setDefaultJoin('mix') restructures them.
                crate::compose::default_alignment(),
            ),
        }
    }

    pub fn apply(&self, pattern: &Pattern, value: Option<Pattern>) -> Pattern {
        self.apply_for(pattern, value)
    }

    /// A compound control called with one argument per positional name:
    /// `limit(-6, "hard")` beside `limit("-6:hard")`.
    ///
    /// The mini-notation form joins the parts into one string, and a string
    /// cannot hold a pattern such as `slider(-6, -24, 0)`. As separate
    /// arguments, each part keeps its own structure, so a slider on the
    /// ceiling stays a slider. The parts merge as any two controls do.
    ///
    /// Arguments past the names are dropped, as extra elements of the
    /// mini-notation form already are.
    pub fn apply_positional(&self, pattern: &Pattern, parts: &[Pattern]) -> Pattern {
        // Each name applied in turn, which is exactly what writing them out
        // by hand does: `limit(-6, "hard")` is `limit(-6).limitchar("hard")`
        // and goes down the same path, rather than a second way of building
        // the same object that could merge it differently.
        self.names
            .iter()
            .zip(parts)
            .fold(pattern.clone(), |so_far, (name, part)| {
                Self::new([name.clone()]).apply(&so_far, Some(part.clone()))
            })
    }

    /// Free-function form: `gain(0.5)` with no receiver.
    pub fn standalone(&self, value: &Pattern) -> Pattern {
        self.pattern(value)
    }
}

/// Apply one generated control to a pattern while retaining the caller's
/// `Pattern`/`PurePattern` view.
///
/// Extensions composed from existing language pieces use this instead of
/// duplicating object promotion, control aliases, lookup flow, or the current
/// default join policy.
pub fn apply_pattern<P: PatOps>(name: &str, pattern: &P, value: &P) -> P {
    default_control_registry()
        .get(name)
        .unwrap_or_else(|| panic!("generated control registry is missing {name}"))
        .apply_for(pattern, Some(value.clone()))
}

/// The compose `set` applied to two values, object promotion included.
pub fn set_value(left: &Value, right: &Value) -> Value {
    if matches!(left, Value::Object(_)) || matches!(right, Value::Object(_)) {
        let mut out = match left {
            Value::Object(object) => object.clone(),
            value => OrderedMap::from_entries([("value".into(), value.clone())]),
        };
        match right {
            Value::Object(object) => {
                for (key, value) in object {
                    out.insert(key.clone(), value.clone());
                }
            }
            value => {
                out.insert("value".into(), value.clone());
            }
        }
        Value::Object(out)
    } else {
        right.clone()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ControlRegistry {
    specs: Vec<ControlSpec>,
    aliases: BTreeMap<Arc<str>, usize>,
}

impl ControlRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        names: impl IntoIterator<Item = impl Into<Arc<str>>>,
        aliases: impl IntoIterator<Item = impl Into<Arc<str>>>,
    ) {
        let spec = ControlSpec::new(names);
        let index = self.specs.len();
        // Only `names[0]` becomes an accessor. The SECONDARY positional names
        // of a compound control are output keys, not entry points - `s` writes
        // `n` and `gain`, but `pat.n(…)` is the separately registered `n`
        // control, and `pat.resonance(…)` is `resonance`, not `cutoff`.
        // Registering every positional name here would let a compound control
        // shadow a real one depending on declaration order.
        if let Some(name) = spec.names.first() {
            self.aliases.insert(name.clone(), index);
        }
        for alias in aliases {
            self.aliases.insert(alias.into(), index);
        }
        self.specs.push(spec);
    }

    pub fn get(&self, name: &str) -> Option<&ControlSpec> {
        self.aliases.get(name).map(|index| &self.specs[*index])
    }

    /// Every registered name (aliases included) with its canonical name -
    /// the alias → control-name map.
    pub fn alias_entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.aliases
            .iter()
            .map(|(alias, index)| (alias.as_ref(), self.specs[*index].name()))
    }

    pub fn canonical_name(&self, name: &str) -> Option<&str> {
        self.get(name).map(ControlSpec::name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.aliases.keys().map(|name| &**name)
    }

    /// Number of distinct controls (not counting aliases).
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
}

/// The output key a control name writes, resolving aliases.
///
/// Kept as the lightweight public compatibility entry point introduced with
/// MIDI maps; the process-wide registry remains the single source of truth.
pub fn canonical_control_name(name: &str) -> Option<&'static str> {
    default_control_registry().canonical_name(name)
}

/// The complete pinned control surface from the table that also documents
/// it: each row installs one control and carries its reference entry.
pub fn default_control_registry() -> &'static ControlRegistry {
    static REGISTRY: std::sync::LazyLock<ControlRegistry> = std::sync::LazyLock::new(|| {
        let mut registry = ControlRegistry::new();
        for row in crate::controls_generated::CONTROLS {
            registry.register(row.names.iter().copied(), row.aliases.iter().copied());
        }
        registry
    });
    &REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Value, pure};
    use rustel_fraction::Fraction;

    #[test]
    fn public_canonical_name_uses_the_shared_registry() {
        assert_eq!(canonical_control_name("lpf"), Some("cutoff"));
        assert_eq!(canonical_control_name("cutoff"), Some("cutoff"));
        assert_eq!(canonical_control_name("not-a-control"), None);
    }

    #[test]
    fn compound_control_ignores_extra_values() {
        let sound = ControlSpec::new(["s", "n", "gain"]);
        assert_eq!(
            sound.with_value(Value::List(vec![
                Value::Str("bd".into()),
                Value::F64(2.0),
                Value::F64(0.5),
                Value::F64(99.0),
            ])),
            Value::object([
                ("s".into(), Value::Str("bd".into())),
                ("n".into(), Value::F64(2.0)),
                ("gain".into(), Value::F64(0.5)),
            ])
        );
    }

    #[test]
    fn unnamed_control_preserves_existing_bag() {
        let note = ControlSpec::new(["note"]);
        let input = Value::object([
            ("value".into(), Value::Str("c4".into())),
            ("gain".into(), Value::F64(0.7)),
        ]);
        assert_eq!(
            note.with_value(input),
            Value::object([
                ("gain".into(), Value::F64(0.7)),
                ("note".into(), Value::Str("c4".into())),
            ])
        );
    }

    /// A chord dictionary is an object. `.dict({...})` must pass it to
    /// `voicing()` whole under `dict`, and must not spread it into the event.
    #[test]
    fn an_object_argument_is_named_like_any_other_value() {
        let dict = ControlSpec::new(["dict"]);
        let dictionary =
            Value::object([("".into(), Value::List(vec![Value::Str("0 7 12 16".into())]))]);
        assert_eq!(
            dict.with_value(dictionary.clone()),
            Value::object([("dict".into(), dictionary)])
        );
    }

    /// With no argument, a control names the receiver's raw values, as in
    /// `"c e g".note()`. It leaves values that are already controls unchanged:
    /// `s("bd").gain()` does not store the event under `gain`.
    #[test]
    fn a_bare_control_names_raw_values_and_leaves_controls_alone() {
        let note = ControlSpec::new(["note"]);
        let raw = note.apply(&pure(Value::Str("c4".into())), None);
        assert_eq!(
            raw.query_arc_sorted(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::object([("note".into(), Value::Str("c4".into()))])
        );
        let gain = ControlSpec::new(["gain"]);
        let event = Value::object([("s".into(), Value::Str("bd".into()))]);
        let kept = gain.apply(&pure(event.clone()), None);
        assert_eq!(
            kept.query_arc_sorted(Fraction::ZERO, Fraction::ONE)[0].value,
            event
        );
        // A bag with an unnamed value still has one to name.
        let bag = Value::object([
            ("value".into(), Value::Str("c4".into())),
            ("gain".into(), Value::F64(0.7)),
        ]);
        let named = note.apply(&pure(bag), None);
        assert_eq!(
            named.query_arc_sorted(Fraction::ZERO, Fraction::ONE)[0].value,
            Value::object([
                ("gain".into(), Value::F64(0.7)),
                ("note".into(), Value::Str("c4".into())),
            ])
        );
    }

    #[test]
    fn setting_control_preserves_receiver_structure() {
        let gain = ControlSpec::new(["gain"]);
        let base = crate::fastcat(vec![
            pure(Value::Str("bd".into())),
            pure(Value::Str("sd".into())),
        ]);
        let result = gain.apply(&base, Some(pure(Value::F64(0.5))));
        let haps = result.query_arc_sorted(Fraction::ZERO, Fraction::ONE);
        assert_eq!(haps.len(), 2);
        assert_eq!(
            haps[0].value,
            Value::object([
                ("value".into(), Value::Str("bd".into())),
                ("gain".into(), Value::F64(0.5)),
            ])
        );
    }
}
