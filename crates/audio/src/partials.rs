//! Custom periodic waves for `partials`, kept alive while notes play them.
//!
//! The voice resolver registers each list of partials and gets an id. The
//! event ring or the offline backend leases the wave before its note can
//! render: it builds the wave's tables when they are missing, points the
//! note at them and holds them until the renderer has passed the note's
//! last frame. The renderer reads the tables through that pointer and never
//! frees them.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::ptr::NonNull;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use crate::periodic_wave::CustomWave;

/// Tables that no note holds are dropped, least recently used first, once
/// all tables take more than this. The next lease builds them again.
const IDLE_TABLE_BUDGET_BYTES: usize = 64 << 20;

/// Lists whose tables no note holds are forgotten, least recently used
/// first, once all lists take more than this. A note whose list is
/// forgotten before its lease plays silence.
const LIST_BUDGET_BYTES: usize = 64 << 20;

/// Lease bounds use this rate until the consumer publishes its own: the
/// highest rate gives the longest hold.
const UNKNOWN_RATE: u32 = 768_000;

/// The custom wave of one onset: `partials`/`phases`, or `n` on a synth
/// sound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartialsControls {
    id: NonZeroU64,
    /// Set by a lease, for the renderer. A note without one plays silence.
    pub(crate) wave: Option<WaveRef>,
}

/// A leased wave. Only a lease makes one, and the lease holds the wave
/// past the last frame its note renders.
#[derive(Clone, Copy)]
pub(crate) struct WaveRef(NonNull<CustomWave>);

// SAFETY: a `CustomWave` is immutable, and the lease that made this pointer
// keeps it allocated while any renderer can read it.
unsafe impl Send for WaveRef {}
unsafe impl Sync for WaveRef {}

impl WaveRef {
    #[inline]
    pub(crate) fn wave(&self) -> &CustomWave {
        // SAFETY: see the impl of `Send` above.
        unsafe { self.0.as_ref() }
    }
}

impl PartialEq for WaveRef {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl std::fmt::Debug for WaveRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WaveRef")
    }
}

/// The partials a list of `requested` partials plays. Firefox keeps half
/// its table in coefficients, DC included: 2047 partials, or 4095 from a
/// list of 4096.
pub fn played_partials(requested: usize) -> usize {
    CustomWave::played_partials(requested)
}

/// Register a list of `requested` partials whose cosine and sine terms for
/// partials 1, 2, ... are `real` and `imag`. Terms past [`played_partials`]
/// are ignored. An identical list shares one wave.
pub fn register_partials(requested: usize, real: &[f32], imag: &[f32]) -> PartialsControls {
    let played = played_partials(requested);
    let key = WaveKey {
        table_size: CustomWave::table_size(requested),
        real: real
            .iter()
            .take(played)
            .map(|term| term.to_bits())
            .collect(),
        imag: imag
            .iter()
            .take(played)
            .map(|term| term.to_bits())
            .collect(),
    };
    PartialsControls {
        id: registry().register(key),
        wave: None,
    }
}

/// Point `partials` at its wave and return the wave for the caller to hold
/// until the note's last frame. `None` when the list is forgotten: the note
/// then plays silence.
pub(crate) fn lease(partials: &mut PartialsControls) -> Option<Arc<CustomWave>> {
    // A statement of its own: the lock must end before the build below
    // takes it again.
    let found = registry().wave(partials.id)?;
    let wave = match found {
        Ok(wave) => wave,
        // Build outside the lock: a long list takes milliseconds, and
        // another producer may be registering meanwhile.
        Err(key) => {
            let terms = |bits: &[u32]| {
                bits.iter()
                    .map(|bits| f32::from_bits(*bits))
                    .collect::<Vec<_>>()
            };
            let wave = CustomWave::new(key.table_size, &terms(&key.real), &terms(&key.imag));
            registry().store(partials.id, Arc::new(wave))
        }
    };
    partials.wave = Some(WaveRef(NonNull::from(&*wave)));
    Some(wave)
}

/// Frames after a note's start that its renderer can still read the wave:
/// the source stop, `duration + release` or a later worklet end, with the
/// filter ring and a render quantum of margin.
pub(crate) fn hold_frames(
    duration_secs: f32,
    release_secs: f32,
    start_frame: u64,
    worklet_end_secs: Option<f64>,
    sample_rate: u32,
) -> u64 {
    let sample_rate = if sample_rate == 0 {
        UNKNOWN_RATE
    } else {
        sample_rate
    };
    let rate = f64::from(sample_rate);
    let mut stop_secs = f64::from(duration_secs) + f64::from(release_secs);
    if let Some(end_secs) = worklet_end_secs {
        stop_secs = stop_secs.max(end_secs - start_frame as f64 / rate);
    }
    let stop_secs = f64::from(crate::sample::stop_secs_for(
        stop_secs as f32,
        true,
        sample_rate as f32,
    ));
    if !stop_secs.is_finite() {
        return u64::MAX;
    }
    // The renderer's age clock is f32: round past its boundary, then add a
    // worklet quantum.
    let frames = (stop_secs.max(0.0) as f32).next_up() * sample_rate as f32;
    f64::from(frames.next_up()).ceil() as u64 + 256
}

/// Waves held for the notes a producer has handed to a renderer.
#[derive(Default)]
pub(crate) struct WaveLeases {
    held: Vec<(Arc<CustomWave>, u64)>,
}

impl WaveLeases {
    /// Hold `wave` until a renderer starts a block at or after `until_frame`.
    pub(crate) fn hold(&mut self, wave: Arc<CustomWave>, until_frame: u64) {
        self.held.push((wave, until_frame));
    }

    /// Let go of every wave whose note ended before `rendered_frame`, the
    /// first frame the renderer has not finished.
    pub(crate) fn release_before(&mut self, rendered_frame: u64) {
        self.held.retain(|(_, until)| *until > rendered_frame);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.held.len()
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct WaveKey {
    table_size: usize,
    real: Box<[u32]>,
    imag: Box<[u32]>,
}

impl WaveKey {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of_val(&*self.real)
            + std::mem::size_of_val(&*self.imag)
    }
}

struct Entry {
    key: WaveKey,
    /// Built at the first lease. Dropped when idle and over budget.
    wave: Option<Arc<CustomWave>>,
    /// The tick of the last registration or lease.
    used: u64,
}

impl Entry {
    /// Whether a note holds the tables.
    fn held(&self) -> bool {
        self.wave
            .as_ref()
            .is_some_and(|wave| Arc::strong_count(wave) > 1)
    }
}

#[derive(Default)]
struct Registry {
    ids: HashMap<WaveKey, NonZeroU64>,
    entries: HashMap<NonZeroU64, Entry>,
    next: u64,
    tick: u64,
    table_bytes: usize,
    list_bytes: usize,
}

impl Registry {
    fn register(&mut self, key: WaveKey) -> NonZeroU64 {
        self.tick += 1;
        if let Some(&id) = self.ids.get(&key) {
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.used = self.tick;
            }
            return id;
        }
        self.next += 1;
        let id = NonZeroU64::new(self.next).expect("ids count up from one");
        self.list_bytes += key.bytes();
        self.ids.insert(key.clone(), id);
        self.entries.insert(
            id,
            Entry {
                key,
                wave: None,
                used: self.tick,
            },
        );
        self.trim(id);
        id
    }

    /// The tables of `id`, or the list to build them from.
    fn wave(&mut self, id: NonZeroU64) -> Option<Result<Arc<CustomWave>, WaveKey>> {
        self.tick += 1;
        let entry = self.entries.get_mut(&id)?;
        entry.used = self.tick;
        Some(entry.wave.clone().ok_or_else(|| entry.key.clone()))
    }

    /// Keep `wave` as the tables of `id`, or the tables another producer
    /// stored first.
    fn store(&mut self, id: NonZeroU64, wave: Arc<CustomWave>) -> Arc<CustomWave> {
        let Some(entry) = self.entries.get_mut(&id) else {
            return wave;
        };
        if let Some(stored) = &entry.wave {
            return stored.clone();
        }
        self.table_bytes += wave.bytes();
        entry.wave = Some(wave.clone());
        self.trim(id);
        wave
    }

    /// Drop idle tables, then idle lists, least recently used first, until
    /// both fit their budgets. Held tables stay, and so does `keep`.
    fn trim(&mut self, keep: NonZeroU64) {
        if self.table_bytes <= IDLE_TABLE_BUDGET_BYTES && self.list_bytes <= LIST_BUDGET_BYTES {
            return;
        }
        let mut idle: Vec<(u64, NonZeroU64)> = self
            .entries
            .iter()
            .filter(|(id, entry)| **id != keep && !entry.held())
            .map(|(id, entry)| (entry.used, *id))
            .collect();
        idle.sort_unstable();
        for (_, id) in &idle {
            if self.table_bytes <= IDLE_TABLE_BUDGET_BYTES {
                break;
            }
            if let Some(wave) = self.entries.get_mut(id).and_then(|entry| entry.wave.take()) {
                self.table_bytes -= wave.bytes();
            }
        }
        for (_, id) in &idle {
            if self.list_bytes <= LIST_BUDGET_BYTES {
                break;
            }
            self.forget(*id);
        }
    }

    fn forget(&mut self, id: NonZeroU64) {
        if let Some(entry) = self.entries.remove(&id) {
            self.ids.remove(&entry.key);
            self.list_bytes -= entry.key.bytes();
            if let Some(wave) = entry.wave {
                self.table_bytes -= wave.bytes();
            }
        }
    }
}

/// Forget a list, as its budget eventually does.
#[cfg(test)]
pub(crate) fn forget(partials: &PartialsControls) {
    registry().forget(partials.id);
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(Mutex::default);
    // The registry stays consistent across a panic: every update finishes
    // before the next lock.
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}
