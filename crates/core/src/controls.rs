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
use crate::{Hap, Key, OrderedMap, Pattern, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct ControlSpec {
    /// Compound controls use each name positionally. The first name is the
    /// canonical control name and method name. Each copy of the spec, and
    /// each sequence that the control names, shares the one list.
    pub names: Arc<[Arc<str>]>,
    /// The map of this control over the haps of a pattern, for each value
    /// and for unnamed values alone. Each is the interned map of the names,
    /// kept at its first use, so a later call takes no lock. A copy of the
    /// spec shares the maps it has.
    maps: [std::sync::OnceLock<crate::OwnedHapMapFn>; 2],
}

/// A spec is its names: the maps it keeps follow from them.
impl PartialEq for ControlSpec {
    fn eq(&self, other: &Self) -> bool {
        self.names == other.names
    }
}

impl Eq for ControlSpec {}

impl std::hash::Hash for ControlSpec {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.names.hash(state);
    }
}

impl std::fmt::Debug for ControlSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlSpec")
            .field("names", &self.names)
            .finish_non_exhaustive()
    }
}

impl ControlSpec {
    pub fn new(names: impl IntoIterator<Item = impl Into<Arc<str>>>) -> Self {
        Self {
            names: names.into_iter().map(Into::into).collect(),
            maps: Default::default(),
        }
    }

    pub fn name(&self) -> &str {
        self.names.first().map(|name| &**name).unwrap_or("")
    }

    /// Wraps a raw value into this control's named form.
    pub fn with_value(&self, value: Value) -> Value {
        named_value(&self.names, value)
    }

    /// The canonical name as a map key, which every map this control makes
    /// shares.
    fn key(&self) -> Key {
        key_of(&self.names)
    }

    fn pattern_for<P: PatOps>(&self, values: &P) -> P {
        values.map_control(self, false)
    }

    /// The receiver's own values, named: `"c e g".note()`. A value that is
    /// already controls has no unnamed value to name, so `s("bd").gain()`
    /// leaves it unchanged. With the whole event stored under `gain`, no
    /// voice resolves and the track is silent.
    fn pattern_for_unnamed<P: PatOps>(&self, values: &P) -> P {
        values.map_control(self, true)
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
    ///
    /// The receiver moves in: the result of one call of a chain is the
    /// receiver of the next, and no other handle holds it, so a constant
    /// control is set on it where it is.
    fn apply_for<P: PatOps>(&self, mut pattern: P, value: Option<&P>) -> P {
        let Some(value) = value else {
            return self.pattern_for_unnamed(&pattern);
        };
        // Controls route through the DEFAULT `set` wrapper, so
        // setDefaultJoin('mix') restructures them.
        let alignment = crate::compose::default_alignment();
        // A constant on a sequence joins each step where the step is. That
        // is the node the composition below folds to, built here with no
        // pattern of the constant between.
        if alignment == crate::compose::Alignment::In {
            pattern = match pattern.set_constant_control(self, value) {
                Ok(set) => return set,
                Err(back) => back,
            };
        }
        crate::compose::compose(
            &pattern,
            &self.pattern_for(value),
            crate::compose::ComposeOp::Set,
            alignment,
        )
    }

    pub fn apply(&self, pattern: &Pattern, value: Option<Pattern>) -> Pattern {
        // A handle of the receiver, which its owner keeps as it is.
        self.apply_for(pattern.clone(), value.as_ref())
    }

    /// [`Self::apply`] for a receiver that the caller gives away, as the
    /// next call of a chain takes the result of the call before it.
    pub fn apply_to(&self, pattern: Pattern, value: Option<Pattern>) -> Pattern {
        self.apply_for(pattern, value.as_ref())
    }

    /// [`Self::apply_to`] of a `pure` of `value`, as a score writes a
    /// number. A receiver that takes the control where it is takes it with
    /// no pattern of the value.
    pub fn apply_value(&self, mut pattern: Pattern, value: Value) -> Pattern {
        let alignment = crate::compose::default_alignment();
        if alignment == crate::compose::Alignment::In {
            pattern = match pattern.set_control_value(self, &value) {
                Ok(set) => return set,
                Err(back) => back,
            };
        }
        crate::compose::compose(
            &pattern,
            &self.pattern_for(&crate::pure(value)),
            crate::compose::ComposeOp::Set,
            alignment,
        )
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
                Self::new([name.clone()]).apply_to(so_far, Some(part.clone()))
            })
    }

    /// Free-function form: `gain(0.5)` with no receiver.
    pub fn standalone(&self, value: &Pattern) -> Pattern {
        self.pattern(value)
    }
}

/// The names of the control `note`, which a rewrite of pitch names each
/// note by.
pub(crate) fn note_names() -> Arc<[Arc<str>]> {
    static NAMES: std::sync::LazyLock<Arc<[Arc<str>]>> =
        std::sync::LazyLock::new(|| Arc::from([Arc::from("note")]));
    Arc::clone(&NAMES)
}

/// The first of `names`, the canonical name of their control, as a map key.
pub(crate) fn key_of(names: &[Arc<str>]) -> Key {
    names
        .first()
        .map_or_else(|| Key::from(""), |name| Key::from(Arc::clone(name)))
}

/// Whether a map holds its value under `value`, apart from its controls.
fn has_value_key(object: &OrderedMap) -> bool {
    object.contains_key("value")
}

/// Whether a list of values is one value for each of `names`.
fn opens_list(names: &[Arc<str>], value: &Value) -> bool {
    names.len() > 1 && matches!(value, Value::List(_))
}

/// Whether [`named_value`] makes the map of one entry of `value`: the key
/// of the control, and `value` as it is. A data value that holds a `value`
/// key or opens as a list under more than one name makes another map.
pub(crate) fn names_as_one_entry(names: &[Arc<str>], value: &Value) -> bool {
    !opens_list(names, value) && !matches!(value, Value::Object(object) if has_value_key(object))
}

/// `value` in the named form of the control of `names`: see
/// [`ControlSpec::with_value`].
pub(crate) fn named_value(names: &[Arc<str>], mut value: Value) -> Value {
    // Compound controls inspect array elements positionally (`s`, `n`,
    // `gain`), so a script-owned array must be opened at that boundary;
    // otherwise it is stored wholesale under `s`. A single-name control
    // preserves an ordinary array as its value, identity included, for
    // a following callback to observe.
    let single_js_array =
        names.len() == 1 && matches!(&value, Value::JsValue(reference) if reference.is_array());
    // A value of data holds nothing of the script to open.
    if !single_js_array && value.contains_js_value() {
        value = crate::materialize_js_value(&value);
    }
    let mut bag = match &value {
        Value::Object(object) if has_value_key(object) => {
            let mut object = object.clone();
            value = object.remove("value").expect("the key was just checked");
            Some(object)
        }
        _ => None,
    };

    if opens_list(names, &value)
        && let Value::List(values) = value
    {
        let result = bag.get_or_insert_with(OrderedMap::new);
        for (name, value) in names.iter().zip(values) {
            result.insert_key(Key::from(Arc::clone(name)), value);
        }
        return Value::Object(result.clone());
    }

    // An object is a value like any other here: `.dict({…})` hands a
    // chord dictionary that `voicing()` reads back under `dict`.
    let Some(mut named) = bag else {
        return Value::Object(OrderedMap::of_one(key_of(names), value));
    };
    named.insert_key(key_of(names), value);
    Value::Object(named)
}

/// The argument of one control call of a chain: a number as a score writes
/// it, or a pattern.
#[derive(Clone)]
pub enum ChainArgument {
    Number(f64),
    Pattern(Pattern),
}

impl ChainArgument {
    /// Whether this argument is a constant of one plain value.
    fn is_constant(&self) -> bool {
        match self {
            Self::Number(_) => true,
            Self::Pattern(values) => crate::sequence::is_constant_value(values),
        }
    }

    /// The value that this argument is a constant of, with the place of
    /// the value in the text. `None` for a pattern that is no constant of
    /// one plain value.
    fn constant(&self) -> Option<(Value, crate::Locations)> {
        match self {
            Self::Number(number) => Some((Value::F64(*number), crate::Locations::new())),
            Self::Pattern(values) => crate::sequence::constant_value_at(values),
        }
    }
}

/// One control call of a chain: its control and its argument.
pub type ChainCall<'a> = (&'a ControlSpec, ChainArgument);

/// `pattern` with each control of `chain` applied in turn: a number as
/// [`ControlSpec::apply_value`] applies it, and a pattern as
/// [`ControlSpec::apply_to`] does.
///
/// A chain of controls is mostly constants in a row, such as
/// `.s("saw").lpf(800).room(0.3)`. A receiver that takes constants takes
/// such a row in one step: the union of the controls, set one time, where
/// each control alone visits every node and step under the receiver.
pub fn apply_chain(mut pattern: Pattern, chain: &[ChainCall<'_>]) -> Pattern {
    let folds = crate::compose::default_alignment() == crate::compose::Alignment::In;
    // Where the constants in a row so far begin.
    let mut row = 0;
    for (index, (spec, argument)) in chain.iter().enumerate() {
        // A row begins on a pattern that a constant may go into.
        let held = folds && (row < index || pattern.takes_constants());
        if held && argument.is_constant() {
            continue;
        }
        pattern = apply_row(pattern, &chain[row..index]);
        pattern = apply_argument(spec, pattern, argument);
        row = index + 1;
    }
    apply_row(pattern, &chain[row..])
}

/// The transformer that applies `chain` to each pattern as [`apply_chain`]
/// does, under `name`. It keeps a pure pattern pure when each pattern of
/// the chain is pure: a control composes pure operands into a pure result.
pub fn chain_transformer(
    name: &str,
    chain: impl IntoIterator<Item = ChainCall<'static>>,
) -> crate::value::FunctionRef {
    let chain: Arc<[ChainCall<'static>]> = chain.into_iter().collect();
    let pure = chain.iter().all(|(_, argument)| match argument {
        ChainArgument::Number(_) => true,
        ChainArgument::Pattern(values) => values.is_pure(),
    });
    crate::value::FunctionRef::registered(
        name,
        Arc::new(move |pattern| apply_chain(pattern, &chain)),
        pure,
    )
}

/// `pattern` with the constants of `row` applied: in one step when the
/// pattern takes them so, and in turn otherwise.
///
/// A step that is longer than a cycle takes no row: a constant is another
/// hap in each cycle, which cuts the step. The first control in turn makes
/// the cut, and the steps then take the others as one row.
fn apply_row(pattern: Pattern, row: &[ChainCall<'_>]) -> Pattern {
    let Some(((spec, argument), rest)) = row.split_first() else {
        return pattern;
    };
    let in_turn =
        |pattern, (spec, argument): &ChainCall<'_>| apply_argument(spec, pattern, argument);
    let back = match set_row(pattern, row) {
        Ok(set) => return set,
        Err(back) => back,
    };
    let cut = apply_argument(spec, back, argument);
    if rest.is_empty() {
        return cut;
    }
    match set_row(cut, rest) {
        Ok(set) => set,
        Err(back) => rest.iter().fold(back, in_turn),
    }
}

/// `pattern` with the constants of `row` set in one step. `Err` gives the
/// pattern back as it was, when it does not take the row so.
fn set_row(pattern: Pattern, row: &[ChainCall<'_>]) -> Result<Pattern, Pattern> {
    if let [(spec, argument)] = row {
        // One control, as its method sets it: a number through its method.
        return match argument {
            ChainArgument::Pattern(values) => pattern.set_constant_control(spec, values),
            ChainArgument::Number(_) => Err(pattern),
        };
    }
    let Some((union, context)) = union_of(row) else {
        return Err(pattern);
    };
    // The control of each call, and its locations.
    let each = || {
        let apart = row.iter().filter_map(|(spec, argument)| {
            let (value, at) = argument.constant()?;
            Some((spec.with_value(value), at))
        });
        apart.collect()
    };
    pattern.with_row(&union, context, &crate::Apart::new(&each))
}

/// The union of the controls of a row, as a `set` of each control in turn
/// makes it: the keys in their order, with the later value of a key that
/// comes again. With it, the locations of each control, the last one
/// first. `None` when a control is no map of data that a union takes, so
/// that the caller sets each control in turn.
fn union_of(row: &[ChainCall<'_>]) -> Option<(Value, crate::Locations)> {
    let mut context = crate::Locations::new();
    let mut entries: Vec<(Key, Value)> = Vec::with_capacity(row.len());
    let mut set = |key: Key, value: Value| match entries.iter_mut().find(|(name, _)| *name == key) {
        Some((_, current)) => *current = value,
        None => entries.push((key, value)),
    };
    for (spec, argument) in row {
        let (value, mut at) = argument.constant()?;
        // The locations of a later control come first.
        if !at.is_empty() {
            at.append(&context);
            context = at;
        }
        // A number, a text or a flag is the one entry of its control, with
        // no map of that entry. A map of `value` alone leaves the other
        // side of a union as it is.
        let plain = matches!(value, Value::F64(_) | Value::Str(_) | Value::Bool(_));
        if plain && spec.name() != "value" {
            set(spec.key(), value);
            continue;
        }
        let named = spec.with_value(value);
        let controls = named.as_object().filter(|_| named.is_data(0))?;
        if crate::compose::union_drops(controls) {
            return None;
        }
        for (key, value) in controls {
            // A handle of the key and of the value.
            set(key.clone(), value.clone());
        }
    }
    let mut union = OrderedMap::of_keys(entries);
    union.mark_made_of_data();
    Some((Value::Object(union), context))
}

/// One control of a chain, applied as its method applies it.
fn apply_argument(spec: &ControlSpec, pattern: Pattern, argument: &ChainArgument) -> Pattern {
    match argument {
        ChainArgument::Number(number) => spec.apply_value(pattern, Value::F64(*number)),
        ChainArgument::Pattern(values) => spec.apply_for(pattern, Some(values)),
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
        .apply_for(pattern.clone(), Some(value))
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
                    out.insert_key(key.clone(), value.clone());
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

/// The complete pinned control surface, installed from the names and aliases
/// of the catalog table. The registry reads the names-only table and holds no
/// reference text, so a build which never shows the text does not link the
/// text.
pub fn default_control_registry() -> &'static ControlRegistry {
    static REGISTRY: std::sync::LazyLock<ControlRegistry> = std::sync::LazyLock::new(|| {
        let mut registry = ControlRegistry::new();
        for control in crate::controls_generated::CONTROL_NAMES.iter() {
            registry.register(
                control.names.iter().copied(),
                control.aliases.iter().copied(),
            );
        }
        registry
    });
    &REGISTRY
}

/// The per-hap maps of the control applications, interned by what each
/// reads.
///
/// A map is a pure function of its spec and its unnamed-only flag, so the
/// same control built by a later evaluation is the same closure object. The
/// node that holds it then agrees with its counterpart by pointer, and the
/// answers of the unchanged subtree carry across evaluations and edits
/// instead of rebuilding. See [`crate::interned_closure`].
#[derive(Default)]
struct ControlMaps {
    maps: std::collections::HashMap<(ControlSpec, bool), crate::OwnedHapMapFn>,
    /// The name of each map of every value, by the address of the map,
    /// which the pool keeps for the life of the program.
    names: std::collections::HashMap<usize, Key>,
}

static INTERNED_CONTROL_MAPS: std::sync::OnceLock<std::sync::Mutex<ControlMaps>> =
    std::sync::OnceLock::new();

/// The address of `map`, which tells one interned map from another.
fn address(map: &crate::OwnedHapMapFn) -> usize {
    Arc::as_ptr(map).cast::<()>() as usize
}

pub(crate) fn interned_control_map(spec: &ControlSpec, unnamed_only: bool) -> crate::OwnedHapMapFn {
    let kept = spec.maps[usize::from(unnamed_only)].get_or_init(|| {
        // The pool and the map hold the names alone: a spec with its maps
        // would hold the maps that hold it.
        let names = || ControlSpec::new(spec.names.iter().cloned());
        let make = || -> crate::OwnedHapMapFn { Arc::new(control_map(names(), unnamed_only)) };
        let Ok(mut pool) = INTERNED_CONTROL_MAPS.get_or_init(Default::default).lock() else {
            return make();
        };
        if let Some(found) = pool.maps.get(&(names(), unnamed_only)) {
            return Arc::clone(found);
        }
        let made = make();
        // A full pool takes no map: a fresh one only loses the carry.
        if pool.maps.len() < crate::MAX_INTERNED_CLOSURES {
            if !unnamed_only {
                pool.names.insert(address(&made), spec.key());
            }
            pool.maps.insert((names(), unnamed_only), Arc::clone(&made));
        }
        made
    });
    Arc::clone(kept)
}

/// The name of the control whose map of each value `map` is: that map
/// names a number `{name: number}`, also for a control of several names,
/// which reads the other names from a list. `None` for any other map of
/// haps.
pub(crate) fn named_by(map: &crate::OwnedHapMapFn) -> Option<Key> {
    let pool = INTERNED_CONTROL_MAPS.get()?.lock().ok()?;
    pool.names.get(&address(map)).cloned()
}

/// The place of the live binding of the control `name` in
/// [`crate::Hap::live_controls`], for a control that a slider can drive.
pub(crate) fn live_slot(name: &str) -> Option<usize> {
    match name {
        "gain" => Some(0),
        "cutoff" => Some(1),
        "resonance" => Some(2),
        "fmi" => Some(3),
        _ => None,
    }
}

/// The body of one interned control map. See [`interned_control_map`].
fn control_map(spec: ControlSpec, unnamed_only: bool) -> impl Fn(Hap) -> Option<Hap> + Send + Sync {
    move |hap| {
        let already_controls = unnamed_only
            && match &hap.value {
                Value::Object(object) => !object.contains_key("value"),
                Value::JsValue(reference) if !reference.is_array() => matches!(
                    crate::materialize_js_value(&hap.value),
                    Value::Object(object) if !object.contains_key("value")
                ),
                _ => false,
            };
        if already_controls {
            return Some(hap);
        }
        let slider_binding = hap.slider_binding;
        let scalar = matches!(hap.value, Value::F64(_));
        let mut mapped = hap.map_value(|value| spec.with_value(value.clone()));
        if slider_binding != 0
            && scalar
            && let Some(slot) = live_slot(spec.name())
        {
            mapped.live_controls[slot] = slider_binding;
        }
        Some(mapped)
    }
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
    fn the_registry_holds_every_control_of_the_table() {
        let registry = default_control_registry();
        let rows = crate::controls_generated::CONTROLS;
        assert_eq!(registry.len(), rows.len());
        for row in rows {
            for name in row.names.iter().take(1).chain(row.aliases) {
                assert!(registry.get(name).is_some(), "`{name}` is not installed");
            }
        }
    }

    #[test]
    fn a_chain_transformer_is_native_for_pure_patterns_alone() {
        let gain: &'static ControlSpec = Box::leak(Box::new(ControlSpec::new(["gain"])));
        let script = pure(Value::Function(Box::new(crate::value::FunctionRef::js(9))));
        assert!(!script.is_pure());
        let chains = [
            (vec![(gain, ChainArgument::Number(0.5))], true),
            (
                vec![(gain, ChainArgument::Pattern(pure(Value::F64(0.5))))],
                true,
            ),
            (
                vec![
                    (gain, ChainArgument::Number(0.5)),
                    (gain, ChainArgument::Pattern(script)),
                ],
                false,
            ),
        ];
        for (chain, native) in chains {
            assert_eq!(chain_transformer("gain", chain).is_native(), native);
        }
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
