/*
midi_in.rs - Live MIDI input state, shared between the driver and the queries
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! What a controller is doing right now, readable from a query.
//!
//! This module has no dependencies. It is in core so that the scheduler side
//! can see it, and the platform MIDI stack stays behind the `midi` feature of
//! `rustel-runtime`. This module does not decode MIDI messages: the driver
//! side passes scalars, and the message-type match happens where the decoded
//! event is. The split prevents a defect. strudel.cc reads `dataBytes[0]`
//! with no check on the message type, so a note-on for note 74 overwrites
//! CC74. A function that accepts only `(channel, controller, value)` cannot
//! make that mistake.
//!
//! Two threads meet here. The MIDI driver writes; the producer thread reads
//! while it queries patterns. The driver side never blocks or allocates. A
//! small producer-only mutex serialises placement of key hits so two concurrent
//! queries cannot assign the same note-on to different cycle positions.
//!
//! Memory ordering, which is not uniform and must not be made so:
//!
//! - **CC slots - `Relaxed`.** One writer per port (a driver delivers a port's
//!   callbacks in sequence), many readers, one self-contained scalar per
//!   location, nothing ordered against it. Per-location coherence holds even at
//!   `Relaxed`, so a load always returns a value that was really stored. Skew
//!   between two knobs is indistinguishable from turning them a microsecond
//!   apart.
//! - **The key ring - `SeqCst`.** A ring slot is revoked before it is recycled
//!   and published with its exact monotonically increasing publication id.
//!   Readers verify that id before and after reading the payload. This is
//!   stronger than the CC table deliberately: a single release/acquire cursor
//!   cannot protect a slot that the writer may already be recycling.
//!
//! One ring slot, where `id` is its publication id:
//!
//! ```text
//! driver, KeyRing::push            producer, KeyRing::select
//! 1. published[slot] = 0           a. published[slot] == id, else skip
//! 2. write stamp, epoch, note      b. read stamp, epoch, note
//! 3. published[slot] = id          c. published[slot] == id, else skip
//! 4. written = id
//! ```
//!
//! A recycle that starts between a and c changes `published[slot]`, so the
//! reader drops the payload it read.

use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};

/// How many distinct inputs one score may name.
///
/// A score is re-evaluated on every save, so an unbounded intern would let a
/// typo'd selector spawn a reader thread per keystroke.
pub const MAX_INPUT_PORTS: usize = 8;

/// Maximum UTF-8 bytes accepted in one score-controlled device selector.
///
/// Device matching happens later on the host, but the selector is retained by
/// both candidate and audible generations. Bound it before cloning so a score
/// cannot turn a harmless device name into unbounded retained memory.
pub const MAX_INPUT_SELECTOR_BYTES: usize = 1024;

/// How many note-ons are remembered.
pub const KEY_RING: usize = 512;

/// A note older than this is never placed. A producer stall must not end in a
/// burst of notes the player let go of seconds ago.
pub const KEY_STALE_NANOS: u64 = 2_000_000_000;

/// Nanoseconds since a process-wide epoch.
///
/// The driver stamps a note with this clock and the producer measures
/// staleness against it, so both sides must read the same clock. Two
/// `Instant`s taken in different modules differ by the time between their
/// creation, and that offset would make the staleness check wrong.
pub fn now_nanos() -> u64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos() as u64
}

/// One note-on selected for a query span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyHit {
    pub note: u8,
    pub velocity: u8,
    pub channel: u8,
    /// Cycle position, as an exact rational so a re-query reproduces it.
    pub num: i64,
    pub den: i64,
    /// How long before the note was placed the key was struck, in
    /// nanoseconds.
    ///
    /// A note can only sound at the frontier the scheduler has reached, and
    /// the frontier is always a little later than the key press. `num/den`
    /// is the frontier. This field is the distance back to the key press.
    /// A patterned note length is sampled at the key press time: a sample
    /// taken a few milliseconds late can fall on the other side of a gate
    /// change.
    ///
    /// Frozen at the first trigger query that assigns this press a cycle
    /// position. A later re-query of the same span (stack, jux, a refused
    /// tick, a live takeover) must not increase the lookback, or that length
    /// is sampled on the other side of the gate.
    pub struck_nanos_ago: u64,
}

/// One press that a query has placed, as [`KeyRing::placed`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedKey {
    /// Which press of its ring this is, counted from one. A later press has
    /// a higher number.
    pub press: u64,
    pub note: u8,
    /// Cycle position, as the query that placed the press spelled it.
    pub num: i64,
    pub den: i64,
}

/// The note-ons a controller has played, as a fixed ring.
#[derive(Clone, Copy, Default)]
struct KeyPlacement {
    publication: u64,
    epoch: u64,
    num: i64,
    den: i64,
    /// Lookback at the query that first placed this press. Recomputed from
    /// the current query clock it would grow on every re-select.
    ///
    /// `Default` is an empty slot (`den == 0`). A later forget that resets
    /// the slot therefore stores a fresh lookback on the re-claim, rather
    /// than inheriting the pin it just discarded.
    struck_nanos_ago: u64,
    /// A live replacement retired this pin: the outgoing score's rendition
    /// of it sounds before the takeover, so no later query may select the
    /// press again, and no forget may free it to be claimed a second time
    /// (see [`KeyRing::retire_backlog`]).
    retired: bool,
}

pub struct KeyRing {
    /// Exact publication id for each slot. Zero means the writer has revoked
    /// the slot while recycling it.
    published: [AtomicU64; KEY_RING],
    stamps: [AtomicU64; KEY_RING],
    /// Transport epoch in which the note-on arrived. A stop/restart advances
    /// the epoch so pre-stop key presses cannot fire in the restarted set.
    event_epochs: [AtomicU64; KEY_RING],
    /// `note << 16 | velocity << 8 | channel`, packed so one store publishes
    /// all three.
    notes: [AtomicU32; KEY_RING],
    /// Placement is producer-side state. The publication and epoch in each
    /// entry prevent a recycled slot from inheriting its predecessor's cycle
    /// position.
    placements: Mutex<[KeyPlacement; KEY_RING]>,
    written: AtomicU64,
    /// How many of the ring's presses were MUSICAL - not transport.
    ///
    /// The studio's at-once requery watches press counts (see `presses`),
    /// and a transport pad must not count: it launches a scene, and the
    /// requery it would otherwise arm lands milliseconds before the
    /// launch's own install - a second, truncated attack inside the
    /// restart's first beat. Musical presses and total presses are
    /// therefore counted apart, and the watcher sums only the musical
    /// ones. Written by the driver thread beside `written`.
    musical_written: AtomicU64,
    epoch: AtomicU64,
}

impl Default for KeyRing {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyRing {
    pub fn new() -> Self {
        Self {
            published: std::array::from_fn(|_| AtomicU64::new(0)),
            stamps: std::array::from_fn(|_| AtomicU64::new(0)),
            event_epochs: std::array::from_fn(|_| AtomicU64::new(0)),
            notes: std::array::from_fn(|_| AtomicU32::new(0)),
            placements: Mutex::new([KeyPlacement::default(); KEY_RING]),
            written: AtomicU64::new(0),
            musical_written: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
        }
    }

    /// DRIVER THREAD. A transport NoteOn is not musical: no query places or
    /// selects it, and it does not count toward a requery. The body is empty
    /// on purpose. The caller (`InputPort::observe_note_on`) keeps the press
    /// out of the ring. This method is the one named entry point for a
    /// future reader of transport presses.
    pub fn push_transport(&self) {}

    /// DRIVER THREAD. Wait-free, allocation-free, and DROP-OLDEST.
    ///
    /// The opposite of the output queue's refuse-when-full, and deliberately:
    /// there the queue IS the schedule and dropping the oldest drops a
    /// note-off. Here the notes being played NOW are the ones that matter, and
    /// replaying a backlog late just sounds broken.
    pub fn push(&self, now_nanos: u64, note: u8, velocity: u8, channel: u8) {
        // One driver callback stream writes a port in sequence. The exact
        // publication id (rather than only the cursor) lets readers detect a
        // slot recycled underneath them.
        let written = self.written.load(SeqCst);
        let publication = written.wrapping_add(1);
        let slot = (written as usize) % KEY_RING;
        self.published[slot].store(0, SeqCst);
        self.stamps[slot].store(now_nanos, SeqCst);
        self.event_epochs[slot].store(self.epoch.load(SeqCst), SeqCst);
        self.notes[slot].store(
            u32::from(note) << 16 | u32::from(velocity) << 8 | u32::from(channel),
            SeqCst,
        );
        self.published[slot].store(publication, SeqCst);
        self.written.store(publication, SeqCst);
        self.musical_written.fetch_add(1, SeqCst);
    }

    /// Start a fresh transport epoch and forget every pre-clear key hit.
    pub fn clear(&self) {
        self.epoch.fetch_add(1, SeqCst);
    }

    /// PRODUCER SIDE. At a live replacement's takeover, retire every press
    /// the outgoing score pinned before `takeover_cycle`, forget every pin
    /// at or after it, and keep every press no query has placed yet.
    ///
    /// A live replacement over a running transport must not inherit the
    /// outgoing score's key backlog: its horizon queries pinned a fast
    /// player's recent burst to cycle positions, and the incoming score
    /// would re-render every one of them through its own voices. Those pins
    /// sound where they are - the device keeps the outgoing generation up to
    /// the takeover - so they are retired: selected by nobody again, and
    /// kept from a later forget.
    ///
    /// A pin AT or AFTER the takeover names audio the device swap retires,
    /// and the outgoing score's horizon queries place a press at their span
    /// begin, the frontier, which is often past the takeover. Retiring those
    /// too meant the press was heard by nobody. They are forgotten instead,
    /// exactly as [`Self::forget_placements_from`] forgets them for a
    /// control re-query, so the incoming score claims them at its own first
    /// span and each is heard once.
    ///
    /// A press that landed after the outgoing score's last query - during
    /// the evaluation, the probe, the wait for the loop - was never placed,
    /// and stays for the incoming score as it is.
    ///
    /// `takeover_cycle` is a position on the mapping the pins were placed
    /// under: the outgoing score's, read before the replacement moves it.
    /// A takeover that is not a finite cycle retires every pin, so no press
    /// can be claimed twice.
    pub fn retire_backlog(&self, takeover_cycle: f64) {
        let selected_epoch = self.epoch.load(SeqCst);
        let mut placements = self.placements.lock().unwrap_or_else(|e| e.into_inner());
        for placement in placements.iter_mut() {
            if placement.den == 0 || placement.epoch != selected_epoch || placement.retired {
                continue;
            }
            let at = placement.num as f64 / placement.den as f64;
            if takeover_cycle.is_finite() && at >= takeover_cycle {
                *placement = KeyPlacement::default();
            } else {
                placement.retired = true;
            }
        }
    }

    /// PRODUCER THREAD. Forget every placement at or after `cursor` (a cycle
    /// position, as an f64), before a control re-query asks the pattern again
    /// from that cursor.
    ///
    /// The first trigger query that selects a press places it, and the
    /// producer's horizon fill queries far ahead. With a heavy sibling
    /// pattern in the same stack, a horizon query can run before the
    /// close-to-now re-query that the press armed. The press is then pinned
    /// about half a second in the future, and the re-query sounds nothing
    /// because its span does not reach the pin. The next save retires that
    /// generation, and the press is never heard.
    ///
    /// Forgetting the pin at a re-query boundary cannot sound the press
    /// twice. The device swap retires everything the old generation scheduled
    /// at or after the takeover frame, so the pin points at a schedule that
    /// never plays. The re-query's own pass then claims the press again at
    /// its span begin, which is the takeover frame and the earliest moment
    /// the device can sound it. Later horizon refills still cover it. A pin
    /// before the cursor is kept: those frames are already in the device's
    /// lookahead and keep playing across the takeover, so a second claim
    /// would sound the press twice.
    pub fn forget_placements_from(&self, cursor: f64) {
        if !cursor.is_finite() {
            return;
        }
        let selected_epoch = self.epoch.load(SeqCst);
        let mut placements = self.placements.lock().unwrap_or_else(|e| e.into_inner());
        for placement in placements.iter_mut() {
            // A retired pin already sounded under the score it belonged to:
            // freeing it would let this re-query claim the press again.
            if placement.den == 0 || placement.epoch != selected_epoch || placement.retired {
                continue;
            }
            if placement.num as f64 / placement.den as f64 >= cursor {
                *placement = KeyPlacement::default();
            }
        }
    }

    /// PRODUCER SIDE. Append the presses that a query has placed and can
    /// select again: those of this transport epoch that no replacement has
    /// retired. It only reads: it places nothing and forgets nothing.
    ///
    /// A trigger query places every press that it can select and finds not
    /// placed. So when a press has been placed, each earlier press of the
    /// ring has been placed too, or no query can select it.
    pub fn placed(&self, out: &mut Vec<PlacedKey>) {
        let selected_epoch = self.epoch.load(SeqCst);
        let placements = self.placements.lock().unwrap_or_else(|e| e.into_inner());
        for (slot, placement) in placements.iter().enumerate() {
            if placement.den == 0 || placement.epoch != selected_epoch || placement.retired {
                continue;
            }
            // The driver can recycle the slot during this read, as in
            // `select`: the note then belongs to another press.
            if self.published[slot].load(SeqCst) != placement.publication {
                continue;
            }
            let packed = self.notes[slot].load(SeqCst);
            if self.published[slot].load(SeqCst) != placement.publication {
                continue;
            }
            out.push(PlacedKey {
                press: placement.publication,
                note: (packed >> 16) as u8,
                num: placement.num,
                den: placement.den,
            });
        }
    }

    /// How many musical presses this ring has taken since it was made.
    ///
    /// The producer watches this count for a change. A key press is heard
    /// only when the scheduler next queries the pattern, and the scheduler
    /// queries far ahead. A new press tells the studio to query again from
    /// close to now.
    ///
    /// Transport pads (notes bound to scene launch) are not counted. A pad
    /// press is a transport button. The requery that this counter arms would
    /// land next to the launch's own install, and the restart would cut its
    /// new attack short. See [`InputPort::observe_note_on`].
    pub fn presses(&self) -> u64 {
        self.musical_written.load(SeqCst)
    }

    pub fn epoch(&self) -> u64 {
        self.epoch.load(SeqCst)
    }

    /// PRODUCER THREAD. Selects presses and never dequeues them.
    ///
    /// Upstream clears its queue at the end of every query. This runtime
    /// queries a child more than once per pass for
    /// `stack`/`jux`/`superimpose`/`off`/`every`, and re-queries a span after
    /// a refused tick or a live takeover. A drain would silence every branch
    /// of a stack but the first.
    ///
    /// `place` is `Some` only for a real trigger query. An inspection query
    /// (`rustel query`) selects already-placed entries and places nothing, so it
    /// has no side effect at all and stays deterministic.
    pub fn select(
        &self,
        begin: f64,
        end: f64,
        now_nanos: u64,
        place: Option<(i64, i64)>,
        out: &mut Vec<KeyHit>,
    ) {
        self.select_with_hook(begin, end, now_nanos, place, out, || {});
    }

    fn select_with_hook<F>(
        &self,
        begin: f64,
        end: f64,
        now_nanos: u64,
        place: Option<(i64, i64)>,
        out: &mut Vec<KeyHit>,
        mut after_payload: F,
    ) where
        F: FnMut(),
    {
        let selected_epoch = self.epoch.load(SeqCst);
        let written = self.written.load(SeqCst);
        let oldest = written.saturating_sub(KEY_RING as u64);
        let mut selected = Vec::new();
        let mut placements = self.placements.lock().unwrap_or_else(|e| e.into_inner());
        for index in oldest..written {
            let slot = (index as usize) % KEY_RING;
            let publication = index.wrapping_add(1);
            if self.published[slot].load(SeqCst) != publication {
                continue;
            }
            let stamp = self.stamps[slot].load(SeqCst);
            let event_epoch = self.event_epochs[slot].load(SeqCst);
            let packed = self.notes[slot].load(SeqCst);
            after_payload();
            if self.published[slot].load(SeqCst) != publication
                || event_epoch != selected_epoch
                || self.epoch.load(SeqCst) != selected_epoch
            {
                continue;
            }

            let placement = &mut placements[slot];
            let (num, den, struck_nanos_ago) = if placement.publication == publication
                && placement.epoch == selected_epoch
                && placement.den != 0
            {
                if placement.retired {
                    continue;
                }
                (placement.num, placement.den, placement.struck_nanos_ago)
            } else {
                let Some((place_num, place_den)) = place else {
                    continue;
                };
                if place_den == 0 {
                    continue;
                }
                if now_nanos.saturating_sub(stamp) > KEY_STALE_NANOS {
                    continue;
                }
                if self.published[slot].load(SeqCst) != publication
                    || self.epoch.load(SeqCst) != selected_epoch
                {
                    continue;
                }
                let struck_nanos_ago = now_nanos.saturating_sub(stamp);
                *placement = KeyPlacement {
                    publication,
                    epoch: selected_epoch,
                    num: place_num,
                    den: place_den,
                    struck_nanos_ago,
                    retired: false,
                };
                (place_num, place_den, struck_nanos_ago)
            };
            if den == 0 {
                continue;
            }
            let at = num as f64 / den as f64;
            if at < begin || at >= end {
                continue;
            }
            if self.published[slot].load(SeqCst) != publication
                || self.epoch.load(SeqCst) != selected_epoch
            {
                continue;
            }
            selected.push(KeyHit {
                note: (packed >> 16) as u8,
                velocity: (packed >> 8) as u8,
                channel: packed as u8,
                num,
                den,
                struck_nanos_ago,
            });
        }
        // If a clear crossed this selection, publish none of the old epoch --
        // never a prefix of a chord from before transport restart.
        if self.epoch.load(SeqCst) != selected_epoch {
            return;
        }
        out.extend(selected);
        // Stable order so a chord arrives the same way twice.
        out.sort_by_key(|hit| {
            let position = if hit.den == 0 {
                0
            } else {
                i128::from(hit.num) * 1_000_000 / i128::from(hit.den)
            };
            (position, hit.note, hit.channel)
        });
    }
}

/// One named MIDI input's live state.
pub struct InputPort {
    /// The selector the score wrote. Host side only, never on the hot path.
    pub selector: String,
    /// CC value PLUS ONE, across all channels.
    ///
    /// Zero means "never received". Every wire value 0..=127 is legal, so there
    /// is no spare in-band code; the +1 buys a never-seen flag with no second
    /// array. Removing it shifts every knob by 1/127.
    any: [AtomicU8; 128],
    /// The same, per wire channel: `chan[c - 1][n]` for channels 1..=16.
    chan: [[AtomicU8; 128]; 16],
    pub keys: KeyRing,
    messages: AtomicU64,
    /// The notes bound to scene launch: one word per note, bit `c` set for
    /// channel `c` (0..=16) and bit [`TRANSPORT_PAD_ANY_CHANNEL`] for a
    /// binding on every channel. Host side keeps it current; the driver
    /// thread reads one word - no lock, no allocation, nothing it can wait
    /// on behind the host.
    ///
    /// A launch pad is a transport button, not a key, so its press must not
    /// enter the musical ring. The press-count watcher would arm a requery
    /// that lands milliseconds before the launch's own install. The
    /// from-zero flip would then cut the new placement short and attack it
    /// again. A bound pad only launches its scene: it never sounds through
    /// `midikeys`, even in a scene that names the same note.
    transport_pads: [AtomicU32; 128],
}

/// The bit of a transport-pad word that binds every channel of its note.
pub const TRANSPORT_PAD_ANY_CHANNEL: u32 = 17;

/// The per-note transport-pad words for a set of `(note, channel)` bindings,
/// where a channel of `0xFF` binds every channel of the note. Channels above
/// 16 (not a MIDI channel) bind nothing.
pub fn transport_pad_words(pads: &[(u8, u8)]) -> [u32; 128] {
    let mut words = [0u32; 128];
    for &(note, channel) in pads {
        let bit = match channel {
            0xFF => TRANSPORT_PAD_ANY_CHANNEL,
            0..=16 => u32::from(channel),
            _ => continue,
        };
        words[usize::from(note & 0x7f)] |= 1 << bit;
    }
    words
}

impl InputPort {
    pub fn new(selector: String) -> Self {
        Self {
            selector,
            // `[AtomicU8::new(0); 128]` does not compile - AtomicU8 is !Copy.
            any: std::array::from_fn(|_| AtomicU8::new(0)),
            chan: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU8::new(0))),
            keys: KeyRing::new(),
            messages: AtomicU64::new(0),
            transport_pads: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    /// DRIVER THREAD. Two stores, no allocation, no lock.
    pub fn observe_control_change(&self, channel: u8, controller: u8, value: u8) {
        self.messages.fetch_add(1, Relaxed);
        let slot = usize::from(controller & 0x7f);
        let raw = (value & 0x7f) + 1;
        self.any[slot].store(raw, Relaxed);
        if (1..=16).contains(&channel) {
            self.chan[usize::from(channel - 1)][slot].store(raw, Relaxed);
        }
    }

    /// DRIVER THREAD.
    pub fn observe_note_on(&self, now_nanos: u64, channel: u8, note: u8, velocity: u8) {
        self.messages.fetch_add(1, Relaxed);
        // A launch pad is a transport button, not a note: it must not
        // sound through the keys ring (see `transport_pads`), and its
        // press must not arm the at-once requery that would chop the
        // launch's own fresh attack. Everything else is musical.
        if self.is_transport_pad(note, channel) {
            self.keys.push_transport();
            return;
        }
        self.keys.push(now_nanos, note, velocity, channel);
    }

    /// HOST SIDE. Replace the launch-pad set for this port. Idempotent;
    /// called when scene pad bindings change.
    pub fn set_transport_pads(&self, pads: &[(u8, u8)]) {
        self.store_transport_pad_words(&transport_pad_words(pads));
    }

    /// HOST SIDE. [`Self::set_transport_pads`] with the words already built.
    pub fn store_transport_pad_words(&self, words: &[u32; 128]) {
        for (slot, word) in self.transport_pads.iter().zip(words) {
            slot.store(*word, Relaxed);
        }
    }

    /// DRIVER THREAD. Whether this (note, channel) is bound to scene launch.
    /// One load.
    fn is_transport_pad(&self, note: u8, channel: u8) -> bool {
        let word = self.transport_pads[usize::from(note & 0x7f)].load(Relaxed);
        let any = 1 << TRANSPORT_PAD_ANY_CHANNEL;
        let exact = if channel <= 16 { 1 << channel } else { 0 };
        word & (any | exact) != 0
    }

    /// PRODUCER THREAD, on the query hot path. One load.
    ///
    /// Always returns a number, never `None` or NaN. A missing value would
    /// reach the score as `undefined`. `reify(undefined)` is silence, and the
    /// join against silence deletes the note being modulated, so an unplugged
    /// controller would silence the set. NaN would reach the voice as a NaN
    /// cutoff.
    ///
    /// `chan == 0` means `cc(n)` - any channel.
    pub fn read_cc(&self, cc: i32, chan: i32) -> f64 {
        let Ok(slot) = usize::try_from(cc) else {
            return 0.0;
        };
        if slot > 127 {
            return 0.0;
        }
        let raw = match chan {
            0 => self.any[slot].load(Relaxed),
            channel @ 1..=16 => self.chan[(channel - 1) as usize][slot].load(Relaxed),
            _ => return 0.0,
        };
        if raw == 0 {
            0.0
        } else {
            f64::from(raw - 1) / 127.0
        }
    }

    /// Whether this control has ever been received, which a zero value cannot
    /// tell you on its own.
    pub fn has_been_touched(&self, cc: i32, chan: i32) -> bool {
        let Ok(slot) = usize::try_from(cc) else {
            return false;
        };
        if slot > 127 {
            return false;
        }
        match chan {
            0 => self.any[slot].load(Relaxed) != 0,
            channel @ 1..=16 => self.chan[(channel - 1) as usize][slot].load(Relaxed) != 0,
            _ => false,
        }
    }

    pub fn messages(&self) -> u64 {
        self.messages.load(Relaxed)
    }

    pub fn clear_keys(&self) {
        self.keys.clear();
    }

    /// See [`KeyRing::retire_backlog`].
    pub fn retire_key_backlog(&self, takeover_cycle: f64) {
        self.keys.retire_backlog(takeover_cycle);
    }
}

/// Every input a score has named.
///
/// One accepted score generation's input set.
#[derive(Default)]
struct InputGeneration {
    id: u64,
    ports: Vec<Arc<InputPort>>,
}

struct InputBusState {
    generations: Vec<InputGeneration>,
    current_id: u64,
    /// The launch-pad words every port is given, including ports created or
    /// published after the set was sent: a new port is born knowing which
    /// notes are transport buttons, so nothing has to re-state the set to
    /// the bus on every engine turn.
    launch_pad_words: [u32; 128],
    /// Exact generation the live host last reported as device-audible. Before
    /// the first `snapshot_for`, no listener can be attached and only the
    /// current Session generation needs retaining.
    host_audible_id: Option<u64>,
}

impl Default for InputBusState {
    fn default() -> Self {
        Self {
            generations: vec![InputGeneration::default()],
            current_id: 0,
            launch_pad_words: [0; 128],
            host_audible_id: None,
        }
    }
}

#[derive(Default)]
pub struct InputBus {
    state: Mutex<InputBusState>,
}

impl InputBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a selector to its port, creating it on first use.
    ///
    /// Called at SCORE EVALUATION time only - the query path never takes this
    /// lock, because `midin` hands out a handle once and the handle indexes a
    /// mirror on the runtime side.
    pub fn intern(&self, selector: &str) -> Option<(usize, Arc<InputPort>)> {
        if selector.len() > MAX_INPUT_SELECTOR_BYTES {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let current_id = state.current_id;
        let current = state
            .generations
            .iter_mut()
            .find(|generation| generation.id == current_id)?;
        if let Some(index) = current
            .ports
            .iter()
            .position(|port| port.selector == selector)
        {
            return Some((index, Arc::clone(&current.ports[index])));
        }
        if current.ports.len() >= MAX_INPUT_PORTS {
            return None;
        }
        let port = Arc::new(InputPort::new(selector.to_string()));
        port.store_transport_pad_words(&state.launch_pad_words);
        let current = state
            .generations
            .iter_mut()
            .find(|generation| generation.id == current_id)?;
        current.ports.push(Arc::clone(&port));
        Some((current.ports.len() - 1, port))
    }

    /// Atomically publish one accepted score's complete input set.
    pub fn commit_generation(
        &self,
        generation: u64,
        ports: Vec<Arc<InputPort>>,
    ) -> Result<(), String> {
        if ports.len() > MAX_INPUT_PORTS {
            return Err(format!(
                "at most {MAX_INPUT_PORTS} MIDI inputs may be named by one score"
            ));
        }
        for port in &ports {
            if port.selector.len() > MAX_INPUT_SELECTOR_BYTES {
                return Err(format!(
                    "a MIDI input selector may contain at most {MAX_INPUT_SELECTOR_BYTES} UTF-8 bytes"
                ));
            }
        }
        for (index, port) in ports.iter().enumerate() {
            if ports[..index]
                .iter()
                .any(|previous| previous.selector == port.selector)
            {
                return Err("a MIDI input generation contains duplicate selectors".into());
            }
        }

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for port in &ports {
            port.store_transport_pad_words(&state.launch_pad_words);
        }
        if let Some(audible_id) = state.host_audible_id {
            // There can be arbitrarily many accepted-but-never-audible scores
            // during a rapid reload or a run of failed prefill windows. Only
            // the device-audible generation and this newest Session candidate
            // can still be queried. Supersede every older candidate before
            // publishing the replacement, so publication cannot fail after
            // QuickJS has already installed the candidate wrapper.
            state
                .generations
                .retain(|retained| retained.id == audible_id);
        } else {
            // Query/render/non-live Session users never construct MidiInputs.
            // With no device-audible generation to protect, retain only the
            // newest accepted score.
            state.generations.clear();
        }
        if let Some(retained) = state
            .generations
            .iter_mut()
            .find(|retained| retained.id == generation)
        {
            retained.ports = ports;
            state.current_id = generation;
            return Ok(());
        }
        state.generations.push(InputGeneration {
            id: generation,
            ports,
        });
        state.current_id = generation;
        Ok(())
    }

    /// Give the currently accepted ports the scheduler's new generation id
    /// without resolving or opening them again.
    pub fn republish_current_generation(&self, generation: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let current_id = state.current_id;
        if current_id == generation {
            return;
        }
        if !state
            .generations
            .iter()
            .any(|retained| retained.id == current_id)
        {
            return;
        }

        state
            .generations
            .retain(|retained| retained.id != generation);
        let current_index = state
            .generations
            .iter()
            .position(|retained| retained.id == current_id)
            .expect("current generation retained above");
        if state.host_audible_id == Some(current_id) {
            // Requery/slider publication advances the Session generation
            // before device prefill. Preserve the old id for the still-audible
            // graph and give the candidate a pointer-identical Arc snapshot.
            let ports = state.generations[current_index]
                .ports
                .iter()
                .map(Arc::clone)
                .collect();
            state.generations.push(InputGeneration {
                id: generation,
                ports,
            });
        } else {
            state.generations[current_index].id = generation;
        }
        state.current_id = generation;
    }

    /// Inputs needed by either the sounding graph or the accepted candidate.
    pub fn snapshot_for(
        &self,
        audible_generation: u64,
        session_generation: u64,
    ) -> Vec<Arc<InputPort>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.host_audible_id = Some(audible_generation);
        let mut ports = Vec::new();
        for requested in [audible_generation, session_generation] {
            if let Some(generation) = state
                .generations
                .iter()
                .find(|generation| generation.id == requested)
            {
                for port in &generation.ports {
                    if !ports
                        .iter()
                        .any(|retained: &Arc<InputPort>| retained.selector == port.selector)
                    {
                        ports.push(Arc::clone(port));
                    }
                }
            }
        }
        // The host has now told us the only generations that can still be
        // queried. Drop superseded candidates after taking the Arc snapshot,
        // with no device work under this lock.
        state.generations.retain(|generation| {
            generation.id == audible_generation || generation.id == session_generation
        });
        if state
            .generations
            .iter()
            .any(|generation| generation.id == session_generation)
        {
            state.current_id = session_generation;
        }
        ports
    }

    /// Every port, with the lock released before the caller does anything slow
    /// with them - opening a device must never happen under this lock.
    pub fn snapshot(&self) -> Vec<Arc<InputPort>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .generations
            .iter()
            .find(|generation| generation.id == state.current_id)
            .map(|generation| generation.ports.iter().map(Arc::clone).collect())
            .unwrap_or_default()
    }

    pub fn find_retained(&self, selector: &str) -> Option<Arc<InputPort>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let current = state
            .generations
            .iter()
            .find(|generation| generation.id == state.current_id)
            .into_iter()
            .flat_map(|generation| generation.ports.iter());
        current
            .chain(
                state
                    .generations
                    .iter()
                    .rev()
                    .filter(|generation| generation.id != state.current_id)
                    .flat_map(|generation| generation.ports.iter()),
            )
            .find(|port| port.selector == selector)
            .map(Arc::clone)
    }

    pub fn find(&self, selector: &str) -> Option<Arc<InputPort>> {
        self.find_retained(selector)
    }

    pub fn len(&self) -> usize {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .generations
            .iter()
            .find(|generation| generation.id == state.current_id)
            .map_or(0, |generation| generation.ports.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear_keys(&self) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut cleared: Vec<*const InputPort> = Vec::new();
        for port in state
            .generations
            .iter()
            .flat_map(|generation| generation.ports.iter())
        {
            let pointer = Arc::as_ptr(port);
            if cleared.contains(&pointer) {
                continue;
            }
            port.clear_keys();
            cleared.push(pointer);
        }
    }

    /// Retire the key backlog pinned before `takeover_cycle` on every port
    /// the sounding or accepted score can query, forgetting the pins at or
    /// after it and keeping presses no query has placed yet - the live
    /// replacement side of [`KeyRing::retire_backlog`]. Deduped by pointer
    /// like [`Self::clear_keys`].
    pub fn retire_key_backlog(&self, takeover_cycle: f64) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut retired: Vec<*const InputPort> = Vec::new();
        for port in state
            .generations
            .iter()
            .flat_map(|generation| generation.ports.iter())
        {
            let pointer = Arc::as_ptr(port);
            if retired.contains(&pointer) {
                continue;
            }
            port.retire_key_backlog(takeover_cycle);
            retired.push(pointer);
        }
    }

    /// Forget every key placement at or after `cursor` on every port the
    /// sounding or accepted score can query - the re-query side of
    /// [`KeyRing::forget_placements_from`]. Ports that no accepted generation
    /// names went away with their score, so their pins need no cleanup.
    pub fn forget_key_placements_from(&self, cursor: f64) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut forgotten: Vec<*const InputPort> = Vec::new();
        for port in state
            .generations
            .iter()
            .flat_map(|generation| generation.ports.iter())
        {
            let pointer = Arc::as_ptr(port);
            if forgotten.contains(&pointer) {
                continue;
            }
            port.keys.forget_placements_from(cursor);
            forgotten.push(pointer);
        }
    }

    /// The presses that a query has placed and can select again, for each
    /// port the sounding or accepted score can query: the bus side of
    /// [`KeyRing::placed`]. Deduped by pointer like [`Self::clear_keys`]. A
    /// port with no such press is left out.
    pub fn placed_keys(&self) -> Vec<(Arc<InputPort>, Vec<PlacedKey>)> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut placed: Vec<(Arc<InputPort>, Vec<PlacedKey>)> = Vec::new();
        let mut read: Vec<*const InputPort> = Vec::new();
        for port in state
            .generations
            .iter()
            .flat_map(|generation| generation.ports.iter())
        {
            let pointer = Arc::as_ptr(port);
            if read.contains(&pointer) {
                continue;
            }
            read.push(pointer);
            let mut keys = Vec::new();
            port.keys.placed(&mut keys);
            if !keys.is_empty() {
                placed.push((Arc::clone(port), keys));
            }
        }
        placed
    }

    /// HOST SIDE. Tell every live port which notes are bound to scene
    /// launch, so the driver can keep those presses out of the musical
    /// ring. Deduped by pointer exactly like the forget above: the same
    /// port can appear in the audible and the accepted candidate's
    /// generations, and the set is per-port state, not per-generation.
    pub fn set_launch_pads(&self, pads: &[(u8, u8)]) {
        let words = transport_pad_words(pads);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.launch_pad_words = words;
        // A port shared by two generations is stored twice; the words are
        // the same, so that is only a repeated store.
        for port in state
            .generations
            .iter()
            .flat_map(|generation| generation.ports.iter())
        {
            port.store_transport_pad_words(&words);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn an_untouched_control_reads_zero() {
        let port = InputPort::new("x".into());
        assert_eq!(port.read_cc(74, 0), 0.0);
        assert!(!port.has_been_touched(74, 0));
    }

    /// The +1 encoding earns its keep here: a knob really at 0 and a knob never
    /// touched read the same value but are not the same state.
    #[test]
    fn a_received_control_of_zero_reads_zero_but_counts_as_touched() {
        let port = InputPort::new("x".into());
        port.observe_control_change(1, 74, 0);
        assert_eq!(port.read_cc(74, 0), 0.0);
        assert!(port.has_been_touched(74, 0));
    }

    #[test]
    fn a_received_control_normalises_to_zero_to_one() {
        let port = InputPort::new("x".into());
        port.observe_control_change(1, 74, 127);
        assert_eq!(port.read_cc(74, 0), 1.0, "127 must be exactly 1.0");
        port.observe_control_change(1, 74, 64);
        assert!((port.read_cc(74, 0) - 64.0 / 127.0).abs() < 1e-12);
    }

    #[test]
    fn channel_zero_reads_any_channel_and_a_filter_ignores_the_others() {
        let port = InputPort::new("x".into());
        port.observe_control_change(3, 74, 127);
        assert_eq!(port.read_cc(74, 0), 1.0, "any-channel must see it");
        assert_eq!(port.read_cc(74, 3), 1.0, "its own channel must see it");
        assert_eq!(port.read_cc(74, 2), 0.0, "another channel must not");
    }

    #[test]
    fn an_out_of_range_control_or_channel_reads_zero_not_nan() {
        let port = InputPort::new("x".into());
        for (cc, chan) in [(-1, 0), (128, 0), (9999, 0), (74, -1), (74, 17)] {
            let value = port.read_cc(cc, chan);
            assert!(value.is_finite(), "cc({cc},{chan}) was not finite");
            assert_eq!(value, 0.0);
        }
    }

    #[test]
    fn interning_the_same_selector_twice_returns_the_same_port_and_handle() {
        let bus = InputBus::new();
        let (first_index, first) = bus.intern("Minilab").unwrap();
        first.observe_control_change(1, 74, 100);
        let (second_index, second) = bus.intern("Minilab").unwrap();
        assert_eq!(first_index, second_index);
        assert_eq!(
            second.read_cc(74, 0),
            first.read_cc(74, 0),
            "a re-evaluation must not reset the knob"
        );
        assert_eq!(bus.len(), 1);
    }

    #[test]
    fn interning_past_the_port_limit_is_refused_not_unbounded() {
        let bus = InputBus::new();
        for index in 0..MAX_INPUT_PORTS {
            assert!(bus.intern(&format!("device-{index}")).is_some());
        }
        assert!(
            bus.intern("one-too-many").is_none(),
            "a typo per save must not open a port per save"
        );
        assert_eq!(bus.len(), MAX_INPUT_PORTS);
    }

    #[test]
    fn direct_generation_commit_refuses_an_oversized_selector() {
        let bus = InputBus::new();
        let oversized = "x".repeat(MAX_INPUT_SELECTOR_BYTES + 1);
        let port = Arc::new(InputPort::new(oversized));
        assert!(bus.commit_generation(1, vec![port]).is_err());
        assert!(bus.is_empty(), "a refused generation mutated the bus");
    }

    #[test]
    fn accepted_generations_keep_only_the_audible_candidate_union() {
        let bus = InputBus::new();
        let shared = Arc::new(InputPort::new("shared".into()));
        let audible = Arc::new(InputPort::new("audible".into()));
        let candidate = Arc::new(InputPort::new("candidate".into()));
        bus.commit_generation(1, vec![Arc::clone(&audible), Arc::clone(&shared)])
            .unwrap();
        bus.snapshot_for(1, 1);
        bus.commit_generation(2, vec![Arc::clone(&shared), Arc::clone(&candidate)])
            .unwrap();
        let retained = bus.snapshot_for(1, 2);
        assert_eq!(retained.len(), 3);
        assert!(retained.iter().any(|port| Arc::ptr_eq(port, &audible)));
        assert!(retained.iter().any(|port| Arc::ptr_eq(port, &shared)));
        assert!(retained.iter().any(|port| Arc::ptr_eq(port, &candidate)));
    }

    #[test]
    fn rollback_preserves_the_generation_that_is_still_audible() {
        let bus = InputBus::new();
        let audible = Arc::new(InputPort::new("audible".into()));
        bus.commit_generation(10, vec![Arc::clone(&audible)])
            .unwrap();
        bus.snapshot_for(10, 10);
        bus.commit_generation(
            11,
            vec![Arc::new(InputPort::new("refused-candidate".into()))],
        )
        .unwrap();
        let retained = bus.snapshot_for(10, 10);
        assert_eq!(retained.len(), 1);
        assert!(Arc::ptr_eq(&retained[0], &audible));
    }

    #[test]
    fn scheduler_only_requery_republishes_the_active_ports() {
        let bus = InputBus::new();
        let port = Arc::new(InputPort::new("controller".into()));
        bus.commit_generation(4, vec![Arc::clone(&port)]).unwrap();
        bus.republish_current_generation(5);
        let retained = bus.snapshot_for(5, 5);
        assert_eq!(retained.len(), 1);
        assert!(Arc::ptr_eq(&retained[0], &port));
    }

    #[test]
    fn sequential_generations_are_bounded_per_score_not_per_process() {
        let bus = InputBus::new();
        for generation in 1..=MAX_INPUT_PORTS as u64 * 4 {
            bus.commit_generation(
                generation,
                vec![Arc::new(InputPort::new(format!("device-{generation}")))],
            )
            .unwrap();
            assert_eq!(bus.len(), 1);
            assert!(
                bus.snapshot_for(generation.saturating_sub(1), generation)
                    .len()
                    <= 2
            );
        }
    }

    #[test]
    fn rapid_supersession_keeps_the_exact_device_audible_generation() {
        let bus = InputBus::new();
        let first = Arc::new(InputPort::new("first-audible".into()));
        let second = Arc::new(InputPort::new("second-candidate".into()));
        let third = Arc::new(InputPort::new("third-candidate".into()));
        bus.commit_generation(1, vec![Arc::clone(&first)]).unwrap();
        // This is the live host confirming generation 1 as device-audible.
        bus.snapshot_for(1, 1);
        bus.commit_generation(2, vec![second]).unwrap();
        bus.commit_generation(3, vec![Arc::clone(&third)]).unwrap();

        let retained = bus.snapshot_for(1, 3);
        assert_eq!(retained.len(), 2);
        assert!(retained.iter().any(|port| Arc::ptr_eq(port, &first)));
        assert!(retained.iter().any(|port| Arc::ptr_eq(port, &third)));
        assert!(bus.find_retained("second-candidate").is_none());
    }

    #[test]
    fn host_managed_rapid_supersession_is_infallible_and_bounded_to_two_generations() {
        let bus = InputBus::new();
        let audible = Arc::new(InputPort::new("audible".into()));
        bus.commit_generation(1, vec![Arc::clone(&audible)])
            .unwrap();
        bus.snapshot_for(1, 1);

        for generation in 2..=10_001 {
            bus.commit_generation(
                generation,
                vec![Arc::new(InputPort::new(format!("candidate-{generation}")))],
            )
            .expect("superseding an unpublished candidate must not consume capacity");
            let state = bus.state.lock().unwrap_or_else(|error| error.into_inner());
            assert_eq!(state.generations.len(), 2);
            assert!(state.generations.iter().any(|retained| retained.id == 1));
            assert!(
                state
                    .generations
                    .iter()
                    .any(|retained| retained.id == generation)
            );
        }

        let retained = bus.snapshot_for(1, 10_001);
        assert_eq!(retained.len(), 2);
        assert!(retained.iter().any(|port| Arc::ptr_eq(port, &audible)));
        assert!(
            retained
                .iter()
                .any(|port| port.selector == "candidate-10001")
        );
    }

    #[test]
    fn republishing_an_audible_generation_keeps_both_ids_until_cutover() {
        let bus = InputBus::new();
        let audible = Arc::new(InputPort::new("controller".into()));
        bus.commit_generation(20, vec![Arc::clone(&audible)])
            .unwrap();
        bus.snapshot_for(20, 20);

        bus.republish_current_generation(21);
        {
            let state = bus.state.lock().unwrap_or_else(|error| error.into_inner());
            assert_eq!(state.generations.len(), 2);
            let old = state
                .generations
                .iter()
                .find(|generation| generation.id == 20)
                .unwrap();
            let new = state
                .generations
                .iter()
                .find(|generation| generation.id == 21)
                .unwrap();
            assert!(Arc::ptr_eq(&old.ports[0], &new.ports[0]));
        }

        let candidate = Arc::new(InputPort::new("replacement".into()));
        bus.commit_generation(22, vec![Arc::clone(&candidate)])
            .unwrap();
        let through_cutover = bus.snapshot_for(20, 22);
        assert_eq!(through_cutover.len(), 2);
        assert!(
            through_cutover
                .iter()
                .any(|port| Arc::ptr_eq(port, &audible))
        );
        assert!(
            through_cutover
                .iter()
                .any(|port| Arc::ptr_eq(port, &candidate))
        );

        let after_cutover = bus.snapshot_for(22, 22);
        assert_eq!(after_cutover.len(), 1);
        assert!(Arc::ptr_eq(&after_cutover[0], &candidate));
        let state = bus.state.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(state.generations.len(), 1);
        assert_eq!(state.generations[0].id, 22);
    }

    #[test]
    fn non_live_reloads_keep_only_the_latest_generation() {
        let bus = InputBus::new();
        for generation in 1..=10_000 {
            bus.commit_generation(
                generation,
                vec![Arc::new(InputPort::new(format!("offline-{generation}")))],
            )
            .expect("a non-live reload must not exhaust a live cutover queue");
        }
        assert_eq!(bus.len(), 1);
        let state = bus.state.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(state.generations.len(), 1);
        assert_eq!(state.generations[0].id, 10_000);
    }

    fn place(ring: &KeyRing, begin: f64, end: f64, num: i64, den: i64) -> Vec<KeyHit> {
        let mut out = Vec::new();
        ring.select(begin, end, 0, Some((num, den)), &mut out);
        out
    }

    #[test]
    fn the_key_ring_overwrites_oldest_when_full() {
        let ring = KeyRing::new();
        for index in 0..(KEY_RING + 10) {
            ring.push(0, (index % 128) as u8, 100, 1);
        }
        let hits = place(&ring, 0.0, 1.0, 0, 1);
        assert_eq!(hits.len(), KEY_RING, "the ring must stay bounded");
    }

    /// A span queried twice must give the same haps. Stack, jux and every
    /// re-query a child, and a refused tick re-queries the same span.
    #[test]
    fn a_placed_key_is_reproduced_identically_by_a_second_select() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        let first = place(&ring, 0.0, 1.0, 1, 4);
        // A LATER placement offer must not move an already-placed note.
        let second = place(&ring, 0.0, 1.0, 3, 4);
        assert_eq!(first, second, "a re-query moved the note");
        assert_eq!(first.len(), 1);
        assert_eq!((first[0].num, first[0].den), (1, 4));
    }

    /// The lookback to the finger is frozen at placement, the way the
    /// cycle position is. A later select's clock must not walk it back:
    /// `query_midi_keys` samples a patterned length at `at - lookback`,
    /// and a few milliseconds is a gate (`keys("0.01 1")`).
    #[test]
    fn a_placed_key_keeps_the_lookback_it_was_placed_with() {
        let ring = KeyRing::new();
        // Struck at 1ms, first asked for at 6ms: five milliseconds back.
        ring.push(1_000_000, 60, 100, 1);
        let mut first = Vec::new();
        ring.select(0.0, 1.0, 6_000_000, Some((1, 2)), &mut first);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].struck_nanos_ago, 5_000_000);
        assert_eq!((first[0].num, first[0].den), (1, 2));

        // Two hundred milliseconds later the same press is still that
        // press: the cycle did not move, and neither did the finger.
        let mut second = Vec::new();
        ring.select(0.0, 1.0, 200_000_000, Some((3, 4)), &mut second);
        assert_eq!(
            second, first,
            "a later re-query recomputed the distance back to the finger"
        );
    }

    #[test]
    fn a_key_is_never_placed_by_a_select_that_does_not_ask_to_place() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        let mut out = Vec::new();
        ring.select(0.0, 1.0, 0, None, &mut out);
        assert!(out.is_empty(), "an inspection query must place nothing");
        // And it must not have consumed it either.
        assert_eq!(place(&ring, 0.0, 1.0, 0, 1).len(), 1);
    }

    #[test]
    fn a_stale_key_is_never_placed() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        let mut out = Vec::new();
        ring.select(0.0, 1.0, KEY_STALE_NANOS + 1, Some((0, 1)), &mut out);
        assert!(out.is_empty(), "a note let go of seconds ago must not fire");
    }

    #[test]
    fn a_placed_key_outside_the_span_is_not_selected() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        assert_eq!(place(&ring, 0.0, 1.0, 1, 2).len(), 1);
        let mut out = Vec::new();
        ring.select(0.0, 0.25, 0, None, &mut out);
        assert!(out.is_empty(), "1/2 is not inside 0..1/4");
    }

    /// A control re-query that begins at the cursor re-claims a forgotten
    /// press AT the cursor: the takeover frame is the earliest moment the
    /// device could sound it, and the pin the outgoing generation left at or
    /// after the takeover names audio the swap retires anyway.
    #[test]
    fn a_placement_at_or_after_the_cursor_is_forgotten_and_reclaimed_there() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        // A racing horizon fill pins the press at 3/4, past the re-query's
        // cursor at 1/2. (The placement offer IS the pinned position.)
        assert_eq!(place(&ring, 0.5, 2.0, 3, 4).len(), 1);
        let mut out = Vec::new();
        ring.select(0.5, 2.0, 0, None, &mut out);
        assert_eq!((out[0].num, out[0].den), (3, 4));

        ring.forget_placements_from(0.5);
        // The re-query's own pass, beginning at the cursor, re-claims the
        // press AT the cursor, not at the stale pin.
        let reclaimed = place(&ring, 0.5, 2.0, 1, 2);
        assert_eq!(reclaimed.len(), 1, "the press survives the re-query");
        assert_eq!((reclaimed[0].num, reclaimed[0].den), (1, 2));
    }

    /// A forgotten pin is an empty slot. The re-claim stores a new lookback
    /// from THAT query's clock, and later selects freeze it - they must not
    /// keep the discarded fill's lookback, nor walk the finger further back.
    #[test]
    fn a_reclaimed_press_keeps_the_lookback_of_the_reclaim() {
        let ring = KeyRing::new();
        ring.push(1_000_000, 60, 100, 1);
        let mut filled = Vec::new();
        ring.select(0.5, 2.0, 6_000_000, Some((3, 4)), &mut filled);
        assert_eq!(filled[0].struck_nanos_ago, 5_000_000);

        ring.forget_placements_from(0.5);
        let mut reclaimed = Vec::new();
        ring.select(0.5, 2.0, 21_000_000, Some((1, 2)), &mut reclaimed);
        assert_eq!(reclaimed.len(), 1);
        assert_eq!((reclaimed[0].num, reclaimed[0].den), (1, 2));
        assert_eq!(
            reclaimed[0].struck_nanos_ago, 20_000_000,
            "the re-claim's own lookback, not the fill's"
        );

        let mut later = Vec::new();
        ring.select(0.5, 2.0, 200_000_000, Some((7, 8)), &mut later);
        assert_eq!(
            later, reclaimed,
            "a later re-query walked the finger back after the re-claim"
        );
    }

    /// Frames before the takeover are already in the device's lookahead and
    /// keep sounding across it; re-claiming one would sound the press twice.
    #[test]
    fn a_placement_before_the_cursor_survives_the_forget() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        assert_eq!(place(&ring, 0.0, 0.5, 1, 4).len(), 1);
        ring.forget_placements_from(0.5);
        let kept = place(&ring, 0.0, 1.0, 0, 1);
        assert_eq!(kept.len(), 1);
        assert_eq!((kept[0].num, kept[0].den), (1, 4), "the pin must not move");
    }

    /// The cursor lands between two pins: only the one the swap retires is
    /// forgotten, the sounding one stays exactly where it is.
    #[test]
    fn the_forget_draws_the_line_at_the_cursor_itself() {
        let ring = KeyRing::new();
        // Press 60 first and let a query place it at 1/4. (An offer reaches
        // every press in the ring, so 64 must not exist yet or it would be
        // placed at 1/4 too.)
        ring.push(0, 60, 100, 1);
        assert_eq!(place(&ring, 0.0, 1.0, 1, 4).len(), 1);
        // Press 64 lands later and a racing fill pins it at 3/4. The older
        // press keeps its own pin; the offer reaches only the new press.
        ring.push(0, 64, 100, 2);
        let placed = place(&ring, 0.0, 1.0, 3, 4);
        assert_eq!(placed.len(), 2);
        assert_eq!((placed[1].note, placed[1].num, placed[1].den), (64, 3, 4));
        ring.forget_placements_from(0.5);
        // A read-only probe (no placement offer) cannot re-claim what the
        // forget removed, so what comes back is exactly what survived.
        let mut survivors = Vec::new();
        ring.select(0.0, 1.0, 0, None, &mut survivors);
        assert_eq!(survivors.len(), 1);
        assert_eq!(
            (survivors[0].num, survivors[0].den, survivors[0].note),
            (1, 4, 60),
            "1/4 stays, 3/4 goes"
        );
    }

    /// A placement left by an older transport epoch is already invisible to
    /// selection; the forget must not resurrect it or touch the new epoch's.
    #[test]
    fn the_forget_respects_the_transport_epoch() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        assert_eq!(place(&ring, 0.0, 1.0, 3, 4).len(), 1);
        ring.clear();
        ring.push(0, 62, 100, 1);
        assert_eq!(place(&ring, 0.0, 1.0, 1, 4).len(), 1);
        ring.forget_placements_from(0.5);
        let hits = place(&ring, 0.0, 1.0, 0, 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].note, hits[0].num, hits[0].den),
            (62, 1, 4),
            "the current epoch's pin stays put"
        );
    }

    /// A live replacement retires the presses that the outgoing score's
    /// queries placed, and keeps every press that no query has placed yet.
    #[test]
    fn retiring_the_backlog_keeps_the_presses_no_query_has_placed() {
        let ring = KeyRing::new();
        ring.push(now_nanos(), 60, 100, 1);
        let mut placed = Vec::new();
        ring.select(0.0, 1.0, now_nanos(), Some((0, 1)), &mut placed);
        assert_eq!(placed.len(), 1, "the outgoing score placed A");
        ring.push(now_nanos(), 62, 100, 1); // B, after that query

        // The replacement takes over at cycle 1: A's pin at 0 sounds under
        // the outgoing score.
        ring.retire_backlog(1.0);
        let mut after = Vec::new();
        ring.select(0.0, 1.0, now_nanos(), Some((1, 4)), &mut after);
        assert_eq!(
            after.iter().map(|hit| hit.note).collect::<Vec<_>>(),
            [62],
            "only the unplaced press survives"
        );
        assert_eq!(
            (after[0].num, after[0].den),
            (1, 4),
            "placed by the new score"
        );

        // A forgotten placement does not bring a retired press back.
        ring.forget_placements_from(0.0);
        let mut again = Vec::new();
        ring.select(0.0, 1.0, now_nanos(), Some((1, 2)), &mut again);
        assert_eq!(again.iter().map(|hit| hit.note).collect::<Vec<_>>(), [62]);

        // A press after the retirement is as live as any.
        ring.push(now_nanos(), 64, 100, 1);
        let mut later = Vec::new();
        ring.select(0.0, 1.0, now_nanos(), Some((1, 2)), &mut later);
        assert_eq!(
            later.iter().map(|hit| hit.note).collect::<Vec<_>>(),
            [62, 64]
        );
        // The next replacement retires what this score has placed by then.
        ring.retire_backlog(1.0);
        let mut next = Vec::new();
        ring.select(0.0, 1.0, now_nanos(), Some((1, 2)), &mut next);
        assert!(next.is_empty(), "{next:?}");
    }

    /// The outgoing score's horizon queries pin a press at their span begin,
    /// the frontier, which is often past the replacement's takeover. The
    /// device swap drops the outgoing score's audio from the takeover on, so
    /// that pin is heard by nobody unless the incoming score may claim the
    /// press again. A pin before the takeover sounds where it is and must not
    /// be claimed a second time, not even after a later forget.
    #[test]
    fn retiring_the_backlog_forgets_the_pins_the_takeover_drops() {
        let ring = KeyRing::new();
        ring.push(now_nanos(), 60, 100, 1);
        assert_eq!(place(&ring, 0.0, 1.0, 1, 4).len(), 1, "A pinned at 1/4");
        ring.push(now_nanos(), 64, 100, 1);
        let placed = place(&ring, 0.0, 1.0, 3, 4);
        assert_eq!((placed[1].note, placed[1].num, placed[1].den), (64, 3, 4));

        // The replacement takes over at 1/2: A sounds under the outgoing
        // score, B's rendition is dropped with everything after the line.
        ring.retire_backlog(0.5);
        let mut probe = Vec::new();
        ring.select(0.0, 1.0, now_nanos(), None, &mut probe);
        assert!(probe.is_empty(), "no pin is selected as it was: {probe:?}");
        let claimed = place(&ring, 0.5, 1.0, 1, 2);
        assert_eq!(
            claimed
                .iter()
                .map(|hit| (hit.note, hit.num, hit.den))
                .collect::<Vec<_>>(),
            [(64, 1, 2)],
            "the incoming score claims B at its own span, and only B"
        );

        // A forget after the replacement cannot free the retired A.
        ring.forget_placements_from(0.0);
        let again = place(&ring, 0.0, 1.0, 0, 1);
        assert_eq!(
            again.iter().map(|hit| hit.note).collect::<Vec<_>>(),
            [64],
            "A is never claimed twice"
        );

        // A takeover that is not a finite cycle retires everything.
        ring.retire_backlog(f64::NAN);
        assert!(place(&ring, 0.0, 1.0, 0, 1).is_empty());
    }

    /// A bus with several live ports must forget on every one of them: the
    /// score queries all of its ports, and one retained pin defeats the point.
    #[test]
    fn the_bus_forgets_placements_on_every_live_port() {
        let bus = InputBus::new();
        let first = Arc::new(InputPort::new("first".into()));
        let second = Arc::new(InputPort::new("second".into()));
        bus.commit_generation(1, vec![Arc::clone(&first), Arc::clone(&second)])
            .unwrap();
        for port in [&first, &second] {
            port.observe_note_on(0, 1, 60, 100);
            let mut out = Vec::new();
            port.keys.select(0.0, 1.0, 0, Some((7, 8)), &mut out);
            assert_eq!(out.len(), 1);
        }
        bus.forget_key_placements_from(0.5);
        for port in [&first, &second] {
            let mut out = Vec::new();
            port.keys.select(0.0, 1.0, 0, Some((1, 8)), &mut out);
            assert_eq!(out.len(), 1, "the press survives the re-query");
            assert_eq!((out[0].num, out[0].den), (1, 8), "re-claimed at the cursor");
        }
    }

    /// The read of the placed presses changes nothing. A forgotten press is
    /// not among them until a query places it again, with its own number,
    /// and a retired press or one of an earlier epoch is never among them.
    #[test]
    fn the_placed_presses_are_those_a_query_can_select_again() {
        let read = |ring: &KeyRing| {
            let mut placed = Vec::new();
            ring.placed(&mut placed);
            let key = |key: &PlacedKey| (key.press, key.note, key.num, key.den);
            placed.iter().map(key).collect::<Vec<_>>()
        };
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        assert_eq!(read(&ring), [], "no query has placed the press");
        assert_eq!(place(&ring, 0.0, 1.0, 1, 4).len(), 1);
        ring.push(0, 64, 90, 2);
        assert_eq!(place(&ring, 0.0, 1.0, 3, 4).len(), 2);
        assert_eq!(read(&ring), [(1, 60, 1, 4), (2, 64, 3, 4)]);
        assert_eq!(read(&ring), [(1, 60, 1, 4), (2, 64, 3, 4)], "a read only");

        ring.forget_placements_from(0.5);
        assert_eq!(read(&ring), [(1, 60, 1, 4)]);
        assert_eq!(place(&ring, 0.5, 1.0, 1, 2).len(), 1);
        assert_eq!(read(&ring), [(1, 60, 1, 4), (2, 64, 1, 2)]);

        ring.retire_backlog(0.5);
        assert_eq!(read(&ring), [], "one is retired, the other forgotten");
        assert_eq!(place(&ring, 0.5, 1.0, 5, 8).len(), 1);
        assert_eq!(read(&ring), [(2, 64, 5, 8)]);
        ring.clear();
        assert_eq!(read(&ring), [], "the epoch of the press is over");
    }

    /// The bus reads each port once, also when two generations name it, and
    /// leaves out a port with no placed press.
    #[test]
    fn the_bus_reads_the_placed_presses_of_each_port_once() {
        let bus = InputBus::new();
        let first = Arc::new(InputPort::new("first".into()));
        let second = Arc::new(InputPort::new("second".into()));
        bus.commit_generation(1, vec![Arc::clone(&first), Arc::clone(&second)])
            .unwrap();
        bus.snapshot_for(1, 1);
        bus.republish_current_generation(2);
        assert!(bus.placed_keys().is_empty());
        first.observe_note_on(0, 1, 60, 100);
        let mut out = Vec::new();
        first.keys.select(0.0, 1.0, 0, Some((7, 8)), &mut out);
        let placed = bus.placed_keys();
        assert_eq!(placed.len(), 1, "one port has a placed press");
        assert!(Arc::ptr_eq(&placed[0].0, &first));
        let key = PlacedKey {
            press: 1,
            note: 60,
            num: 7,
            den: 8,
        };
        assert_eq!(placed[0].1, [key]);
    }

    /// A launch pad is a transport button: its press never enters the
    /// musical ring and never counts as a musical press, while the same
    /// port's other notes are untouched.
    #[test]
    fn a_launch_pad_never_enters_the_musical_ring() {
        let port = InputPort::new("x".into());
        port.set_transport_pads(&[(60, 1)]);
        port.observe_note_on(0, 1, 60, 100); // bound: transport
        port.observe_note_on(0, 1, 61, 100); // not bound: musical
        let mut out = Vec::new();
        port.keys.select(0.0, 1.0, 0, Some((1, 8)), &mut out);
        assert_eq!(out.len(), 1, "only the unbound note is musical");
        assert_eq!(out[0].note, 61);
        assert_eq!(port.keys.presses(), 1, "only the musical press counts");
    }

    /// A wildcard channel binding covers every channel of the note: a
    /// controller that mirrors a pad across channels must not leak a
    /// musical press through the unbound one.
    #[test]
    fn a_wildcard_launch_pad_binds_every_channel() {
        let port = InputPort::new("x".into());
        port.set_transport_pads(&[(60, 0xFF)]);
        port.observe_note_on(0, 5, 60, 100);
        port.observe_note_on(0, 9, 60, 100);
        let mut out = Vec::new();
        port.keys.select(0.0, 1.0, 0, Some((1, 8)), &mut out);
        assert!(out.is_empty(), "no channel of a bound note is musical");
        assert_eq!(port.keys.presses(), 0);
    }

    /// The app sends the launch-pad set when the set opens, before any score
    /// has named a keyboard. A port created (by `intern`) or published (by
    /// `commit_generation`, the path a score's staged inputs take) after that
    /// must still know its transport buttons, with nothing re-stating the set
    /// on every engine turn.
    #[test]
    fn a_port_created_after_the_launch_pads_were_sent_knows_them() {
        let bus = InputBus::new();
        bus.set_launch_pads(&[(60, 1)]);

        let (_, interned) = bus.intern("pads").expect("an interned port");
        interned.observe_note_on(0, 1, 60, 100);
        assert_eq!(interned.keys.presses(), 0, "an interned port knows the pad");

        let staged = Arc::new(InputPort::new("staged".into()));
        bus.commit_generation(1, vec![Arc::clone(&staged)])
            .expect("a committed generation");
        staged.observe_note_on(0, 1, 60, 100);
        staged.observe_note_on(0, 1, 62, 100);
        assert_eq!(
            staged.keys.presses(),
            1,
            "a committed port knows the pad and still hears its other notes"
        );

        // A later change reaches the ports already out there.
        bus.set_launch_pads(&[]);
        staged.observe_note_on(0, 1, 60, 100);
        assert_eq!(staged.keys.presses(), 2, "an unbound note is musical again");
    }

    /// Exact channels bind only themselves, the wildcard binds all, and a
    /// value that is not a MIDI channel binds nothing.
    #[test]
    fn transport_pad_words_bind_exact_channels_and_the_wildcard() {
        let words = transport_pad_words(&[(60, 1), (61, 0xFF), (62, 200), (190, 3)]);
        let port = InputPort::new("x".into());
        port.store_transport_pad_words(&words);
        assert!(port.is_transport_pad(60, 1));
        assert!(
            !port.is_transport_pad(60, 2),
            "another channel of an exact pad"
        );
        assert!(port.is_transport_pad(61, 16) && port.is_transport_pad(61, 0));
        assert!(!port.is_transport_pad(62, 200), "not a MIDI channel");
        // Notes fold into 0..=127 the way the driver's data bytes do.
        assert!(port.is_transport_pad(190 & 0x7f, 3));
        assert!(!port.is_transport_pad(59, 1));
    }

    #[test]
    fn a_key_carries_its_note_velocity_and_channel_intact() {
        let ring = KeyRing::new();
        ring.push(0, 64, 100, 3);
        let hits = place(&ring, 0.0, 1.0, 0, 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].note, hits[0].velocity, hits[0].channel),
            (64, 100, 3)
        );
    }

    #[test]
    fn clear_starts_a_new_epoch_and_forgets_already_placed_hits() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        assert_eq!(place(&ring, 0.0, 1.0, 0, 1).len(), 1);
        let previous_epoch = ring.epoch();
        ring.clear();
        assert_eq!(ring.epoch(), previous_epoch + 1);
        let mut out = Vec::new();
        ring.select(0.0, 1.0, 0, None, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn clear_crossing_a_selection_drops_the_whole_old_epoch() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        ring.push(0, 64, 100, 1);
        let mut cleared = false;
        let mut out = Vec::new();
        ring.select_with_hook(0.0, 1.0, 0, Some((0, 1)), &mut out, || {
            if !cleared {
                cleared = true;
                ring.clear();
            }
        });
        assert!(out.is_empty(), "a prefix of the old epoch escaped clear");
    }

    #[test]
    fn recycling_after_a_payload_read_cannot_move_placement_to_the_new_entry() {
        let ring = KeyRing::new();
        ring.push(0, 60, 100, 1);
        let mut recycled = false;
        let mut out = Vec::new();
        ring.select_with_hook(0.0, 1.0, 0, Some((1, 4)), &mut out, || {
            if !recycled {
                recycled = true;
                for note in 0..KEY_RING {
                    ring.push(0, (note % 128) as u8, 90, 2);
                }
            }
        });
        assert!(
            out.is_empty(),
            "a recycled payload was published as the old note"
        );
        let fresh = place(&ring, 0.0, 1.0, 3, 4);
        assert_eq!(fresh.len(), KEY_RING);
        assert!(fresh.iter().all(|hit| (hit.num, hit.den) == (3, 4)));
    }

    #[test]
    fn concurrent_queries_assign_exactly_one_placement() {
        let ring = Arc::new(KeyRing::new());
        ring.push(0, 60, 100, 1);
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for placement in [(1, 4), (3, 4)] {
            let ring = Arc::clone(&ring);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                let mut out = Vec::new();
                ring.select(0.0, 1.0, 0, Some(placement), &mut out);
                out
            }));
        }
        barrier.wait();
        let first = workers.remove(0).join().unwrap();
        let second = workers.remove(0).join().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        assert!(matches!((first[0].num, first[0].den), (1, 4) | (3, 4)));
    }
}
