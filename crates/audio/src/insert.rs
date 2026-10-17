//! Insert chains process their voices before joining an output orbit.
//!
//! The host builds an insert outside the audio callback, for example from a
//! plugin. The callback owns the insert while it serves a chain. Ownership
//! moves the same way as an orbit reverb: one box through an install ring,
//! and the displaced box back through a return ring.
//!
//! Each logical chain has effect slots and one instrument slot. The chain
//! takes the notes that ask for an effect: such a note sends its dry signal
//! through the effects in call order, and a note on the same orbit with no
//! such request stays dry. The instrument slot holds an insert that makes
//! sound from notes. The instrument goes through the chain when its note
//! asks for an effect too.
//!
//! ```text
//!   voice/instrument -> gain --+-> dry -> [VST effects] -> postgain --+
//!                             +-> postgain -> delay/reverb ---------+
//!                                                                   |
//!                      mix <- fader <- duck <- DJ filter <- orbit <-+
//! ```

/// The parameter values one note carries for its insert.
pub const MAX_INSERT_PARAMS: usize = 16;
/// The effects one chain holds.
pub const EFFECT_CHAIN: usize = 4;
/// Concurrent chains, independent of their output orbits.
pub const MAX_INSERT_BUSES: usize = 64;
/// Physical slots reserved before rendering.
pub const INSERT_SLOTS: usize = (EFFECT_CHAIN + 1) * MAX_INSERT_BUSES;

/// A logical chain and its effect or instrument position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InsertSlot {
    pub chain: u64,
    pub stage: u8,
}

impl InsertSlot {
    pub const fn effect(chain: u64, stage: usize) -> Self {
        assert!(stage < EFFECT_CHAIN);
        Self {
            chain,
            stage: stage as u8,
        }
    }

    pub const fn instrument(chain: u64) -> Self {
        Self {
            chain,
            stage: EFFECT_CHAIN as u8,
        }
    }

    pub const fn is_valid(self) -> bool {
        self.stage as usize <= EFFECT_CHAIN
    }

    pub(crate) const fn physical(self, bus: usize) -> usize {
        self.stage as usize * MAX_INSERT_BUSES + bus
    }
}

pub const fn effect_slot(chain: u64, stage: usize) -> InsertSlot {
    InsertSlot::effect(chain, stage)
}

pub const fn instrument_slot(chain: u64) -> InsertSlot {
    InsertSlot::instrument(chain)
}

/// Which prepared effect a note asks for: the plugin and its preset.
///
/// The host assigns both numbers. Preset 0 is the state the plugin starts in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InsertKey {
    pub plugin: u32,
    pub preset: u32,
}

/// One parameter value. `id` is the number the plugin gives the parameter,
/// and `value` is normalized to `0..=1`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InsertParam {
    pub id: u32,
    pub value: f32,
}

/// A note for an insert that makes sound from notes, such as a synth plugin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InsertNote {
    /// The pitch as a MIDI note number. 69 is 440 Hz. A fraction is a pitch
    /// between two keys.
    pub pitch: f32,
    /// From 0 to 1.
    pub velocity: f32,
    /// The length of the note in frames.
    pub frames: u32,
}

/// What a note asks of the insert in its chain. Plain data, so the value
/// crosses the event ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InsertControls {
    pub key: InsertKey,
    params: [InsertParam; MAX_INSERT_PARAMS],
    param_count: u8,
    /// The position of the note in quarter notes, and the quarter notes in
    /// one minute. A tempo of 0 means the note has no clock.
    beats: f64,
    tempo: f32,
    /// The note for an insert that makes the sound. 0 frames means the
    /// insert gets no note.
    note: InsertNote,
    score: Option<u64>,
}

impl InsertControls {
    pub const fn new(key: InsertKey) -> Self {
        Self {
            key,
            params: [InsertParam { id: 0, value: 0.0 }; MAX_INSERT_PARAMS],
            param_count: 0,
            beats: 0.0,
            tempo: 0.0,
            note: InsertNote {
                pitch: 0.0,
                velocity: 0.0,
                frames: 0,
            },
            score: None,
        }
    }

    /// Gives the insert the note to play. The engine voice of the note is
    /// then silent, and the insert makes the sound.
    pub fn with_note(mut self, note: InsertNote) -> Self {
        self.note = note;
        self
    }

    /// The note for the insert, if the insert makes the sound.
    pub fn note(&self) -> Option<InsertNote> {
        (self.note.frames > 0).then_some(self.note)
    }

    /// Selects a score event prepared by the host outside the callback.
    pub fn with_score(mut self, score: u64) -> Self {
        self.score = Some(score);
        self
    }

    pub fn score(&self) -> Option<u64> {
        self.score
    }

    /// Gives the note its place in the music, so an effect with a tempo
    /// follows the score: the position of the note in quarter notes, and the
    /// quarter notes in one minute.
    pub fn with_clock(mut self, beats: f64, tempo: f32) -> Self {
        self.beats = beats;
        self.tempo = tempo;
        self
    }

    /// The position and the tempo of the note, if the note has a clock.
    pub fn clock(&self) -> Option<(f64, f32)> {
        (self.tempo > 0.0 && self.beats.is_finite()).then_some((self.beats, self.tempo))
    }

    /// Adds one parameter value. Returns false when the note already carries
    /// [`MAX_INSERT_PARAMS`] values.
    pub fn push(&mut self, param: InsertParam) -> bool {
        let count = usize::from(self.param_count);
        if count == MAX_INSERT_PARAMS {
            return false;
        }
        self.params[count] = param;
        self.param_count += 1;
        true
    }

    pub fn params(&self) -> &[InsertParam] {
        &self.params[..usize::from(self.param_count)]
    }
}

/// A stereo effect in one chain.
///
/// Callback methods do not allocate, free or take a lock.
pub trait OrbitInsert: Send {
    /// The effect this box holds. A note with a different key gets no effect
    /// until the host installs a new box.
    fn key(&self) -> InsertKey;

    /// Sets a parameter value `frames` from now.
    fn set_param(&mut self, param: InsertParam, frames: u32);

    /// Puts each parameter a note before set back to its value at the start
    /// of the insert, `frames` from now. The engine calls this before the
    /// values of each note, so a note sounds as its own values say. A
    /// parameter a note set at the same frame keeps its value.
    fn restore_params(&mut self, _frames: u32) {}

    /// Replaces the signal in place. Both slices have the same length, from
    /// 1 to 128 frames.
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);

    /// Starts one block. The default completes it synchronously. An insert
    /// that runs on a different thread returns before its output is ready,
    /// and the other chains run in that time.
    /// Call `collect` with these same slices before another operation on
    /// this insert. An insert has one pending block at most.
    fn submit(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.process(left, right);
    }

    /// Completes the submitted block. Synchronous inserts need no work.
    fn collect(&mut self, _left: &mut [f32], _right: &mut [f32]) {}

    /// Sets the musical clock: `frames` from now, the position is `beats`
    /// quarter notes and the tempo is `tempo` quarter notes in one minute.
    fn sync(&mut self, _beats: f64, _tempo: f32, _frames: u32) {}

    /// Starts a note `frames` from now. The insert ends the note after the
    /// length of the note.
    fn note(&mut self, _note: InsertNote, _frames: u32) {}

    /// Starts a prepared score event `frames` from now.
    fn score(&mut self, _score: u64, _frames: u32) {}

    /// Ends each note `frames` from now: a note that sounds then, and a
    /// note that starts before then. A rewind cuts the engine voices at the
    /// same frame.
    fn cut_notes(&mut self, _frames: u32) {}

    /// True while the insert holds a note, or a value for a later frame.
    /// The engine keeps such an insert awake, also when its output is
    /// silent.
    fn busy(&self) -> bool {
        false
    }

    /// Ends each note and drops the tail after a clock reset.
    fn reset(&mut self) {}
}

/// Builds the insert for a key, a sample rate and a slot, outside the
/// callback. `None` means the host has no such insert now, and the notes
/// play dry.
pub type InsertProvider =
    dyn Fn(InsertKey, u32, InsertSlot) -> Option<Box<dyn OrbitInsert>> + Send + Sync;

/// Gives the chains the playing score has now: see
/// `LiveScalarDevice::set_named_insert_chains`. The output asks before it
/// takes the inserts of a turn.
pub type InsertNames = dyn Fn() -> Vec<u64> + Send + Sync;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_carries_sixteen_parameter_values_and_refuses_one_more() {
        let mut controls = InsertControls::new(InsertKey {
            plugin: 3,
            preset: 0,
        });
        for id in 0..MAX_INSERT_PARAMS as u32 {
            assert!(controls.push(InsertParam { id, value: 0.5 }));
        }
        assert!(!controls.push(InsertParam { id: 99, value: 1.0 }));
        assert_eq!(controls.params().len(), MAX_INSERT_PARAMS);
        assert_eq!(controls.params()[15].id, 15);
    }
}
