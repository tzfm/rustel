use super::{BuiltinDedupFn, KeysDedupFn};
use crate::compose::{ComposeOp, compose_data_in_place, union_drops};
use crate::context_keys::ContextKeys;
use crate::{Locations, Value};
use std::sync::Arc;

/// Pack a compact `(begin, end)` source range into a single non-zero `u32`:
/// `begin` in the high 20 bits and `length + 1` in the low 12 bits, leaving
/// `0` as the empty sentinel.
#[inline]
fn pack_dress_loc((begin, end): (usize, usize)) -> Option<u32> {
    const LEN_BITS: u32 = 12;
    const MAX_LEN: u32 = (1 << LEN_BITS) - 2;
    const MAX_BEGIN: u32 = (1 << (32 - LEN_BITS)) - 1;
    let length = u32::try_from(end.checked_sub(begin)?).ok()?;
    let begin = u32::try_from(begin).ok()?;
    (begin <= MAX_BEGIN && length <= MAX_LEN).then_some((begin << LEN_BITS) | (length + 1))
}

#[inline]
pub(super) fn unpack_dress_loc(packed: u32) -> (usize, usize) {
    const LEN_BITS: u32 = 12;
    const LEN_MASK: u32 = (1 << LEN_BITS) - 1;
    let begin = (packed >> LEN_BITS) as usize;
    let length = ((packed & LEN_MASK) - 1) as usize;
    (begin, begin + length)
}

#[inline]
fn is_internable_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::F64(_) | Value::Str(_) | Value::Bool(_) | Value::Null | Value::Undefined
    )
}

#[inline]
fn hash_intern_scalar(v: &Value, state: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    match v {
        Value::F64(x) => {
            1u8.hash(state);
            x.to_bits().hash(state);
        }
        Value::Str(s) => {
            2u8.hash(state);
            s.as_str().hash(state);
        }
        Value::Bool(b) => {
            3u8.hash(state);
            b.hash(state);
        }
        Value::Null => 4u8.hash(state),
        Value::Undefined => 5u8.hash(state),
        _ => 0u8.hash(state),
    }
}

#[inline]
fn eq_intern_scalar(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F64(x), Value::F64(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => x.as_str() == y.as_str(),
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) | (Value::Undefined, Value::Undefined) => true,
        _ => false,
    }
}

/// Inline built-in control map storage for [`Dress`]: stores single-key
/// controls inline without allocating an `Arc<BuiltinMap<1>>`, upgrading to a
/// shared `Arc<dyn BuiltinEntries>` with precomputed bloom marks for 2+ keys.
#[derive(Clone, Default)]
pub(super) enum DressSet {
    #[default]
    None,
    Single(u16, Value),
    Builtin(Arc<dyn crate::value::BuiltinEntries>, u64),
}

/// Rare tail fields of [`Dress`]: spilled leading locations, trailing locations,
/// custom control names, and non-map or empty-map set values.
#[derive(Clone, Default)]
pub(super) struct DressTail {
    /// Leading locations when more than two ranges or a wide range are present.
    pub(super) before: Locations,
    /// The locations of each hap after its step's own.
    pub(super) after: Locations,
    /// Custom (non-built-in) control names when `Layer::named_id == 0`.
    pub(super) custom_named: Option<Arc<[Arc<str>]>>,
    /// Rare non-object or empty-object `Dress::set` value.
    pub(super) set_other: Option<Value>,
}

/// What the folds on a layer give each hap of it, beyond its step. The
/// layers that a fold dresses alike share one: see [`super::redressed`].
#[derive(Clone, Default)]
pub(crate) struct Dress {
    /// Undressed non-empty built-in control map entries set on each value when
    /// read. Single-key controls live inline; 2+ keys share a `BuiltinEntries`
    /// allocation with precomputed bloom marks. Rare custom-key, >24-key,
    /// empty-map, or non-object set values live in `tail.set_other`.
    pub(super) set: DressSet,
    /// The keys of the context of each hap (for example, `scale` or visual
    /// slots), kept inline so scaled and visualized layers avoid `DressTail`.
    pub(super) keys: ContextKeys,
    /// Up to two compact leading locations packed inline; `[0, 0]` when empty
    /// or spilled into `tail.before`.
    pub(super) before_packed: [u32; 2],
    /// Rare spilled leading locations, trailing locations, custom names, and
    /// non-builtin or non-map set values.
    pub(super) tail: Option<Box<DressTail>>,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Dress>() == 56);

static EMPTY_LOCATIONS: Locations = Locations::new();

/// The dress of a layer that no fold dressed.
pub(super) static BARE: Dress = Dress {
    set: DressSet::None,
    keys: ContextKeys::NONE,
    before_packed: [0, 0],
    tail: None,
};

impl Dress {
    #[inline]
    pub(super) fn before_is_empty(&self) -> bool {
        self.before_packed[0] == 0
            && self
                .tail
                .as_deref()
                .is_none_or(|tail| tail.before.is_empty())
    }

    #[inline]
    pub(super) fn before_len(&self) -> usize {
        if self.before_packed[0] != 0 {
            1 + usize::from(self.before_packed[1] != 0)
        } else {
            self.tail.as_deref().map_or(0, |tail| tail.before.len())
        }
    }

    pub(super) fn before_locations(&self) -> Locations {
        if self.before_packed[0] != 0 {
            let mut out = Locations::new();
            out.push(unpack_dress_loc(self.before_packed[0]));
            if self.before_packed[1] != 0 {
                out.push(unpack_dress_loc(self.before_packed[1]));
            }
            out
        } else {
            self.tail
                .as_deref()
                .map_or_else(Locations::new, |tail| tail.before.clone())
        }
    }

    pub(super) fn prepend_before(&mut self, prefix: Locations) {
        if prefix.is_empty() {
            return;
        }
        let tail_empty = self
            .tail
            .as_deref()
            .is_none_or(|tail| tail.before.is_empty());
        if tail_empty {
            match prefix.len() {
                1 if self.before_packed[1] == 0 => {
                    if let Some(range) = prefix.get(0)
                        && let Some(p) = pack_dress_loc(range)
                    {
                        self.before_packed = [p, self.before_packed[0]];
                        return;
                    }
                }
                2 if self.before_packed[0] == 0 => {
                    if let (Some(r0), Some(r1)) = (prefix.get(0), prefix.get(1))
                        && let (Some(p0), Some(p1)) = (pack_dress_loc(r0), pack_dress_loc(r1))
                    {
                        self.before_packed = [p0, p1];
                        return;
                    }
                }
                _ => {}
            }
        }
        let mut full = prefix;
        full.append(&self.before_locations());
        self.before_packed = [0, 0];
        self.tail.get_or_insert_with(Default::default).before = full;
    }

    pub(super) fn clear_before(&mut self) {
        self.before_packed = [0, 0];
        if let Some(tail) = &mut self.tail {
            tail.before = Locations::new();
            self.prune_tail();
        }
    }

    pub(super) fn same_before(&self, other: &Self) -> bool {
        let a_spilled = self
            .tail
            .as_deref()
            .is_some_and(|tail| !tail.before.is_empty());
        let b_spilled = other
            .tail
            .as_deref()
            .is_some_and(|tail| !tail.before.is_empty());
        if !a_spilled && !b_spilled {
            self.before_packed == other.before_packed
        } else {
            self.before_locations() == other.before_locations()
        }
    }

    #[inline]
    pub(super) fn after(&self) -> &Locations {
        self.tail
            .as_deref()
            .map_or(&EMPTY_LOCATIONS, |tail| &tail.after)
    }

    #[inline]
    pub(super) fn keys(&self) -> &ContextKeys {
        &self.keys
    }

    #[inline]
    pub(super) fn has_custom_name(&self) -> bool {
        self.tail
            .as_deref()
            .is_some_and(|tail| tail.custom_named.is_some())
    }

    #[inline]
    pub(super) fn custom_named(&self) -> Option<&[Arc<str>]> {
        self.tail
            .as_deref()
            .and_then(|tail| tail.custom_named.as_deref())
    }

    pub(super) fn set_custom_named(&mut self, names: &Arc<[Arc<str>]>) {
        self.tail.get_or_insert_with(Default::default).custom_named = Some(Arc::clone(names));
    }

    pub(super) fn clear_custom_named(&mut self) {
        if let Some(tail) = &mut self.tail
            && tail.custom_named.is_some()
        {
            tail.custom_named = None;
            self.prune_tail();
        }
    }

    fn prune_tail(&mut self) {
        if self.tail.as_deref().is_some_and(|tail| {
            tail.before.is_empty()
                && tail.after.is_empty()
                && tail.custom_named.is_none()
                && tail.set_other.is_none()
        }) {
            self.tail = None;
        }
    }

    #[inline(always)]
    pub(super) fn has_set_map(&self) -> bool {
        !matches!(self.set, DressSet::None)
    }

    #[inline]
    pub(super) fn has_set(&self) -> bool {
        self.has_set_map()
            || self
                .tail
                .as_deref()
                .is_some_and(|tail| tail.set_other.is_some())
    }

    #[inline(always)]
    pub(super) fn set_marks(&self) -> u64 {
        match &self.set {
            DressSet::None => 0,
            DressSet::Single(k, _) => 1u64 << (*k >> 10),
            DressSet::Builtin(_, marks) => *marks,
        }
    }

    #[inline(always)]
    pub(super) fn set_packed_keys(&self) -> &[u16] {
        match &self.set {
            DressSet::None => &[],
            DressSet::Single(k, _) => std::slice::from_ref(k),
            DressSet::Builtin(entries, _) => entries.keys(),
        }
    }

    #[inline(always)]
    pub(super) fn set_len(&self) -> usize {
        match &self.set {
            DressSet::None => 0,
            DressSet::Single(..) => 1,
            DressSet::Builtin(entries, _) => entries.keys().len(),
        }
    }

    #[inline(always)]
    pub(super) fn set_slice(&self) -> crate::value::Slice<'_> {
        match &self.set {
            DressSet::None => crate::value::Slice::EMPTY,
            DressSet::Single(k, v) => {
                crate::value::Slice::Builtin(std::slice::from_ref(k), std::slice::from_ref(v))
            }
            DressSet::Builtin(entries, _) => entries.as_slice(),
        }
    }

    #[inline]
    pub(super) fn same_set_keys(&self, other: &Self) -> bool {
        match (&self.set, &other.set) {
            (DressSet::None, DressSet::None) => true,
            (DressSet::Single(a, _), DressSet::Single(b, _)) => a == b,
            (DressSet::Builtin(ma, _), DressSet::Builtin(mb, _)) => {
                Arc::ptr_eq(ma, mb) || ma.keys() == mb.keys()
            }
            (DressSet::Single(k, _), DressSet::Builtin(m, _))
            | (DressSet::Builtin(m, _), DressSet::Single(k, _)) => m.keys() == [*k],
            _ => false,
        }
    }

    fn set_map_builtin(&mut self, entries: Arc<dyn crate::value::BuiltinEntries>) {
        let marks = entries.as_slice().marks();
        self.set = DressSet::Builtin(entries, marks);
    }

    #[inline]
    pub(super) fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    #[inline]
    pub(crate) fn can_intern(&self) -> bool {
        self.tail.is_none()
            && self.keys.can_intern()
            && match &self.set {
                DressSet::None | DressSet::Builtin(..) => true,
                DressSet::Single(_, v) => is_internable_scalar(v),
            }
    }

    #[inline]
    pub(crate) fn can_alias(&self) -> bool {
        match &self.set {
            DressSet::None | DressSet::Single(..) => true,
            DressSet::Builtin(entries, _) => Arc::strong_count(entries) > 1,
        }
    }

    pub(crate) fn dedup_inner_mut(
        &mut self,
        dedup_builtin: &mut BuiltinDedupFn<'_>,
        dedup_keys: &mut KeysDedupFn<'_>,
    ) {
        if let DressSet::Builtin(map, _) = &mut self.set
            && let Some(canon) = dedup_builtin(map, true)
            && !Arc::ptr_eq(map, &canon)
        {
            *map = canon;
        }
        if let Some(keys) = self.keys.as_arc()
            && let Some(canon) = dedup_keys(keys)
            && !Arc::ptr_eq(keys, &canon)
        {
            self.keys = ContextKeys::from_arc(canon);
        }
    }

    pub(crate) fn dedup_inner(
        &self,
        dedup_builtin: &mut BuiltinDedupFn<'_>,
        dedup_keys: &mut KeysDedupFn<'_>,
    ) -> Option<Self> {
        let new_map = match &self.set {
            DressSet::Builtin(map, _) => dedup_builtin(map, true).filter(|c| !Arc::ptr_eq(map, c)),
            DressSet::None | DressSet::Single(..) => None,
        };
        let new_keys = self.keys.as_arc().and_then(|keys| {
            dedup_keys(keys)
                .filter(|c| !Arc::ptr_eq(keys, c))
                .map(ContextKeys::from_arc)
        });
        if new_map.is_none() && new_keys.is_none() {
            return None;
        }
        let mut cloned = self.clone();
        if let Some(map) = new_map {
            let marks = map.as_slice().marks();
            cloned.set = DressSet::Builtin(map, marks);
        }
        if let Some(keys) = new_keys {
            cloned.keys = keys;
        }
        Some(cloned)
    }

    pub(crate) fn hash_intern(&self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        match &self.set {
            DressSet::None => {
                0u8.hash(state);
            }
            DressSet::Single(k, v) => {
                1u8.hash(state);
                k.hash(state);
                hash_intern_scalar(v, state);
            }
            DressSet::Builtin(m, marks) => {
                2u8.hash(state);
                (Arc::as_ptr(m) as *const () as usize).hash(state);
                marks.hash(state);
            }
        }
        self.keys.arc_ptr().hash(state);
        self.before_packed.hash(state);
    }

    pub(crate) fn eq_intern(&self, other: &Self) -> bool {
        self.tail.is_none()
            && other.tail.is_none()
            && self.before_packed == other.before_packed
            && self.keys.arc_ptr() == other.keys.arc_ptr()
            && match (&self.set, &other.set) {
                (DressSet::None, DressSet::None) => true,
                (DressSet::Single(ka, va), DressSet::Single(kb, vb)) => {
                    ka == kb && eq_intern_scalar(va, vb)
                }
                (DressSet::Builtin(a, ma), DressSet::Builtin(b, mb)) => {
                    ma == mb && Arc::ptr_eq(a, b)
                }
                _ => false,
            }
    }

    pub(super) fn clear_set(&mut self) {
        self.set = DressSet::None;
        if let Some(tail) = &mut self.tail {
            tail.set_other = None;
            self.prune_tail();
        }
    }

    pub(super) fn assign_set(&mut self, value: Option<Value>) {
        self.set = DressSet::None;
        match value {
            None => {
                if let Some(tail) = &mut self.tail {
                    tail.set_other = None;
                    self.prune_tail();
                }
            }
            Some(Value::Object(map)) => {
                let map = map.undressed();
                if let Some((pk, val)) = map.single_packed_entry() {
                    self.set = DressSet::Single(pk, val.clone());
                    if let Some(tail) = &mut self.tail {
                        tail.set_other = None;
                        self.prune_tail();
                    }
                } else if let Some(entries) = map.as_builtin() {
                    self.set_map_builtin(Arc::clone(entries));
                    if let Some(tail) = &mut self.tail {
                        tail.set_other = None;
                        self.prune_tail();
                    }
                } else {
                    self.tail.get_or_insert_with(Default::default).set_other =
                        Some(Value::Object(map));
                }
            }
            Some(other) => {
                self.tail.get_or_insert_with(Default::default).set_other = Some(other);
            }
        }
    }

    pub(super) fn set_value(&self) -> Option<Value> {
        match &self.set {
            DressSet::Single(k, v) => Some(Value::Object(
                crate::value::OrderedMap::from_single_packed(*k, v.clone()),
            )),
            DressSet::Builtin(entries, _) => Some(Value::Object(
                crate::value::OrderedMap::from_made_builtin(Arc::clone(entries)),
            )),
            DressSet::None => self.tail.as_deref().and_then(|tail| tail.set_other.clone()),
        }
    }

    pub(super) fn compose_set_into(&self, target: &mut Value) {
        match &self.set {
            DressSet::Single(k, v) => {
                let other = crate::value::OrderedMap::from_single_packed(*k, v.clone());
                if let Value::Object(map) = target
                    && !union_drops(&other)
                {
                    map.merge(&other, |_, value| value.clone());
                    map.mark_made_of_data();
                    return;
                }
                compose_data_in_place(ComposeOp::Set, target, &Value::Object(other));
            }
            DressSet::Builtin(entries, _) => {
                let other = crate::value::OrderedMap::from_made_builtin(Arc::clone(entries));
                if let Value::Object(map) = target
                    && !union_drops(&other)
                {
                    map.merge(&other, |_, value| value.clone());
                    map.mark_made_of_data();
                    return;
                }
                compose_data_in_place(ComposeOp::Set, target, &Value::Object(other));
            }
            DressSet::None => {
                if let Some(other) = self.tail.as_deref().and_then(|t| t.set_other.as_ref()) {
                    compose_data_in_place(ComposeOp::Set, target, other);
                }
            }
        }
    }

    pub(super) fn set_keys(&mut self, keys: ContextKeys) {
        self.keys = keys;
    }

    pub(super) fn add_ui_visual_slot(&mut self, slot: u8) {
        if slot >= 64 {
            return;
        }
        self.keys.add_ui_visual_slot(slot);
    }

    pub(super) fn clear_after(&mut self) {
        if let Some(tail) = &mut self.tail {
            tail.after = Locations::new();
            self.prune_tail();
        }
    }

    pub(super) fn append_after(&mut self, locations: &Locations) {
        if !locations.is_empty() {
            self.tail
                .get_or_insert_with(Default::default)
                .after
                .append(locations);
        }
    }

    pub(super) fn extend_after(&mut self, locations: &[(usize, usize)]) {
        if !locations.is_empty() {
            self.tail
                .get_or_insert_with(Default::default)
                .after
                .extend_from_slice(locations);
        }
    }

    /// Set `control`, a map of controls at `context`, on each value that
    /// this dress makes. See [`super::Layer::set_here`].
    pub(super) fn set_single_here(&mut self, key: &crate::Key, value: Value) {
        let packed = crate::controls::pack_builtin_key_str(key.as_str());
        match std::mem::replace(&mut self.set, DressSet::None) {
            DressSet::Single(k0, v0) => {
                if packed != 0 && value.is_data(0) && key.as_str() != "value" {
                    if packed == k0 {
                        self.set = DressSet::Single(k0, value);
                    } else {
                        let mut vals = [v0, value];
                        self.set_map_builtin(crate::value::make_builtin_arc_owned(
                            &[k0, packed],
                            &mut vals,
                        ));
                    }
                    return;
                }
                let mut map = crate::value::OrderedMap::from_single_packed(k0, v0);
                map.set_data(key, value);
                map.mark_made_of_data();
                if let Some((pk, val)) = map.single_packed_entry() {
                    self.set = DressSet::Single(pk, val.clone());
                } else if let Some(new_entries) = map.as_builtin() {
                    self.set_map_builtin(Arc::clone(new_entries));
                } else {
                    self.tail.get_or_insert_with(Default::default).set_other =
                        Some(Value::Object(map));
                }
                return;
            }
            DressSet::Builtin(entries, _) => {
                let mut map = crate::value::OrderedMap::from_made_builtin(entries);
                map.set_data(key, value);
                map.mark_made_of_data();
                if let Some((pk, val)) = map.single_packed_entry() {
                    self.set = DressSet::Single(pk, val.clone());
                } else if let Some(new_entries) = map.as_builtin() {
                    self.set_map_builtin(Arc::clone(new_entries));
                } else {
                    self.tail.get_or_insert_with(Default::default).set_other =
                        Some(Value::Object(map));
                }
                return;
            }
            DressSet::None => {}
        }
        if let Some(Value::Object(map)) = self
            .tail
            .as_deref_mut()
            .and_then(|tail| tail.set_other.as_mut())
            && !crate::compose::union_drops(map)
        {
            map.set_data(key, value);
            map.mark_made_of_data();
            return;
        }
        if packed != 0
            && value.is_data(0)
            && key.as_str() != "value"
            && self
                .tail
                .as_deref()
                .is_none_or(|tail| tail.set_other.is_none())
        {
            self.set = DressSet::Single(packed, value);
            return;
        }
        let mut map = crate::value::OrderedMap::of_one(key.clone(), value);
        map.mark_made_of_data();
        if let Some(set_other) = self
            .tail
            .as_deref_mut()
            .and_then(|tail| tail.set_other.take())
        {
            let mut held = set_other;
            compose_data_in_place(ComposeOp::Set, &mut held, &Value::Object(map));
            self.assign_set(Some(held));
            return;
        }
        self.assign_set(Some(Value::Object(map)));
    }

    pub(super) fn set_packed_here(
        &mut self,
        keys: &[u16],
        vals: &[Value],
        context: Locations,
        shared: &mut Option<Arc<dyn crate::value::BuiltinEntries>>,
    ) {
        self.prepend_before(context);
        match std::mem::replace(&mut self.set, DressSet::None) {
            DressSet::Single(k0, v0) => {
                if keys.len() == 1 && keys[0] == k0 {
                    self.set = DressSet::Single(k0, vals[0].clone());
                    return;
                }
                match crate::value::OrderedMap::from_single_and_packed(k0, v0, keys, vals) {
                    Ok(map) => {
                        if let Some((pk, val)) = map.single_packed_entry() {
                            self.set = DressSet::Single(pk, val.clone());
                        } else if let Some(entries) = map.as_builtin() {
                            self.set_map_builtin(Arc::clone(entries));
                        } else {
                            self.tail.get_or_insert_with(Default::default).set_other =
                                Some(Value::Object(map));
                        }
                    }
                    Err(v0) => {
                        let mut map = crate::value::OrderedMap::from_single_packed(k0, v0);
                        let other_arc = shared.get_or_insert_with(|| {
                            crate::value::make_builtin_arc_cloned(keys, vals)
                        });
                        let other =
                            crate::value::OrderedMap::from_made_builtin(Arc::clone(other_arc));
                        map.merge(&other, |_, value| value.clone());
                        map.mark_made_of_data();
                        if let Some(new_entries) = map.as_builtin() {
                            self.set_map_builtin(Arc::clone(new_entries));
                        } else {
                            self.tail.get_or_insert_with(Default::default).set_other =
                                Some(Value::Object(map));
                        }
                    }
                }
                return;
            }
            DressSet::Builtin(entries, _) => {
                match crate::value::merge_builtin_with_packed(entries, keys, vals) {
                    Ok(merged) => {
                        self.set_map_builtin(merged);
                        return;
                    }
                    Err(entries) => {
                        let mut map = crate::value::OrderedMap::from_made_builtin(entries);
                        let other_arc = shared.get_or_insert_with(|| {
                            crate::value::make_builtin_arc_cloned(keys, vals)
                        });
                        let other =
                            crate::value::OrderedMap::from_made_builtin(Arc::clone(other_arc));
                        map.merge(&other, |_, value| value.clone());
                        map.mark_made_of_data();
                        if let Some(new_entries) = map.as_builtin() {
                            self.set_map_builtin(Arc::clone(new_entries));
                        } else {
                            self.tail.get_or_insert_with(Default::default).set_other =
                                Some(Value::Object(map));
                        }
                        return;
                    }
                }
            }
            DressSet::None => {}
        }
        if let Some(set_other) = self
            .tail
            .as_deref_mut()
            .and_then(|tail| tail.set_other.take())
        {
            let other_arc =
                shared.get_or_insert_with(|| crate::value::make_builtin_arc_cloned(keys, vals));
            let other = Value::Object(crate::value::OrderedMap::from_made_builtin(Arc::clone(
                other_arc,
            )));
            let mut value = set_other;
            compose_data_in_place(ComposeOp::Set, &mut value, &other);
            self.assign_set(Some(value));
            return;
        }
        if keys.len() == 1 {
            self.set = DressSet::Single(keys[0], vals[0].clone());
            return;
        }
        let arc = shared.get_or_insert_with(|| crate::value::make_builtin_arc_cloned(keys, vals));
        self.set_map_builtin(Arc::clone(arc));
    }

    pub(super) fn set_packed_here_owned(
        &mut self,
        keys: &[u16],
        vals: &mut [Value],
        context: Locations,
    ) {
        self.prepend_before(context);
        match std::mem::replace(&mut self.set, DressSet::None) {
            DressSet::Single(k0, v0) => {
                if keys.len() == 1 && keys[0] == k0 {
                    let v = std::mem::replace(&mut vals[0], Value::Undefined);
                    self.set = DressSet::Single(k0, v);
                    return;
                }
                match crate::value::OrderedMap::from_single_and_packed(k0, v0, keys, vals) {
                    Ok(map) => {
                        if let Some((pk, val)) = map.single_packed_entry() {
                            self.set = DressSet::Single(pk, val.clone());
                        } else if let Some(entries) = map.as_builtin() {
                            self.set_map_builtin(Arc::clone(entries));
                        } else {
                            self.tail.get_or_insert_with(Default::default).set_other =
                                Some(Value::Object(map));
                        }
                    }
                    Err(v0) => {
                        let mut map = crate::value::OrderedMap::from_single_packed(k0, v0);
                        let other_arc = crate::value::make_builtin_arc_owned(keys, vals);
                        let other = crate::value::OrderedMap::from_made_builtin(other_arc);
                        map.merge(&other, |_, value| value.clone());
                        map.mark_made_of_data();
                        if let Some(new_entries) = map.as_builtin() {
                            self.set_map_builtin(Arc::clone(new_entries));
                        } else {
                            self.tail.get_or_insert_with(Default::default).set_other =
                                Some(Value::Object(map));
                        }
                    }
                }
                return;
            }
            DressSet::Builtin(entries, _) => {
                match crate::value::merge_builtin_with_packed_owned(entries, keys, vals) {
                    Ok(merged) => {
                        self.set_map_builtin(merged);
                        return;
                    }
                    Err(entries) => {
                        let mut map = crate::value::OrderedMap::from_made_builtin(entries);
                        let other_arc = crate::value::make_builtin_arc_owned(keys, vals);
                        let other = crate::value::OrderedMap::from_made_builtin(other_arc);
                        map.merge(&other, |_, value| value.clone());
                        map.mark_made_of_data();
                        if let Some(new_entries) = map.as_builtin() {
                            self.set_map_builtin(Arc::clone(new_entries));
                        } else {
                            self.tail.get_or_insert_with(Default::default).set_other =
                                Some(Value::Object(map));
                        }
                        return;
                    }
                }
            }
            DressSet::None => {}
        }
        if let Some(set_other) = self
            .tail
            .as_deref_mut()
            .and_then(|tail| tail.set_other.take())
        {
            let other_arc = crate::value::make_builtin_arc_owned(keys, vals);
            let other = Value::Object(crate::value::OrderedMap::from_made_builtin(other_arc));
            let mut value = set_other;
            compose_data_in_place(ComposeOp::Set, &mut value, &other);
            self.assign_set(Some(value));
            return;
        }
        if keys.len() == 1 {
            let v = std::mem::replace(&mut vals[0], Value::Undefined);
            self.set = DressSet::Single(keys[0], v);
            return;
        }
        let arc = crate::value::make_builtin_arc_owned(keys, vals);
        self.set_map_builtin(arc);
    }

    pub(super) fn set_here(&mut self, control: &Value, context: Locations) {
        // The value side's locations come first.
        self.prepend_before(context);
        match std::mem::replace(&mut self.set, DressSet::None) {
            DressSet::Single(k0, v0) => {
                let mut map = crate::value::OrderedMap::from_single_packed(k0, v0);
                if let Value::Object(other) = control
                    && !union_drops(other)
                {
                    map.merge(other, |_, value| value.clone());
                    map.mark_made_of_data();
                    if let Some((pk, val)) = map.single_packed_entry() {
                        self.set = DressSet::Single(pk, val.clone());
                    } else if let Some(new_entries) = map.as_builtin() {
                        self.set_map_builtin(Arc::clone(new_entries));
                    } else {
                        self.tail.get_or_insert_with(Default::default).set_other =
                            Some(Value::Object(map));
                    }
                    return;
                }
                let mut value = Value::Object(map);
                compose_data_in_place(ComposeOp::Set, &mut value, control);
                self.assign_set(Some(value));
                return;
            }
            DressSet::Builtin(entries, _) => {
                let mut map = crate::value::OrderedMap::from_made_builtin(entries);
                if let Value::Object(other) = control
                    && !union_drops(other)
                {
                    map.merge(other, |_, value| value.clone());
                    map.mark_made_of_data();
                    if let Some((pk, val)) = map.single_packed_entry() {
                        self.set = DressSet::Single(pk, val.clone());
                    } else if let Some(new_entries) = map.as_builtin() {
                        self.set_map_builtin(Arc::clone(new_entries));
                    } else {
                        self.tail.get_or_insert_with(Default::default).set_other =
                            Some(Value::Object(map));
                    }
                    return;
                }
                let mut value = Value::Object(map);
                compose_data_in_place(ComposeOp::Set, &mut value, control);
                self.assign_set(Some(value));
                return;
            }
            DressSet::None => {}
        }
        if let Some(set_other) = self
            .tail
            .as_deref_mut()
            .and_then(|tail| tail.set_other.take())
        {
            let mut value = set_other;
            compose_data_in_place(ComposeOp::Set, &mut value, control);
            self.assign_set(Some(value));
            return;
        }
        self.assign_set(Some(control.clone()));
    }

    pub(crate) fn for_bare_renote(keys: &ContextKeys) -> Option<Arc<Self>> {
        let mut dress = Self::default();
        if crate::controls::note_control_id() == 0 {
            dress.set_custom_named(&crate::controls::note_names());
        }
        dress.set_keys(keys.clone());
        (!dress.is_bare()).then(|| dress.into_arc())
    }

    /// Whether the dress gives a hap nothing.
    pub(super) fn is_bare(&self) -> bool {
        !self.has_custom_name()
            && !self.has_set()
            && self.before_is_empty()
            && self.keys.is_empty()
            && self.after().is_empty()
    }

    /// Whether a hap of a step at `locations` holds them alone.
    pub(super) fn locates_alone(&self) -> bool {
        self.before_is_empty() && self.after().is_empty()
    }

    /// The locations of a hap of a step at `locations`: those before, the
    /// step's own, then those after.
    #[inline]
    pub(super) fn around(&self, locations: &Locations) -> Locations {
        let mut context = self.before_locations();
        context.append(locations);
        if let Some(tail) = &self.tail {
            context.append(&tail.after);
        }
        context
    }

    #[inline]
    pub(super) fn around_owned(&self, mut locations: Locations) -> Locations {
        if self.before_packed[0] != 0 {
            if locations.prepend_packed(self.before_packed[0], self.before_packed[1]) {
                if let Some(tail) = &self.tail
                    && !tail.after.is_empty()
                {
                    locations.append(&tail.after);
                }
                return locations;
            }
        } else if self
            .tail
            .as_deref()
            .is_none_or(|tail| tail.before.is_empty())
        {
            if let Some(tail) = &self.tail
                && !tail.after.is_empty()
            {
                locations.append(&tail.after);
            }
            return locations;
        }
        let mut context = self.before_locations();
        context.append(&locations);
        if let Some(tail) = &self.tail
            && !tail.after.is_empty()
        {
            context.append(&tail.after);
        }
        context
    }

    /// Whether two dresses give each hap the same, with set controls that
    /// `same_value` holds to be the same.
    pub(super) fn same(&self, other: &Self, same_value: impl Fn(&Value, &Value) -> bool) -> bool {
        let self_no_other = self.tail.as_deref().is_none_or(|t| t.set_other.is_none());
        let other_no_other = other.tail.as_deref().is_none_or(|t| t.set_other.is_none());
        let same_set = match (&self.set, &other.set) {
            (DressSet::Single(ka, va), DressSet::Single(kb, vb)) => ka == kb && same_value(va, vb),
            (DressSet::None, DressSet::None) if self_no_other && other_no_other => true,
            (DressSet::Single(..), DressSet::None) if other_no_other => false,
            (DressSet::None, DressSet::Single(..)) if self_no_other => false,
            (DressSet::Single(k, v), DressSet::Builtin(m, _))
            | (DressSet::Builtin(m, _), DressSet::Single(k, v)) => {
                m.keys() == [*k] && same_value(v, m.as_slice().value_at(0))
            }
            _ => match (self.set_value(), other.set_value()) {
                (Some(a), Some(b)) => same_value(&a, &b),
                (None, None) => true,
                _ => false,
            },
        };
        same_set
            && self.custom_named() == other.custom_named()
            && self.keys.same(&other.keys)
            && self.same_before(other)
            && self.after() == other.after()
    }
}
