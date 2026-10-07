use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, RwLock, Weak};

/// Mutable module settings for one native rustel runtime.
///
/// A process may host several independent runtimes. Settings and score
/// registrations belong to one runtime so sessions cannot affect each other.
#[derive(Clone, Debug, Default)]
pub struct RuntimeSettings {
    inner: Arc<RuntimeSettingsInner>,
}
#[derive(Debug, Default)]
struct RuntimeSettingsInner {
    /// Snapshots used by nested graphs keep this identity while owning a
    /// detached publication slot. Re-entering the same runtime during one
    /// operation therefore reuses the already selected state.
    identity: Arc<()>,
    /// All clones share this publication slot. A bound operation pins the
    /// selected state identity once, so replacement is atomic across the
    /// complete operation rather than field-by-field in its middle.
    state: RwLock<Arc<SettingsState>>,
}

#[derive(Debug, Default)]
struct SettingsState {
    /// A score override; absent means the host chooses its default.
    max_polyphony: Option<usize>,
    rng_mode: u8,
    default_join: u8,
    default_voicings: Option<VoicingSetting>,
    /// MIDI control maps a score registered with `midimaps`/`defaultmidimap`.
    /// The map under the name "default" is what `defaultmidimap` sets and what
    /// a hap with no `midimap` control reads.
    midi_maps: HashMap<String, Arc<HashMap<String, crate::midimap::MidiMapEntry>>>,
    /// Dictionaries a score registered with `addVoicings`/`registerVoicings`.
    ///
    /// Settings-scoped, not process-global, for the same reason
    /// `default_voicings` is: two Sessions must not see each other's
    /// registrations, and a fresh Session starts from the pinned dictionaries
    /// alone. Entries are `Arc` so a query can hold one without cloning the
    /// whole table.
    user_voicing_dicts: HashMap<String, Arc<UserVoicingDictionaries>>,
    /// `voicings()` needs only the parsed MIDI of its last emitted note across
    /// queries. `None` is fresh/reset history; an empty or unparseable result
    /// stores `Some(0)`. The cell is shared by snapshots from the same runtime,
    /// but a fresh runtime receives a fresh cell.
    legacy_voicing_top_note: Arc<Mutex<Option<i64>>>,
}

#[derive(Clone, Debug)]
struct VoicingSetting {
    name: String,
    _lease: Option<VoicingDictionaryLease>,
    /// A non-string default remains owned by QuickJS so later user mutation is
    /// observable. Query entry refreshes this parsed, bounded native view.
    live_dictionary: Option<Arc<RwLock<Option<VoicingDict>>>>,
}

/// A parsed voicing dictionary: chord symbol to its voicings, each a list of
/// semitone steps.
pub(crate) type VoicingDict = HashMap<String, Vec<Vec<f64>>>;

#[derive(Debug)]
pub(crate) struct UserVoicingDictionaries {
    pub(crate) semantic: Arc<VoicingDict>,
    pub(crate) legacy: Arc<crate::voicings::LegacyVoicingDict>,
}

/// Keeps host-owned state alive for as long as a settings snapshot selects it.
#[doc(hidden)]
#[derive(Clone)]
pub struct VoicingDictionaryLease {
    _inner: Arc<VoicingDictionaryLeaseInner>,
}

/// Identifies a selected host dictionary without keeping its realm alive.
#[doc(hidden)]
#[derive(Clone)]
pub struct VoicingDictionaryIdentity(Weak<VoicingDictionaryLeaseInner>);

impl PartialEq for VoicingDictionaryIdentity {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for VoicingDictionaryIdentity {}

struct VoicingDictionaryLeaseInner {
    retired: Arc<std::sync::atomic::AtomicBool>,
    any_retired: Arc<std::sync::atomic::AtomicBool>,
}

impl VoicingDictionaryLease {
    #[doc(hidden)]
    pub fn new(
        retired: Arc<std::sync::atomic::AtomicBool>,
        any_retired: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            _inner: Arc::new(VoicingDictionaryLeaseInner {
                retired,
                any_retired,
            }),
        }
    }
}

impl std::fmt::Debug for VoicingDictionaryLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VoicingDictionaryLease")
    }
}

impl Drop for VoicingDictionaryLeaseInner {
    fn drop(&mut self) {
        self.retired
            .store(true, std::sync::atomic::Ordering::Release);
        self.any_retired
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

struct BoundSettings {
    scope: u64,
    /// The handle whose scope was requested. When a detached candidate is
    /// already selected, a same-runtime exported graph inherits that selected
    /// state while metadata still keeps the published handle requested by the
    /// graph.
    requested: Arc<RuntimeSettingsInner>,
    owner: Arc<RuntimeSettingsInner>,
    selected: Arc<SettingsState>,
}

thread_local! {
    static CURRENT: RefCell<Vec<BoundSettings>> = const { RefCell::new(Vec::new()) };
    static NEXT_SCOPE: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// Preserve the historical module state for direct `rustel-core` callers.
/// `JsRuntime` binds its own `RuntimeSettings` at evaluation and query entry
/// points, so independent Sessions never read or mutate this fallback.
static FALLBACK: LazyLock<RuntimeSettings> = LazyLock::new(RuntimeSettings::default);

/// Restores the previously bound runtime settings on drop.
pub struct RuntimeSettingsScope {
    // The token both identifies this exact frame and keeps the scope on the
    // thread whose thread-local stack it entered.
    scope: u64,
    _not_send: PhantomData<Rc<()>>,
}

impl RuntimeSettings {
    /// Conservatively charge immutable native storage retained by a snapshot.
    ///
    /// Includes container capacities and immutable Arc-retained dictionaries,
    /// counting shared allocations again rather than deduplicating them: an
    /// older snapshot can become their sole owner after publication changes.
    ///
    /// Each snapshot also pins one shared, fixed-size legacy top-note cell,
    /// charged in full, and at most one shared host-dictionary view. Only the
    /// host view's fixed-size Arc/cell header is charged; its mutable heap
    /// contents and opaque host objects are NOT included, even if a snapshot
    /// becomes their sole owner. Those contents retain their existing
    /// runtime/parser/host limits, not this byte budget. This neither freezes
    /// shared state nor bounds native RSS or future growth.
    ///
    /// No data is cloned or materialized. Returns `None` on byte/work budget
    /// exhaustion, arithmetic overflow, or a busy/poisoned publication lock.
    /// Mutable cells are not locked or traversed. The walk visits at most
    /// 65,536 native entries, with fixed nesting depth.
    pub fn retained_snapshot_bytes(&self, limit: usize) -> Option<usize> {
        let state = self.inner.state.try_read().ok()?;
        let mut charge = NativeRetention::new(limit);
        charge.add(std::mem::size_of::<Self>())?;
        charge.arc::<RuntimeSettingsInner>()?;
        charge.arc::<()>()?;
        charge.arc::<SettingsState>()?;

        if let Some(selection) = &state.default_voicings {
            charge.string(&selection.name)?;
            if selection._lease.is_some() {
                charge.arc::<VoicingDictionaryLeaseInner>()?;
                charge.arc::<std::sync::atomic::AtomicBool>()?;
                charge.arc::<std::sync::atomic::AtomicBool>()?;
            }
            if selection.live_dictionary.is_some() {
                charge.arc::<RwLock<Option<VoicingDict>>>()?;
            }
        }

        charge.map(&state.midi_maps)?;
        for (name, entries) in &state.midi_maps {
            charge.string(name)?;
            charge.arc::<HashMap<String, crate::midimap::MidiMapEntry>>()?;
            charge.map(entries)?;
            for (key, entry) in entries.iter() {
                charge.string(key)?;
                charge.string(&entry.control)?;
            }
        }

        charge.map(&state.user_voicing_dicts)?;
        for (name, dictionaries) in &state.user_voicing_dicts {
            charge.string(name)?;
            charge.arc::<UserVoicingDictionaries>()?;
            charge.arc::<VoicingDict>()?;
            charge.voicing_dict(&dictionaries.semantic)?;
            charge.arc::<crate::voicings::LegacyVoicingDict>()?;
            let (dictionary, range) = dictionaries.legacy.retention_parts();
            charge.map(dictionary)?;
            for (symbol, voicings) in dictionary {
                charge.string(symbol)?;
                charge.vector(voicings)?;
                for voicing in voicings {
                    charge.strings(voicing)?;
                }
            }
            charge.vector(range)?;
        }

        charge.arc::<Mutex<Option<i64>>>()?;
        Some(charge.bytes)
    }

    pub(crate) fn same_handle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Request this handle for the dynamic extent of `f`.
    ///
    /// A nested handle from the same runtime keeps the snapshot already
    /// selected for the surrounding operation.
    pub fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        if self.is_current_requested() {
            return f();
        }
        let _scope = self.bind();
        f()
    }

    /// Request this handle until the returned scope is dropped.
    ///
    /// A same-runtime binding inherits the surrounding operation's selected
    /// snapshot while retaining the requested handle for exported metadata.
    pub fn bind(&self) -> RuntimeSettingsScope {
        let inherited = CURRENT.with(|current| {
            current
                .borrow()
                .iter()
                .rev()
                .find(|bound| Arc::ptr_eq(&bound.owner.identity, &self.inner.identity))
                .map(|bound| (Arc::clone(&bound.owner), Arc::clone(&bound.selected)))
        });
        let (owner, selected) = inherited.unwrap_or_else(|| {
            (
                Arc::clone(&self.inner),
                self.inner
                    .state
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            )
        });
        let scope = NEXT_SCOPE.with(|next| {
            let scope = next.get();
            next.set(
                scope
                    .checked_add(1)
                    .expect("runtime settings scope id overflow"),
            );
            scope
        });
        CURRENT.with(|current| {
            current.borrow_mut().push(BoundSettings {
                scope,
                requested: Arc::clone(&self.inner),
                owner,
                selected,
            });
        });
        RuntimeSettingsScope {
            scope,
            _not_send: PhantomData,
        }
    }

    /// Replace these settings with a snapshot of the currently selected
    /// native module state.
    ///
    /// Rust hosts use this when an ambient `rustel-core` pattern first enters
    /// an isolated runtime. Later changes to the process fallback cannot then
    /// couple that runtime to another one.
    #[doc(hidden)]
    pub fn inherit_current(&self) {
        let source = Self::snapshot_current();
        self.replace_with(&source);
    }

    /// Return a detached snapshot of the currently selected module state.
    ///
    /// Native score adoption binds this snapshot while parsing and probing a
    /// candidate. Until [`RuntimeSettings::replace_with`] publishes it, an
    /// exported last-good graph continues to observe its original state.
    #[doc(hidden)]
    pub fn snapshot_current() -> Self {
        let selected = current_state();
        Self {
            inner: Arc::new(RuntimeSettingsInner {
                identity: Arc::new(()),
                state: RwLock::new(selected),
            }),
        }
    }

    /// Return a detached publication slot with this runtime's identity.
    ///
    /// Score construction uses this so callbacks that re-enter the runtime
    /// inherit the candidate state, while the published last-good state stays
    /// unchanged until the host accepts the candidate.
    #[doc(hidden)]
    pub fn detached_snapshot(&self) -> Self {
        self.with(Self::pin_current)
    }

    /// Create an unpublished candidate in this runtime, seeded from `source`.
    ///
    /// The new slot owns later mutations. Binding `source` while creating a
    /// same-identity candidate would instead make nested setters mutate the
    /// seed through scope inheritance.
    #[doc(hidden)]
    pub fn detached_from(&self, source: &Self) -> Self {
        let selected = source
            .inner
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Self {
            inner: Arc::new(RuntimeSettingsInner {
                identity: Arc::clone(&self.inner.identity),
                state: RwLock::new(selected),
            }),
        }
    }

    /// Identify the selected host dictionary for retained setup replay.
    #[doc(hidden)]
    pub fn host_voicing_identity(&self) -> Option<VoicingDictionaryIdentity> {
        let state = self
            .inner
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lease = state.default_voicings.as_ref()?._lease.as_ref()?;
        Some(VoicingDictionaryIdentity(Arc::downgrade(&lease._inner)))
    }

    /// Restore a setup-selected dictionary from the new realm.
    #[doc(hidden)]
    pub fn adopt_replayed_voicing_default(&self, source: &Self) {
        let selection = source
            .inner
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .default_voicings
            .clone();
        self.with(|| update_current(|state| state.default_voicings = selection));
    }

    /// Copy native settings into a fresh realm after a producer panic.
    ///
    /// Keep the last parsed host dictionary. Do not retain its old realm lease
    /// or share mutable dictionary and legacy voicing cells with that realm.
    #[doc(hidden)]
    pub fn recovery_snapshot(&self) -> Self {
        let source = self
            .inner
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let default_voicings = source
            .default_voicings
            .as_ref()
            .map(|selection| VoicingSetting {
                name: selection.name.clone(),
                _lease: None,
                live_dictionary: selection.live_dictionary.as_ref().map(|dictionary| {
                    Arc::new(RwLock::new(
                        dictionary
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone(),
                    ))
                }),
            });
        let top_note = *source
            .legacy_voicing_top_note
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = SettingsState {
            max_polyphony: source.max_polyphony,
            rng_mode: source.rng_mode,
            default_join: source.default_join,
            default_voicings,
            midi_maps: source.midi_maps.clone(),
            user_voicing_dicts: source.user_voicing_dicts.clone(),
            legacy_voicing_top_note: Arc::new(Mutex::new(top_note)),
        };
        Self {
            inner: Arc::new(RuntimeSettingsInner {
                identity: Arc::new(()),
                state: RwLock::new(Arc::new(state)),
            }),
        }
    }

    /// Detach the selected state while retaining the current runtime's
    /// identity. Join resolution uses this for its temporary inner patterns,
    /// keeping one query coherent even after the wrapper's lexical scope has
    /// returned.
    pub(crate) fn pin_current() -> Self {
        let (identity, selected) = current_binding();
        Self {
            inner: Arc::new(RuntimeSettingsInner {
                identity,
                state: RwLock::new(selected),
            }),
        }
    }

    /// Return the handle that established the innermost settings scope.
    ///
    /// Query-created lookup metadata uses this rather than the effective
    /// detached candidate owner. A candidate graph therefore observes its
    /// staged state while probing, but metadata retained after acceptance
    /// continues to follow the Session's published state.
    pub(crate) fn current_requested() -> Option<Self> {
        CURRENT.with(|current| {
            current.borrow().last().map(|bound| Self {
                inner: Arc::clone(&bound.requested),
            })
        })
    }

    /// Whether the innermost scope already requested this exact handle.
    pub(crate) fn is_current_requested(&self) -> bool {
        CURRENT.with(|current| {
            current
                .borrow()
                .last()
                .is_some_and(|bound| Arc::ptr_eq(&bound.requested, &self.inner))
        })
    }

    /// Atomically publish a detached snapshot to every clone of this runtime.
    #[doc(hidden)]
    pub fn replace_with(&self, source: &Self) {
        let selected = source
            .inner
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        *self
            .inner
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = selected;
    }
}

/// A fixed-depth storage walk, separate from score evaluation and formatting.
struct NativeRetention {
    limit: usize,
    bytes: usize,
    remaining_work: usize,
}

impl NativeRetention {
    const MAX_WORK: usize = 65_536;

    fn new(limit: usize) -> Self {
        Self {
            limit,
            bytes: 0,
            remaining_work: Self::MAX_WORK,
        }
    }

    fn add(&mut self, bytes: usize) -> Option<()> {
        let total = self.bytes.checked_add(bytes)?;
        if total > self.limit {
            return None;
        }
        self.bytes = total;
        Some(())
    }

    fn work(&mut self, entries: usize) -> Option<()> {
        self.remaining_work = self.remaining_work.checked_sub(entries)?;
        Some(())
    }

    fn arc<T>(&mut self) -> Option<()> {
        self.work(1)?;
        // Strong/weak counters and alignment padding around the payload.
        self.add(std::mem::size_of::<T>().checked_add(
            (2 * std::mem::size_of::<usize>()).checked_add(std::mem::align_of::<T>())?,
        )?)
    }

    fn string(&mut self, value: &String) -> Option<()> {
        self.work(1)?;
        self.add(value.capacity())
    }

    fn vector<T>(&mut self, values: &Vec<T>) -> Option<()> {
        self.work(values.len())?;
        self.add(values.capacity().checked_mul(std::mem::size_of::<T>())?)
    }

    fn map<K, V>(&mut self, values: &HashMap<K, V>) -> Option<()> {
        let capacity = values.capacity();
        // HashMap iteration scans spare buckets too. Check work before entry,
        // not after walking a possibly sparse table. Settings tables are built
        // by insertion/replacement, without tombstone-producing removals.
        if capacity == 0 {
            return Some(());
        }
        // Charge spare buckets, control bytes and table alignment rather than
        // treating capacity as a tightly packed array of live entries.
        let buckets = capacity.checked_add(1)?.checked_next_power_of_two()?;
        self.work(buckets)?;
        let bucket_bytes =
            std::mem::size_of::<(K, V)>().checked_add(std::mem::size_of::<usize>())?;
        self.add(buckets.checked_mul(bucket_bytes)?.checked_add(64)?)
    }

    fn strings(&mut self, values: &Vec<String>) -> Option<()> {
        self.vector(values)?;
        for value in values {
            self.string(value)?;
        }
        Some(())
    }

    fn voicing_dict(&mut self, dictionary: &VoicingDict) -> Option<()> {
        self.map(dictionary)?;
        for (symbol, voicings) in dictionary {
            self.string(symbol)?;
            self.vector(voicings)?;
            for voicing in voicings {
                self.vector(voicing)?;
            }
        }
        Some(())
    }
}

impl Drop for RuntimeSettingsScope {
    fn drop(&mut self) {
        let _ = CURRENT.try_with(|current| {
            let mut current = current.borrow_mut();
            if current
                .last()
                .is_some_and(|bound| bound.scope == self.scope)
            {
                current.pop();
                return;
            }
            if let Some(index) = current.iter().position(|bound| bound.scope == self.scope) {
                current.remove(index);
            }
        });
    }
}

fn current_binding() -> (Arc<()>, Arc<SettingsState>) {
    CURRENT
        .with(|current| {
            current.borrow().last().map(|bound| {
                (
                    Arc::clone(&bound.owner.identity),
                    Arc::clone(&bound.selected),
                )
            })
        })
        .unwrap_or_else(|| {
            (
                Arc::clone(&FALLBACK.inner.identity),
                FALLBACK
                    .inner
                    .state
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            )
        })
}

fn current_state() -> Arc<SettingsState> {
    current_binding().1
}

/// The identity of the settings the operation in progress runs under: the
/// snapshot selected for it, and the handle that requested the scope.
///
/// A query-result cache keys on it. Every setter publishes a fresh snapshot,
/// so a changed setting is a changed identity and the cache misses; two
/// identities are equal only when they name the very same snapshot through
/// the very same handle. The handle side matters because a detached
/// candidate shares its snapshot with the runtime it was taken from while
/// its lookup metadata follows the handle, so the same graph asked the same
/// question under either would otherwise answer with the other's metadata.
/// The identity keeps both alive, so a later snapshot or handle cannot land
/// at the same address while an entry still names the old one. Host state
/// mutated in place - the legacy voicing history, a live dictionary - is not
/// part of the identity; the graphs that read it declare themselves volatile.
#[derive(Clone, Debug)]
pub struct SettingsStateId {
    requested: Arc<RuntimeSettingsInner>,
    selected: Arc<SettingsState>,
}

impl PartialEq for SettingsStateId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.requested, &other.requested)
            && Arc::ptr_eq(&self.selected, &other.selected)
    }
}

impl Eq for SettingsStateId {}

/// See [`SettingsStateId`].
pub fn current_state_id() -> SettingsStateId {
    CURRENT
        .with(|current| {
            current.borrow().last().map(|bound| SettingsStateId {
                requested: Arc::clone(&bound.requested),
                selected: Arc::clone(&bound.selected),
            })
        })
        .unwrap_or_else(|| SettingsStateId {
            requested: Arc::clone(&FALLBACK.inner),
            selected: FALLBACK
                .inner
                .state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        })
}

fn with_current<R>(f: impl FnOnce(&SettingsState) -> R) -> R {
    CURRENT.with(|current| {
        let current = current.borrow();
        if let Some(bound) = current.last() {
            return f(&bound.selected);
        }
        drop(current);
        let fallback = FALLBACK
            .inner
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&fallback)
    })
}

fn current_owner_and_selected() -> (Arc<RuntimeSettingsInner>, Option<Arc<SettingsState>>) {
    CURRENT
        .with(|current| {
            current
                .borrow()
                .last()
                .map(|bound| (Arc::clone(&bound.owner), Some(Arc::clone(&bound.selected))))
        })
        .unwrap_or_else(|| (Arc::clone(&FALLBACK.inner), None))
}

fn update_current<R>(f: impl FnOnce(&mut SettingsState) -> R) -> R {
    let (owner, selected) = current_owner_and_selected();
    let (replacement, result) = {
        let mut published = owner
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = selected.as_deref().unwrap_or(&published);
        let mut next = SettingsState {
            max_polyphony: source.max_polyphony,
            rng_mode: source.rng_mode,
            default_join: source.default_join,
            default_voicings: source.default_voicings.clone(),
            midi_maps: source.midi_maps.clone(),
            user_voicing_dicts: source.user_voicing_dicts.clone(),
            legacy_voicing_top_note: Arc::clone(&source.legacy_voicing_top_note),
        };
        let result = f(&mut next);
        let next = Arc::new(next);
        *published = Arc::clone(&next);
        (next, result)
    };
    CURRENT.with(|current| {
        for bound in current.borrow_mut().iter_mut() {
            if Arc::ptr_eq(&bound.owner, &owner) {
                bound.selected = Arc::clone(&replacement);
            }
        }
    });
    result
}

/// Maximum score-selected polyphony supported by the native host.
/// Kept independent of the audio crate so the JavaScript evaluator stays portable.
pub const MAX_CONFIGURABLE_POLYPHONY: usize = 256;

/// The current runtime's explicit score override, if any.
pub fn max_polyphony() -> Option<usize> {
    with_current(|settings| settings.max_polyphony)
}

/// Set the score override in the currently bound candidate/runtime.
/// Host defaults belong to the host and must not be written through this setter.
pub fn set_max_polyphony(voices: usize) {
    update_current(|settings| {
        settings.max_polyphony = Some(voices.clamp(1, MAX_CONFIGURABLE_POLYPHONY));
    });
}

pub(crate) fn set_rng_mode(mode: u8) {
    update_current(|settings| settings.rng_mode = mode);
}

pub(crate) fn rng_mode() -> u8 {
    with_current(|settings| settings.rng_mode)
}

pub(crate) fn set_default_join(alignment: u8) {
    update_current(|settings| settings.default_join = alignment);
}

pub(crate) fn default_join() -> u8 {
    with_current(|settings| settings.default_join)
}

pub(crate) fn set_default_voicings(name: Option<String>) {
    update_current(|settings| {
        settings.default_voicings = name.map(|name| VoicingSetting {
            name,
            _lease: None,
            live_dictionary: None,
        });
    });
}

pub(crate) fn set_default_voicings_with_lease(name: String, lease: VoicingDictionaryLease) {
    update_current(|settings| {
        settings.default_voicings = Some(VoicingSetting {
            name,
            _lease: Some(lease),
            live_dictionary: Some(Arc::new(RwLock::new(None))),
        });
    });
}

pub(crate) fn register_midi_maps(
    maps: Vec<(String, Arc<HashMap<String, crate::midimap::MidiMapEntry>>)>,
) -> Result<(), String> {
    update_current(|settings| {
        let new_names = maps
            .iter()
            .filter(|(name, _)| !settings.midi_maps.contains_key(name))
            .count();
        if settings.midi_maps.len().saturating_add(new_names)
            > crate::midimap::MAX_REGISTERED_MIDI_MAPS
        {
            return Err(format!(
                "midimaps: at most {} named maps may be registered in one runtime",
                crate::midimap::MAX_REGISTERED_MIDI_MAPS
            ));
        }
        for (name, entries) in maps {
            settings.midi_maps.insert(name, entries);
        }
        Ok(())
    })
}

pub(crate) fn midi_map(name: &str) -> Option<Arc<HashMap<String, crate::midimap::MidiMapEntry>>> {
    with_current(|settings| settings.midi_maps.get(name).map(Arc::clone))
}

pub(crate) fn midi_maps() -> HashMap<String, Arc<HashMap<String, crate::midimap::MidiMapEntry>>> {
    with_current(|settings| settings.midi_maps.clone())
}

pub(crate) fn register_voicing_dict(
    name: String,
    semantic: Arc<VoicingDict>,
    legacy: Arc<crate::voicings::LegacyVoicingDict>,
) -> Result<(), String> {
    update_current(|settings| {
        if !settings.user_voicing_dicts.contains_key(&name)
            && settings.user_voicing_dicts.len()
                >= crate::voicings::MAX_REGISTERED_USER_VOICING_DICTS
        {
            return Err(format!(
                "addVoicings: at most {} dictionaries may be registered in one runtime",
                crate::voicings::MAX_REGISTERED_USER_VOICING_DICTS
            ));
        }
        settings
            .user_voicing_dicts
            .insert(name, Arc::new(UserVoicingDictionaries { semantic, legacy }));
        Ok(())
    })
}

pub(crate) fn user_voicing_dict(name: &str) -> Option<Arc<VoicingDict>> {
    with_current(|settings| {
        settings
            .user_voicing_dicts
            .get(name)
            .map(|entry| Arc::clone(&entry.semantic))
    })
}

pub(crate) fn user_legacy_voicing_dict(
    name: &str,
) -> Option<Arc<crate::voicings::LegacyVoicingDict>> {
    with_current(|settings| {
        settings
            .user_voicing_dicts
            .get(name)
            .map(|entry| Arc::clone(&entry.legacy))
    })
}

pub(crate) fn default_voicings() -> Option<String> {
    with_current(|settings| {
        settings
            .default_voicings
            .as_ref()
            .map(|selection| selection.name.clone())
    })
}

pub(crate) fn default_voicings_is_host_owned() -> bool {
    with_current(|settings| {
        settings
            .default_voicings
            .as_ref()
            .is_some_and(|selection| selection._lease.is_some())
    })
}

pub(crate) fn default_host_voicing_dict() -> Option<Arc<RwLock<Option<VoicingDict>>>> {
    with_current(|settings| {
        settings
            .default_voicings
            .as_ref()
            .and_then(|selection| selection.live_dictionary.as_ref().map(Arc::clone))
    })
}

pub(crate) fn sync_host_voicing_dict(name: &str, dict: Option<VoicingDict>) -> bool {
    let cell = with_current(|settings| {
        settings.default_voicings.as_ref().and_then(|selection| {
            (selection.name == name)
                .then(|| selection.live_dictionary.as_ref().map(Arc::clone))
                .flatten()
        })
    });
    let Some(cell) = cell else {
        return false;
    };
    *cell
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = dict;
    true
}

pub(crate) fn with_legacy_voicing_top_note<R>(f: impl FnOnce(&mut Option<i64>) -> R) -> R {
    let cell = with_current(|settings| Arc::clone(&settings.legacy_voicing_top_note));
    let mut top_note = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut top_note)
}

pub(crate) fn reset_legacy_voicing_top_note() {
    with_legacy_voicing_top_note(|top_note| *top_note = None);
}

#[cfg(test)]
mod poison_tests {
    use super::*;

    fn interrupt_update(settings: &RuntimeSettings) {
        let result = std::panic::catch_unwind(|| {
            settings.with(|| {
                update_current(|state| {
                    state.max_polyphony = Some(99);
                    panic!("interrupt settings update");
                });
            });
        });
        assert!(result.is_err());
        assert!(settings.inner.state.is_poisoned());
    }

    #[test]
    fn poisoned_settings_keep_the_last_publication_and_accept_later_updates() {
        let settings = RuntimeSettings::default();
        settings.with(|| set_max_polyphony(13));
        interrupt_update(&settings);

        settings.with(|| {
            assert_eq!(max_polyphony(), Some(13));
            set_max_polyphony(27);
            assert_eq!(max_polyphony(), Some(27));
        });
        settings.with(|| assert_eq!(max_polyphony(), Some(27)));
        assert_eq!(settings.retained_snapshot_bytes(usize::MAX), None);
    }

    #[test]
    fn snapshots_and_replacements_recover_poisoned_publication_slots() {
        let source = RuntimeSettings::default();
        source.with(|| set_max_polyphony(33));
        interrupt_update(&source);
        let target = RuntimeSettings::default();
        target.with(|| set_max_polyphony(12));
        interrupt_update(&target);

        let candidate = target.detached_from(&source);
        candidate.with(|| {
            assert_eq!(max_polyphony(), Some(33));
            set_max_polyphony(17);
        });
        source.with(|| assert_eq!(max_polyphony(), Some(33)));
        target.with(|| assert_eq!(max_polyphony(), Some(12)));

        let snapshot = source.detached_snapshot();
        snapshot.with(|| assert_eq!(max_polyphony(), Some(33)));
        target.replace_with(&source);
        target.with(|| assert_eq!(max_polyphony(), Some(33)));
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn recovery_keeps_native_values_and_releases_the_old_realm() {
        let settings = RuntimeSettings::default();
        let retired = Arc::new(AtomicBool::new(false));
        settings.with(|| {
            set_max_polyphony(37);
            set_rng_mode(1);
            set_default_join(2);
            register_midi_maps(vec![("controller".into(), Arc::new(HashMap::new()))])
                .expect("MIDI map");
            crate::voicings::register_user_dict_json("native", r#"{"M":["1P 3M 5P"]}"#)
                .expect("native dictionary");
            set_default_voicings_with_lease(
                "host-view".into(),
                VoicingDictionaryLease::new(retired.clone(), Arc::new(AtomicBool::new(false))),
            );
            assert!(sync_host_voicing_dict(
                "host-view",
                Some(HashMap::from([("M".into(), vec![vec![0.0, 4.0, 7.0]])])),
            ));
            with_legacy_voicing_top_note(|note| *note = Some(60));
        });
        let identity = settings
            .host_voicing_identity()
            .expect("host dictionary identity");
        let recovered = settings.recovery_snapshot();
        assert!(!Arc::ptr_eq(
            &settings.inner.identity,
            &recovered.inner.identity
        ));
        let original_dict = settings.with(default_host_voicing_dict).unwrap();
        let recovered_dict = recovered.with(default_host_voicing_dict).unwrap();
        assert!(!Arc::ptr_eq(&original_dict, &recovered_dict));
        settings.with(|| {
            set_max_polyphony(99);
            *original_dict.write().unwrap() = None;
            with_legacy_voicing_top_note(|note| *note = Some(72));
        });
        drop(settings);
        assert!(
            retired.load(Ordering::Acquire),
            "recovery retained a realm lease"
        );
        assert!(
            identity.0.upgrade().is_none(),
            "identity retained the old realm"
        );
        recovered.with(|| {
            assert_eq!(max_polyphony(), Some(37));
            assert_eq!(rng_mode(), 1);
            assert_eq!(default_join(), 2);
            assert_eq!(default_voicings().as_deref(), Some("host-view"));
            assert!(!default_voicings_is_host_owned());
            assert!(midi_map("controller").is_some());
            assert!(
                recovered
                    .inner
                    .state
                    .read()
                    .unwrap()
                    .user_voicing_dicts
                    .contains_key("native")
            );
            assert_eq!(
                recovered_dict.read().unwrap().as_ref().unwrap()["M"],
                vec![vec![0.0, 4.0, 7.0]]
            );
            with_legacy_voicing_top_note(|note| assert_eq!(*note, Some(60)));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_retention_default_and_byte_limit_are_finite() {
        let settings = RuntimeSettings::default();
        let bytes = settings.retained_snapshot_bytes(16 * 1024).unwrap();
        assert!(bytes > std::mem::size_of::<RuntimeSettings>());
        assert_eq!(settings.retained_snapshot_bytes(bytes), Some(bytes));
        assert_eq!(settings.retained_snapshot_bytes(bytes - 1), None);
        assert_eq!(settings.retained_snapshot_bytes(0), None);
    }

    #[test]
    fn snapshot_retention_charges_map_and_string_capacity() {
        let settings = RuntimeSettings::default();
        let before = settings.retained_snapshot_bytes(16 * 1024).unwrap();
        let mut control = String::with_capacity(128);
        control.push_str("lpf");
        let control_capacity = control.capacity();
        let entries = HashMap::from([(
            "cutoff".to_owned(),
            crate::midimap::MidiMapEntry {
                control,
                ccn: 74,
                min: 0.0,
                max: 1.0,
                exp: 1.0,
            },
        )]);
        settings.with(|| {
            register_midi_maps(vec![("native-map".to_owned(), Arc::new(entries))]).unwrap();
        });
        let with_map = settings.retained_snapshot_bytes(16 * 1024).unwrap();
        assert!(with_map >= before + control_capacity + "cutoff".len() + "native-map".len());
        settings.with(|| set_default_voicings(Some("v".repeat(256))));
        let with_name = settings.retained_snapshot_bytes(16 * 1024).unwrap();
        assert!(with_name >= with_map + 256);
        assert_eq!(settings.retained_snapshot_bytes(with_map), None);
    }

    #[test]
    fn snapshot_retention_charges_semantic_and_legacy_dictionaries() {
        let settings = RuntimeSettings::default();
        let before = settings.retained_snapshot_bytes(64 * 1024).unwrap();
        settings.with(|| {
            crate::voicings::register_user_dict_json("native", r#"{"M":["1P 3M 5P"]}"#).unwrap();
        });
        let after = settings.retained_snapshot_bytes(64 * 1024).unwrap();
        assert!(after > before);
        let state = settings.inner.state.read().unwrap();
        let dictionaries = &state.user_voicing_dicts["native"];
        let mut semantic = NativeRetention::new(64 * 1024);
        semantic.voicing_dict(&dictionaries.semantic).unwrap();
        assert!(after > before + semantic.bytes);

        // Legacy splitting preserves empty strings too; charging only numeric
        // steps would miss these native vector elements.
        let mut work = NativeRetention::new(usize::MAX);
        work.remaining_work = 2;
        assert_eq!(work.strings(&vec![String::new(); 3]), None);
        assert_eq!(work.bytes, 0, "work admission precedes traversal");
    }

    #[test]
    fn snapshot_retention_refuses_sparse_table_work_and_overflow() {
        let values: HashMap<String, String> = HashMap::with_capacity(8);
        let mut charge = NativeRetention::new(usize::MAX);
        charge.remaining_work = values.capacity() - 1;
        assert_eq!(charge.map(&values), None);
        assert_eq!(charge.bytes, 0);

        charge.bytes = usize::MAX - 1;
        assert_eq!(charge.add(2), None);
        assert_eq!(charge.bytes, usize::MAX - 1);
    }

    #[test]
    fn snapshot_retention_detaches_publication_and_charges_fixed_shared_cells() {
        let settings = RuntimeSettings::default();
        let retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let any_retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        settings.with(|| {
            set_default_voicings_with_lease(
                "host-view".to_owned(),
                VoicingDictionaryLease::new(retired, any_retired),
            );
        });
        let snapshot = settings.detached_from(&settings);
        let before = snapshot.retained_snapshot_bytes(64 * 1024).unwrap();
        let view = settings.with(default_host_voicing_dict).unwrap();
        assert!(Arc::ptr_eq(
            &view,
            &snapshot.with(default_host_voicing_dict).unwrap(),
        ));
        assert!(Arc::ptr_eq(
            &settings.inner.state.read().unwrap().legacy_voicing_top_note,
            &snapshot.inner.state.read().unwrap().legacy_voicing_top_note,
        ));
        settings.with(|| {
            assert!(sync_host_voicing_dict(
                "host-view",
                Some(HashMap::from([("M".to_owned(), vec![vec![0.0, 4.0, 7.0]])])),
            ));
            with_legacy_voicing_top_note(|top_note| *top_note = Some(60));
        });
        assert_eq!(snapshot.retained_snapshot_bytes(before), Some(before));
        assert_eq!(snapshot.retained_snapshot_bytes(before - 1), None);
        snapshot.with(|| {
            assert_eq!(
                default_host_voicing_dict()
                    .unwrap()
                    .read()
                    .unwrap()
                    .as_ref()
                    .unwrap()["M"],
                vec![vec![0.0, 4.0, 7.0]],
            );
            with_legacy_voicing_top_note(|top_note| {
                assert_eq!(*top_note, Some(60));
            });
        });

        settings.with(|| set_default_voicings(Some("new-selection".to_owned())));
        assert!(settings.with(default_host_voicing_dict).is_none());
        assert!(Arc::ptr_eq(
            &view,
            &snapshot.with(default_host_voicing_dict).unwrap(),
        ));
        assert_eq!(snapshot.retained_snapshot_bytes(before), Some(before));
        snapshot.with(|| assert_eq!(default_voicings().as_deref(), Some("host-view")));
        settings.with(|| assert_eq!(default_voicings().as_deref(), Some("new-selection")));
    }

    #[test]
    fn snapshot_retention_accepts_shared_dictionary_growth_to_the_parser_boundary() {
        let settings = RuntimeSettings::default();
        settings.with(|| {
            set_default_voicings_with_lease(
                "host-view".to_owned(),
                VoicingDictionaryLease::new(
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                ),
            );
        });
        let snapshot = settings.detached_from(&settings);
        let before = snapshot.retained_snapshot_bytes(64 * 1024).unwrap();
        let symbols = crate::voicings::MAX_USER_VOICING_SYMBOLS;
        let total_steps = crate::voicings::MAX_USER_VOICING_TOTAL_STEPS;
        let voicings = total_steps / symbols;
        assert!(voicings <= crate::voicings::MAX_USER_VOICINGS_PER_SYMBOL);
        assert_eq!(symbols * voicings, total_steps);
        let dictionary: HashMap<_, _> = (0..symbols)
            .map(|symbol| (symbol.to_string(), vec![vec![0.0]; voicings]))
            .collect();
        let json = serde_json::to_string(&dictionary).unwrap();
        assert!(json.len() <= crate::voicings::MAX_USER_VOICING_JSON_BYTES);
        settings.with(|| {
            crate::voicings::sync_host_default_json("host-view", Some(&json)).unwrap();
        });
        snapshot.with(|| {
            let view = default_host_voicing_dict().unwrap();
            let view = view.read().unwrap();
            let view = view.as_ref().unwrap();
            assert_eq!(view.len(), symbols);
            assert_eq!(
                view.values().flatten().map(Vec::len).sum::<usize>(),
                total_steps,
            );
        });
        // Valid shared growth must not exhaust immutable-snapshot admission,
        // including after the current publication stops selecting this view.
        settings.with(|| set_default_voicings(Some("triads".to_owned())));
        assert_eq!(snapshot.retained_snapshot_bytes(before), Some(before));
    }

    #[test]
    fn snapshot_retention_does_not_lock_shared_cells_or_wait_for_publication() {
        let settings = RuntimeSettings::default();
        settings.with(|| {
            set_default_voicings_with_lease(
                "host-view".to_owned(),
                VoicingDictionaryLease::new(
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                ),
            );
        });
        let bytes = settings.retained_snapshot_bytes(64 * 1024).unwrap();
        {
            let state = settings.inner.state.read().unwrap();
            let cell = state
                .default_voicings
                .as_ref()
                .unwrap()
                .live_dictionary
                .as_ref()
                .unwrap();
            let _view = cell.write().unwrap();
            let _history = state.legacy_voicing_top_note.lock().unwrap();
            assert_eq!(settings.retained_snapshot_bytes(bytes), Some(bytes));
        }
        let _publication = settings.inner.state.write().unwrap();
        assert_eq!(settings.retained_snapshot_bytes(usize::MAX), None);
    }

    #[test]
    fn legacy_history_snapshots_share_one_mutex_and_native_queries_across_threads() {
        fn notes(chord: &str) -> Vec<crate::Value> {
            crate::voicings::voicings(
                &crate::pure(crate::Value::Str(chord.to_owned())),
                crate::Value::Str("thread-history".into()),
            )
            .query_arc(
                rustel_fraction::Fraction::ZERO,
                rustel_fraction::Fraction::ONE,
            )
            .into_iter()
            .map(|hap| hap.value)
            .collect()
        }
        fn expected(notes: [&str; 3]) -> Vec<crate::Value> {
            notes
                .into_iter()
                .map(|note| crate::Value::Str(note.to_owned()))
                .collect()
        }

        let settings = RuntimeSettings::default();
        settings.with(|| {
            crate::voicings::register_user_dict_json_with_range(
                "thread-history",
                r#"{"": ["1P 3M 5P", "3M 5P 8P"]}"#,
                Some(r#"["C3", "C5"]"#),
                false,
            )
            .unwrap();
            assert_eq!(notes("C"), expected(["C3", "E3", "G3"]));
        });
        let snapshot = settings.detached_snapshot();
        let cell = Arc::clone(&settings.inner.state.read().unwrap().legacy_voicing_top_note);
        assert!(Arc::ptr_eq(
            &cell,
            &snapshot.inner.state.read().unwrap().legacy_voicing_top_note
        ));
        assert!(!Arc::ptr_eq(
            &cell,
            &RuntimeSettings::default()
                .inner
                .state
                .read()
                .unwrap()
                .legacy_voicing_top_note
        ));
        settings.with(|| set_default_join(6));
        assert!(Arc::ptr_eq(
            &cell,
            &settings.inner.state.read().unwrap().legacy_voicing_top_note
        ));

        let lock = cell.lock().unwrap();
        let (probed_tx, probed_rx) = std::sync::mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
        let remote = snapshot.clone();
        let worker = std::thread::spawn(move || {
            remote.with(|| {
                let cell = with_current(|state| Arc::clone(&state.legacy_voicing_top_note));
                let held = matches!(cell.try_lock(), Err(std::sync::TryLockError::WouldBlock));
                probed_tx.send(held).unwrap();
                if resume_rx.recv().is_err() {
                    return Vec::new();
                }
                notes("G")
            })
        });
        assert!(
            probed_rx.recv().unwrap(),
            "the detached snapshot must use the same history mutex"
        );
        drop(lock);
        resume_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), expected(["G3", "B3", "D4"]));
        settings.with(|| assert_eq!(notes("C"), expected(["E3", "G3", "C4"])));
        snapshot.with(crate::voicings::reset_voicings);
        settings.with(|| assert_eq!(notes("C"), expected(["C3", "E3", "G3"])));
    }

    #[test]
    fn runtime_handle_is_pointer_sized_and_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RuntimeSettings>();
        assert_eq!(
            std::mem::size_of::<RuntimeSettings>(),
            std::mem::size_of::<Arc<()>>()
        );
    }

    #[test]
    fn nested_runtime_scopes_restore_the_previous_settings() {
        let baseline = RuntimeSettings::default();
        let outer = RuntimeSettings::default();
        let inner = RuntimeSettings::default();

        baseline.with(|| {
            outer.with(|| {
                set_rng_mode(1);
                assert_eq!(rng_mode(), 1);
                inner.with(|| {
                    assert_eq!(rng_mode(), 0);
                    set_rng_mode(2);
                    assert_eq!(rng_mode(), 2);
                });
                assert_eq!(rng_mode(), 1);
            });
            assert_eq!(rng_mode(), 0);
        });
    }

    #[test]
    fn runtime_scope_restores_after_unwind() {
        let baseline = RuntimeSettings::default();
        let settings = RuntimeSettings::default();
        baseline.with(|| {
            let result = std::panic::catch_unwind(|| {
                settings.with(|| {
                    set_rng_mode(1);
                    panic!("leave scope");
                });
            });

            assert!(result.is_err());
            assert_eq!(rng_mode(), 0);
            settings.with(|| assert_eq!(rng_mode(), 1));
        });
    }

    #[test]
    fn ambient_snapshots_are_detached_until_atomically_published() {
        let ambient = RuntimeSettings::default();
        let target = RuntimeSettings::default();
        target.with(|| {
            set_rng_mode(1);
            set_default_join(2);
            set_default_voicings(Some("guidetones".into()));
        });
        ambient.with(|| {
            set_rng_mode(0);
            set_default_join(6);
            set_default_voicings(Some("lefthand".into()));

            let candidate = RuntimeSettings::snapshot_current();
            target.with(|| {
                assert_eq!(rng_mode(), 1);
                assert_eq!(default_join(), 2);
                assert_eq!(default_voicings().as_deref(), Some("guidetones"));
            });
            candidate.with(|| {
                assert_eq!(rng_mode(), 0);
                assert_eq!(default_join(), 6);
                assert_eq!(default_voicings().as_deref(), Some("lefthand"));
            });

            target.replace_with(&candidate);
            target.with(|| {
                assert_eq!(rng_mode(), 0);
                assert_eq!(default_join(), 6);
                assert_eq!(default_voicings().as_deref(), Some("lefthand"));
            });
        });
    }

    #[test]
    fn candidate_snapshots_share_identity_without_publishing_state() {
        let published = RuntimeSettings::default();
        published.with(|| set_default_join(2));
        let candidate = published.detached_snapshot();
        candidate.with(|| set_default_join(6));

        published.with(|| assert_eq!(default_join(), 2));
        candidate.with(|| {
            assert_eq!(default_join(), 6);
            published.with(|| assert_eq!(default_join(), 6));
        });

        published.replace_with(&candidate);
        published.with(|| assert_eq!(default_join(), 6));
    }

    #[test]
    fn seeded_candidates_own_mutations_without_changing_the_seed() {
        let published = RuntimeSettings::default();
        published.with(|| set_default_join(2));
        let seed = RuntimeSettings::default();
        seed.with(|| {
            set_rng_mode(1);
            set_default_join(4);
            set_default_voicings(Some("guidetones".into()));
        });

        let candidate = published.detached_from(&seed);
        candidate.with(|| {
            assert_eq!(
                (rng_mode(), default_join(), default_voicings()),
                (1, 4, Some("guidetones".into()))
            );
            set_default_join(6);
            published.with(|| assert_eq!(default_join(), 6));
        });
        seed.with(|| {
            assert_eq!(
                (rng_mode(), default_join(), default_voicings()),
                (1, 4, Some("guidetones".into())),
                "candidate mutation changed its seed"
            );
        });
        published.with(|| assert_eq!(default_join(), 2));
        candidate.with(|| assert_eq!(default_join(), 6));
    }

    #[test]
    fn repeated_same_handle_scopes_do_not_grow_the_binding_stack() {
        let settings = RuntimeSettings::default();
        settings.with(|| {
            let depth = CURRENT.with(|current| current.borrow().len());
            settings.with(|| {
                assert_eq!(CURRENT.with(|current| current.borrow().len()), depth);
                set_rng_mode(1);
            });
            assert_eq!(rng_mode(), 1);
        });
    }

    #[test]
    fn a_bound_operation_keeps_one_snapshot_across_concurrent_publication() {
        let stable = RuntimeSettings::default();
        stable.with(|| {
            set_rng_mode(1);
            set_default_join(2);
            set_default_voicings(Some("guidetones".into()));
        });
        let candidate = RuntimeSettings::default();
        candidate.with(|| {
            set_rng_mode(0);
            set_default_join(6);
            set_default_voicings(Some("lefthand".into()));
        });

        let entered = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let query_settings = stable.clone();
        let query_entered = entered.clone();
        let query_resume = resume.clone();
        let query = std::thread::spawn(move || {
            query_settings.with(|| {
                let rng = rng_mode();
                query_entered.wait();
                query_resume.wait();
                (rng, default_join(), default_voicings())
            })
        });

        entered.wait();
        stable.replace_with(&candidate);
        resume.wait();
        assert_eq!(
            query.join().expect("query thread"),
            (1, 2, Some("guidetones".into()))
        );
        stable.with(|| {
            assert_eq!(rng_mode(), 0);
            assert_eq!(default_join(), 6);
            assert_eq!(default_voicings().as_deref(), Some("lefthand"));
        });
    }

    #[test]
    fn a_bound_setter_never_merges_fields_from_an_unobserved_publication() {
        let settings = RuntimeSettings::default();
        settings.with(|| {
            set_rng_mode(1);
            set_default_join(2);
            set_default_voicings(Some("guidetones".into()));
        });

        let entered = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let setter_settings = settings.clone();
        let setter_entered = entered.clone();
        let setter_resume = resume.clone();
        let setter = std::thread::spawn(move || {
            setter_settings.with(|| {
                assert_eq!(
                    (rng_mode(), default_join(), default_voicings()),
                    (1, 2, Some("guidetones".into()))
                );
                setter_entered.wait();
                setter_resume.wait();
                set_default_join(4);
                (rng_mode(), default_join(), default_voicings())
            })
        });

        entered.wait();
        settings.with(|| {
            set_rng_mode(0);
            set_default_join(6);
            set_default_voicings(Some("lefthand".into()));
        });
        resume.wait();

        assert_eq!(
            setter.join().expect("setter thread"),
            (1, 4, Some("guidetones".into()))
        );
        settings.with(|| {
            assert_eq!(
                (rng_mode(), default_join(), default_voicings()),
                (1, 4, Some("guidetones".into())),
                "the setter imported fields published after its scope began"
            );
        });
    }

    #[test]
    fn host_setting_lease_retires_after_the_last_snapshot() {
        let published = RuntimeSettings::default();
        let retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let any_retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        published.with(|| {
            set_default_voicings_with_lease(
                "object-default".into(),
                VoicingDictionaryLease::new(Arc::clone(&retired), Arc::clone(&any_retired)),
            );
        });
        let retained = published.detached_from(&published);

        published.with(|| set_default_voicings(Some("ireal".into())));
        assert!(!retired.load(std::sync::atomic::Ordering::Acquire));
        assert!(!any_retired.load(std::sync::atomic::Ordering::Acquire));

        drop(retained);
        assert!(retired.load(std::sync::atomic::Ordering::Acquire));
        assert!(any_retired.load(std::sync::atomic::Ordering::Acquire));
    }
}
