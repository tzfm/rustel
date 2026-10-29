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
//! such request stays dry. The stage of an effect is the effect slot that
//! serves it, by default its place in the note. The instrument slot holds
//! an insert that makes sound from notes. The instrument goes through the
//! chain when its note asks for an effect too.
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
/// No stage. An effect with no stage takes the stage of its place in the
/// note, and a list of stages ends here.
pub const NO_STAGE: u8 = u8::MAX;
/// The shortest quiet time of an insert, in seconds. A stopped output
/// drains an insert until it is quiet for its tail, and for this long at
/// least: see [`OrbitInsert::drain`]. The plugin host takes the same time
/// for its flush, so a copy the drain made quiet skips its flush.
pub const INSERT_DRAIN_QUIET_SECONDS: f32 = 0.25;

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

/// A value a slider moved: a parameter value of the copy of `key` in
/// `slot`. The output gives the copy the value at once, and each note in
/// its queue of `generation` or older takes the value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InsertValue {
    pub slot: InsertSlot,
    pub key: InsertKey,
    pub param: InsertParam,
    /// The generation of the score at the move. A note of a newer
    /// generation keeps its own value.
    pub generation: u64,
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
    /// The stage of the chain that serves this effect: see [`Self::stage`].
    stage: u8,
    /// The latency of the copy of the stage in frames: see
    /// [`Self::latency`].
    latency: u32,
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
            stage: NO_STAGE,
            latency: 0,
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

    /// Sets the stage of the chain that serves this effect: see
    /// [`Self::stage`].
    pub fn set_stage(&mut self, stage: u8) {
        self.stage = stage;
    }

    /// The stage of the chain that serves this effect, the effect at
    /// `place` in the note: the stage the effect got, else the place. A
    /// stage not below [`EFFECT_CHAIN`], such as [`NO_STAGE`], gives the
    /// place.
    pub fn stage(&self, place: usize) -> usize {
        match usize::from(self.stage) {
            stage if stage < EFFECT_CHAIN => stage,
            _ => place,
        }
    }

    /// Gives the stage the latency of its copy: see [`Self::latency`].
    pub fn with_latency(mut self, frames: u32) -> Self {
        self.latency = frames;
        self
    }

    /// The latency of the copy that serves the stage, in frames. The output
    /// of the stage comes this many frames after its input.
    pub fn latency(&self) -> u32 {
        self.latency
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

    /// Sets a parameter value now. Each value of the parameter that waits
    /// for a later frame takes the value too. A slider moves a value this
    /// way, and the insert does its own smoothing. The default sets the
    /// value now only.
    fn set_param_now(&mut self, param: InsertParam) {
        self.set_param(param, 0);
    }

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
    /// and the other chains run in that time. An insert that answers true to
    /// [`Self::any_thread`] gets this call and `collect` on a plugin worker
    /// of the output or on the callback, and the callback joins the block in
    /// the same sub-block.
    /// Call `collect` with these same slices before another operation on
    /// this insert. An insert has one pending block at most.
    fn submit(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.process(left, right);
    }

    /// Completes the submitted block. Synchronous inserts need no work.
    fn collect(&mut self, _left: &mut [f32], _right: &mut [f32]) {}

    /// Waits until the next `frames` frames are ready to collect. An
    /// offline render calls this before `collect`: it reads as fast as
    /// the host can, and an insert that renders ahead on its own thread
    /// would starve. The audio callback never calls it.
    fn flush(&mut self, _frames: usize) {}

    /// Prepares the first `frames` frames before device playback starts.
    /// Initial notes arrive first. This may wait on the caller, but keeps
    /// the playback position at zero and returns to live rendering.
    fn prime(&mut self, _frames: usize) {}

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

    /// True when the insert renders on its own thread ahead of the
    /// callback. The engine then hands such an insert each note when the
    /// event is admitted, a schedule lead before the frame it sounds, so
    /// the insert has the note ready at its own frame. The activation of
    /// the onset delivers the note again, and a prepared score plays once,
    /// so nothing sounds twice.
    fn renders_ahead(&self) -> bool {
        false
    }

    /// True while the insert holds a note, or a value for a later frame.
    /// The engine keeps such an insert awake, also when its output is
    /// silent.
    fn busy(&self) -> bool {
        false
    }

    /// True when the blocks of this insert run on any thread. The engine
    /// then makes the block calls (`submit`, `collect`, `drain` and `busy`)
    /// on the audio callback or on a plugin worker of the output, one thread
    /// at a time. Between the start of a block and its join, the engine makes
    /// no other call on the insert. Each other call stays on the callback.
    fn any_thread(&self) -> bool {
        false
    }

    /// Ends each note and drops the tail after a clock reset.
    fn reset(&mut self) {}

    /// The frames the insert sounds after its input goes silent, as the
    /// tail of a reverb or a delay: 3 s at most. The default has no tail.
    fn tail_frames(&self) -> u32 {
        0
    }

    /// Plays one block of silence while the output is stopped, after
    /// `reset`. The output of the block goes nowhere. Both slices have the
    /// same length, from 1 to 128 frames. A stopped output drains an insert
    /// until it is quiet for its tail, or for 0.25 s when that is longer,
    /// and for 3 s at most: see [`Self::tail_frames`]. So an insert that
    /// keeps its tail through a reset has no tail left at its next note. The
    /// default processes the block.
    fn drain(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.process(left, right);
    }
}

/// Builds the insert for a key, a sample rate and a slot, outside the
/// callback. `None` means the host has no such insert now, and the notes
/// play dry.
pub type InsertProvider =
    dyn Fn(InsertKey, u32, InsertSlot) -> Option<Box<dyn OrbitInsert>> + Send + Sync;

/// The counts of a live output at a turn of its producer: see
/// [`InsertNames`]. Each count is a total since the output started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InsertTurn {
    /// The copies and the notes of a chain the output had no bus for.
    pub refusals: u64,
    /// The plugin jobs that ran late while the callback waited for them.
    pub late_jobs: u64,
    /// The plugin jobs the callback ran itself.
    pub inline_jobs: u64,
}

/// What the names source gives the output at a turn: see [`InsertNames`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InsertChains {
    /// The chains the playing score has: see
    /// `LiveScalarDevice::set_named_insert_chains`.
    pub named: Vec<u64>,
    /// The copies of these chains that the playing score no longer has, by
    /// slot and key. The callback gives such a copy back when it is not busy
    /// and its stage is not heard.
    pub gone: Vec<(InsertSlot, InsertKey)>,
}

/// Gives the chains the playing score has now, and the copies it no longer
/// has. The output asks before it takes the inserts of a turn, and gives the
/// counts of the turn.
pub type InsertNames = dyn Fn(InsertTurn) -> InsertChains + Send + Sync;

/// Gives the values sliders moved since the output asked last, in the order
/// of the moves, `room` values at most. The source keeps the rest for the
/// next turn. The output asks at each turn, after the names.
pub type InsertValues = dyn Fn(usize) -> Vec<InsertValue> + Send + Sync;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_carries_sixteen_parameter_values_and_refuses_one_more() {
        // The stage and the latency fit in the room the other fields leave,
        // so the controls keep their size.
        assert_eq!(std::mem::size_of::<InsertControls>(), 184);
        let mut controls = InsertControls::new(InsertKey {
            plugin: 3,
            preset: 0,
        });
        assert_eq!(controls.latency(), 0);
        // An effect with no stage takes the stage of its place.
        assert_eq!(controls.stage(2), 2);
        controls.set_stage(1);
        assert_eq!(controls.stage(2), 1);
        controls.set_stage(EFFECT_CHAIN as u8);
        assert_eq!(controls.stage(2), 2);
        for id in 0..MAX_INSERT_PARAMS as u32 {
            assert!(controls.push(InsertParam { id, value: 0.5 }));
        }
        assert!(!controls.push(InsertParam { id: 99, value: 1.0 }));
        assert_eq!(controls.params().len(), MAX_INSERT_PARAMS);
        assert_eq!(controls.params()[15].id, 15);

        // The other builders keep the latency of the stage.
        let controls = controls.with_latency(480);
        assert_eq!(controls.latency(), 480);
        let note = InsertNote {
            pitch: 60.0,
            velocity: 1.0,
            frames: 64,
        };
        assert_eq!(controls.with_note(note).latency(), 480);
        assert_eq!(controls.with_score(7).latency(), 480);
        assert_eq!(controls.with_clock(2.0, 120.0).latency(), 480);

        // A chain sums the latencies of its stages, whatever the lead of the
        // note.
        let stage = |frames| InsertControls::new(InsertKey::default()).with_latency(frames);
        let mut note_controls: crate::OscillatorControls = Default::default();
        assert_eq!(note_controls.lead, 0);
        assert_eq!(note_controls.chain_latency(), 0);
        note_controls.effects[0] = Some(stage(100));
        note_controls.effects[2] = Some(stage(200));
        note_controls.instrument = Some(stage(64));
        for lead in [0, 100, 364] {
            note_controls.lead = lead;
            assert_eq!(note_controls.effect_latency(), 300);
            assert_eq!(note_controls.chain_latency(), 364);
        }
        // The musical onset is the frame the note starts on plus its lead.
        note_controls.lead = 0;
        assert_eq!(note_controls.musical(1000), 1000);
        note_controls.lead = 480;
        assert_eq!(note_controls.musical(1000), 1480);
    }
}
