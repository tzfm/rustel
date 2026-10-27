use super::*;

pub(super) fn claim_insert_bus(chains: &mut [Option<u64>], chain: u64) -> Option<usize> {
    if let Some(bus) = chains.iter().position(|held| *held == Some(chain)) {
        return Some(bus);
    }
    let preferred = usize::try_from(chain)
        .ok()
        .filter(|bus| chains.get(*bus) == Some(&None));
    let bus = preferred.or_else(|| chains.iter().position(Option::is_none))?;
    chains[bus] = Some(chain);
    Some(bus)
}

/// A sample of an insert that is not finite becomes silence, so one bad
/// insert cannot poison what comes after. True means the output was silent.
pub(crate) fn settle_insert_block(left: &mut [f32], right: &mut [f32]) -> bool {
    let mut silent = true;
    for sample in left.iter_mut().chain(right.iter_mut()) {
        if !sample.is_finite() {
            *sample = 0.0;
        }
        silent &= sample.abs() < INSERT_SILENCE;
    }
    silent
}

/// The wake test of an effect stage. The stage runs when a sample of its
/// input is not zero, while its idle frames are under `limit`, while it is
/// busy, and while a level of its slot moves. A stage at the level 0 does
/// not run, and its input passes. Gives whether the input is fed, or `None`
/// when the stage sleeps.
pub(crate) fn effect_stage_wakes(
    effect: &dyn crate::insert::OrbitInsert,
    left: &[f32],
    right: &[f32],
    idle: u32,
    limit: u32,
    mix: &InsertMix,
) -> Option<bool> {
    if mix.out() {
        return None;
    }
    let fed = left.iter().chain(right).any(|sample| *sample != 0.0);
    (fed || mix.moving() || idle < limit || effect.busy()).then_some(fed)
}

/// Renders one block of the copy in the leaving place of a slot into
/// `block`: on `input`, the input of its effect stage, or on silence for an
/// instrument. An `offline` render waits for the block first, as it waits
/// for the active instrument.
pub(crate) fn render_leaving_insert(
    leaving: &mut dyn crate::insert::OrbitInsert,
    input: Option<&[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    block: &mut [[f32; crate::reverb::REVERB_BLOCK]; 2],
    frames: usize,
    offline: bool,
) {
    for (channel, samples) in block.iter_mut().enumerate() {
        match input {
            Some(input) => samples[..frames].copy_from_slice(&input[channel][..frames]),
            None => samples[..frames].fill(0.0),
        }
    }
    let [left, right] = block;
    let (left, right) = (&mut left[..frames], &mut right[..frames]);
    leaving.submit(left, right);
    if offline {
        leaving.flush(frames);
    }
    leaving.collect(left, right);
    settle_insert_block(left, right);
}

/// Ends the block of an effect stage while a level of its slot moves: see
/// [`InsertMix`]. The copy in the leaving place renders the input of the
/// stage, `dry`, while its own level moves. An effect renders no block
/// ahead, so an offline render waits for neither copy. Each frame then takes
/// the output of the active copy at its level, the output of the leaving
/// copy at its level, and the input of the stage for the rest.
pub(crate) fn fade_effect_stage(
    left: &mut [f32],
    right: &mut [f32],
    dry: &[[f32; crate::reverb::REVERB_BLOCK]; 2],
    leaving: Option<&mut (dyn crate::insert::OrbitInsert + '_)>,
    block: &mut [[f32; crate::reverb::REVERB_BLOCK]; 2],
    mix: &mut InsertMix,
) {
    let frames = left.len();
    let leaving = leaving.filter(|_| mix.leaving.is_moving()).map(|leaving| {
        render_leaving_insert(leaving, Some(dry), block, frames, false);
        &*block
    });
    for (i, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let (active, level) = (mix.active.next(), mix.leaving.next());
        let rest = 1.0 - active - level;
        let (old_left, old_right) = leaving.map_or((0.0, 0.0), |old| (old[0][i], old[1][i]));
        *left = *left * active + old_left * level + dry[0][i] * rest;
        *right = *right * active + old_right * level + dry[1][i] * rest;
    }
}

/// The stages of an effect order from the place `first` on, each with its
/// place, up to [`crate::insert::NO_STAGE`].
pub(crate) fn ordered_stages(
    order: &[u8; crate::insert::EFFECT_CHAIN],
    first: usize,
) -> impl Iterator<Item = (usize, usize)> {
    let places = order.iter().enumerate().skip(first);
    places.map_while(|(place, stage)| {
        (*stage != crate::insert::NO_STAGE).then_some((place, usize::from(*stage)))
    })
}

/// The value of `param` that a note of `generation` gives the copy of `key`
/// in `slot`: the value a slider moved, when the move has that generation
/// or a newer one, else the value of the note.
fn moved(
    values: &[crate::insert::InsertValue],
    slot: crate::insert::InsertSlot,
    key: crate::insert::InsertKey,
    param: crate::insert::InsertParam,
    generation: u64,
) -> crate::insert::InsertParam {
    let moved = values.iter().find(|value| {
        (value.slot, value.key, value.param.id) == (slot, key, param.id)
            && value.generation >= generation
    });
    moved.map_or(param, |value| value.param)
}

/// Gives the insert in `slot` what a note of `generation` asks of it,
/// `frames` from now. Each value an earlier note set goes back first, so the
/// note sounds as its own values say. A value a slider moved, in `values`,
/// takes the place of the value of the note: see [`moved`].
pub(super) fn deliver(
    insert: &mut dyn crate::insert::OrbitInsert,
    slot: crate::insert::InsertSlot,
    wanted: &crate::insert::InsertControls,
    generation: u64,
    values: &[crate::insert::InsertValue],
    frames: u32,
) {
    insert.restore_params(frames);
    for param in wanted.params() {
        let param = moved(values, slot, wanted.key, *param, generation);
        insert.set_param(param, frames);
    }
    if let Some((beats, tempo)) = wanted.clock() {
        insert.sync(beats, tempo, frames);
    }
    if let Some(score) = wanted.score() {
        insert.score(score, frames);
    }
    if let Some(note) = wanted.note() {
        insert.note(note, frames);
    }
}

/// Takes the copy out of a slot for a copy of a new key. A heard copy fades
/// out in the leaving place from the level it has, and the copy there before
/// goes back. Any other copy goes back at once. False, with nothing taken,
/// when a copy that goes back finds no place in `retired`.
pub(super) fn displace_insert(
    held: &mut Option<Box<dyn crate::insert::OrbitInsert>>,
    leaving: &mut Option<Box<dyn crate::insert::OrbitInsert>>,
    mix: &mut InsertMix,
    retired: &mut Vec<Box<dyn crate::insert::OrbitInsert>>,
    heard: bool,
    sample_rate: u32,
) -> bool {
    let goes_back = if heard {
        leaving.is_some()
    } else {
        held.is_some()
    };
    if goes_back && retired.len() == retired.capacity() {
        return false;
    }
    let Some(old) = held.take() else {
        return true;
    };
    if !heard {
        retired.push(old);
        return true;
    }
    if let Some(before) = leaving.replace(old) {
        retired.push(before);
    }
    mix.leaving = crate::live_control::Ramp::new(0, 0.0);
    mix.leaving.declick_from(mix.active, sample_rate);
    true
}

/// The order of a chain after a note that goes through the stages of
/// `wanted`, in its order. While the chain is heard, each stage of `order`
/// that leaves stays for its fade, right after the stage before it in
/// `order`, or first. So it keeps its place among the stages that stay. A
/// chain that is not heard takes `wanted`.
fn merged_order(
    order: &[u8; crate::insert::EFFECT_CHAIN],
    wanted: &[u8; crate::insert::EFFECT_CHAIN],
    heard: bool,
) -> [u8; crate::insert::EFFECT_CHAIN] {
    let mut merged = *wanted;
    if !heard {
        return merged;
    }
    // The place after the last stage of `order` the walk met.
    let mut next = 0;
    for (_, stage) in ordered_stages(order, 0) {
        let stage = stage as u8;
        match merged.iter().position(|at| *at == stage) {
            Some(place) => next = place + 1,
            None => {
                // A stage that leaves is not in `wanted`, so the last place
                // is free.
                merged[next..].rotate_right(1);
                merged[next] = stage;
                next += 1;
            }
        }
    }
    merged
}

/// Sets the order of the chain on `bus` after a newer note that goes
/// through the stages of `wanted`: see [`merged_order`]. While the chain is
/// heard, a new stage fades in, a stage that stays moves back to its level,
/// and a stage that leaves fades out. Else each stage of `wanted` has its
/// level at once, and a stage that leaves and each copy in a leaving place
/// go now. [`ScalarBackend::finish_insert_fades`] ends each stage at the
/// level 0.
pub(super) fn order_insert_stages(
    order: &mut [u8; crate::insert::EFFECT_CHAIN],
    mixes: &mut [InsertMix],
    fading: &mut u64,
    bus: usize,
    wanted: &[u8; crate::insert::EFFECT_CHAIN],
    heard: bool,
    sample_rate: u32,
) {
    let merged = merged_order(order, wanted, heard);
    for stage in 0..crate::insert::EFFECT_CHAIN as u8 {
        let mix = &mut mixes[usize::from(stage) * crate::insert::MAX_INSERT_BUSES + bus];
        let (was, stays) = (order.contains(&stage), wanted.contains(&stage));
        let target = match (was, stays) {
            (_, true) => 1.0,
            (true, false) => 0.0,
            (false, false) => continue,
        };
        let mut active = crate::live_control::Ramp::new(0, target);
        if heard {
            // A new stage fades in from 0.
            let from = match was {
                true => mix.active,
                false => crate::live_control::Ramp::new(0, 0.0),
            };
            active.declick_from(from, sample_rate);
        } else {
            // The copy in the leaving place goes now. Its fade beside a
            // stage at its level would take the input away.
            mix.leaving = crate::live_control::Ramp::new(0, 0.0);
        }
        mix.active = active;
        if mix.moving() || target == 0.0 {
            *fading |= 1u64 << bus;
        }
    }
    *order = merged;
}

/// Takes `stage` out of an effect order. Each stage after it moves up one
/// place.
fn leave_order(order: &mut [u8; crate::insert::EFFECT_CHAIN], stage: u8) {
    if let Some(place) = order.iter().position(|at| *at == stage) {
        order.copy_within(place + 1.., place);
        order[crate::insert::EFFECT_CHAIN - 1] = crate::insert::NO_STAGE;
    }
}

impl Default for InsertPostgain {
    fn default() -> Self {
        Self {
            owner: (0, 0),
            value: 1.0,
            offset: crate::live_control::Ramp::default(),
            applied: 1.0,
            owned: false,
            // A new chain is silent.
            quiet: u16::MAX,
            unfed: u16::MAX,
        }
    }
}

impl InsertPostgain {
    /// The level after a reset: 0 until a note owns the chain. The chain
    /// keeps its count of silent frames. No insert runs while the output is
    /// stopped, so a tail at the end of the stop ramp is still there after
    /// the resume. The next note then moves the level up from 0, also in the
    /// first block after the resume.
    pub(super) fn silent(quiet: u16) -> Self {
        Self {
            value: 0.0,
            applied: 0.0,
            owned: true,
            quiet,
            ..Self::default()
        }
    }
}

impl Default for InsertMix {
    fn default() -> Self {
        Self {
            active: crate::live_control::Ramp::new(0, 1.0),
            leaving: crate::live_control::Ramp::new(0, 0.0),
        }
    }
}

impl InsertMix {
    /// True while a level of the slot moves.
    pub(crate) fn moving(&self) -> bool {
        self.active.is_moving() || self.leaving.is_moving()
    }

    /// True when the active copy rests at the level 0, until its stage
    /// leaves the order.
    fn out(&self) -> bool {
        !self.active.is_moving() && self.active.target() == 0.0
    }
}

impl ScalarBackend {
    pub(super) fn init_insert_pool(&mut self) {
        if self.orbit_inserts.is_empty() {
            self.orbit_inserts = (0..crate::insert::INSERT_SLOTS).map(|_| None).collect();
            self.prepared_inserts = (0..crate::insert::INSERT_SLOTS).map(|_| None).collect();
            self.leaving_inserts = (0..crate::insert::INSERT_SLOTS).map(|_| None).collect();
            self.retired_inserts = Vec::with_capacity(crate::insert::INSERT_SLOTS * 3);
            self.insert_chains = vec![None; crate::insert::MAX_INSERT_BUSES];
            self.insert_last_used = vec![0; crate::insert::MAX_INSERT_BUSES];
            self.insert_named = vec![u64::MAX; crate::insert::MAX_INSERT_BUSES];
            self.insert_fade_from = vec![u64::MAX; crate::insert::MAX_INSERT_BUSES];
        }
    }

    /// Names the chains the playing score has. A named chain keeps its
    /// inserts through each silence: an insert with no input sleeps, and
    /// the next note wakes the same insert. A chain with no name leaves
    /// after its tail, [`INSERT_IDLE_SECONDS`] at most.
    ///
    /// `list` is the number of the list, above 0. With no list, each chain
    /// leaves after [`INSERT_IDLE_SECONDS`] with no use.
    pub fn set_named_insert_chains(&mut self, list: u64, named: &[u64]) {
        self.init_insert_pool();
        self.insert_named_list = list;
        for (at, slot) in self.insert_named.iter_mut().enumerate() {
            *slot = named.get(at).copied().unwrap_or(u64::MAX);
        }
    }

    /// The number of the list of [`Self::set_named_insert_chains`].
    pub fn named_insert_list(&self) -> u64 {
        self.insert_named_list
    }

    /// The level of a chain in its fade, at one frame.
    #[inline]
    pub(super) fn insert_fade(from: u64, frame: u64, sample_rate: u32) -> f32 {
        let past = frame.saturating_sub(from) as f32;
        (1.0 - past / (CUT_FADE_SECS * sample_rate as f32)).max(0.0)
    }

    /// Install a logical slot. The caller frees displaced or refused copies.
    pub fn install_insert(
        &mut self,
        slot: crate::insert::InsertSlot,
        insert: Box<dyn crate::insert::OrbitInsert>,
    ) -> Option<Box<dyn crate::insert::OrbitInsert>> {
        self.init_insert_pool();
        let Some(bus) = self.claim_bus(slot) else {
            return Some(insert);
        };
        self.orbit_inserts[slot.physical(bus)].replace(insert)
    }

    /// A live copy waits for a matching accepted onset before replacing one.
    /// The caller frees a displaced or refused copy.
    pub fn prepare_insert(
        &mut self,
        slot: crate::insert::InsertSlot,
        insert: Box<dyn crate::insert::OrbitInsert>,
    ) -> Option<Box<dyn crate::insert::OrbitInsert>> {
        let Some(bus) = self.claim_bus(slot) else {
            return Some(insert);
        };
        self.prepared_inserts[slot.physical(bus)].replace(insert)
    }

    /// The bus of the chain of `slot`, in use now. With no free bus the
    /// copy is a refusal, and a chain the host names waits for the bus of a
    /// chain it does not name: see [`Self::reclaim_insert_chains`].
    fn claim_bus(&mut self, slot: crate::insert::InsertSlot) -> Option<usize> {
        let bus = slot
            .is_valid()
            .then(|| claim_insert_bus(&mut self.insert_chains, slot.chain))
            .flatten();
        match bus {
            Some(bus) => self.insert_last_used[bus] = self.frame,
            None => {
                self.insert_refusals += 1;
                self.insert_bus_wanted |=
                    slot.is_valid() && self.insert_named.contains(&slot.chain);
            }
        }
        bus
    }

    pub(crate) fn take_retired_insert(&mut self) -> Option<Box<dyn crate::insert::OrbitInsert>> {
        self.retired_inserts.pop()
    }

    /// The insert a logical slot currently holds.
    pub fn orbit_insert_key(
        &self,
        slot: crate::insert::InsertSlot,
    ) -> Option<crate::insert::InsertKey> {
        if !slot.is_valid() {
            return None;
        }
        let bus = self
            .insert_chains
            .iter()
            .position(|chain| *chain == Some(slot.chain))?;
        self.orbit_inserts
            .get(slot.physical(bus))?
            .as_ref()
            .map(|insert| insert.key())
    }

    /// The copies and the notes of a chain that found no free bus.
    pub fn insert_refusals(&self) -> u64 {
        self.insert_refusals
    }

    /// Gives the bus of a chain back, and its inserts to the host.
    ///
    /// ```text
    ///   the host names the chain     it stays, and sleeps when silent
    ///   no name                      it leaves after 1 s of silence,
    ///                                or after 30 s and a fade
    ///   no list, or no bus is free   it leaves after 30 s with no use
    /// ```
    ///
    /// An offline render names each chain. The chain of an orbit that a
    /// full list leaves out has the rule of no list. A chain the host names
    /// that finds no free bus takes the bus of the chain with no name and
    /// the oldest use: see [`Self::fade_for_named_chain`].
    pub(super) fn reclaim_insert_chains(&mut self) {
        let limit = self.sample_rate.saturating_mul(INSERT_IDLE_SECONDS);
        let quiet = self.sample_rate.saturating_mul(INSERT_QUIET_SECONDS);
        let fade = (CUT_FADE_SECS * self.sample_rate as f32) as u64;
        let listed = self.offline || self.insert_named_list != 0;
        let full = !self.insert_named.contains(&u64::MAX);
        // With no list, or with no free bus, a chain with no use for a long
        // time gives its bus back, named or not.
        let by_use = !listed || self.insert_chains.iter().all(Option::is_some);
        for bus in 0..self.insert_chains.len() {
            let Some(chain) = self.insert_chains[bus] else {
                continue;
            };
            let in_list = self.insert_named.contains(&chain);
            // A full list leaves out the chain of an orbit until the next
            // Csound note of the orbit. Such a chain keeps the rule of no
            // list, so a plugin chain does not take its bus.
            let unlisted = full && !in_list && chain < MAX_ORBITS as u64;
            let named = !listed || self.offline || in_list || unlisted;
            let by_use = by_use || unlisted;
            let unused = self.frame.saturating_sub(self.insert_last_used[bus]) >= u64::from(limit);
            // Only the rule by use takes a named chain, and only after the
            // limit: most turns end here.
            if named && !(by_use && unused) {
                self.insert_fade_from[bus] = u64::MAX;
                continue;
            }
            let held = self
                .voices
                .iter()
                .any(|voice| voice.insert_bus == Some(bus))
                || self
                    .pending
                    .slots
                    .iter()
                    .filter_map(|slot| slot.event.as_ref())
                    .any(|event| {
                        (event.controls.instrument.is_some()
                            || event.controls.effects.iter().any(Option::is_some))
                            && event.controls.insert_chain() == chain
                    });
            if held {
                // A chain in its fade goes on with the fade, and leaves once
                // no note holds it.
                if self.insert_fade_from[bus] > self.frame {
                    self.insert_fade_from[bus] = u64::MAX;
                }
                continue;
            }
            // True when each insert the chain uses was silent for `frames`.
            let silent_for = |frames: u32, awake: bool| {
                (0..=crate::insert::EFFECT_CHAIN).all(|stage| {
                    let slot = stage * crate::insert::MAX_INSERT_BUSES + bus;
                    let used = stage == crate::insert::EFFECT_CHAIN
                        || self.effect_order[bus].contains(&(stage as u8));
                    self.orbit_inserts[slot].as_ref().is_none_or(|insert| {
                        !(awake && insert.busy())
                            && (!used || self.insert_idle_frames[slot] >= frames)
                    })
                })
            };
            let mut leaves = by_use && unused && silent_for(limit, true);
            if !leaves && !named {
                // The chain has no note now. Its tail has the time of the
                // limit, then a fade.
                if self.insert_fade_from[bus] == u64::MAX {
                    self.insert_fade_from[bus] = self.frame.saturating_add(u64::from(limit));
                }
                let faded = self.frame >= self.insert_fade_from[bus].saturating_add(fade);
                // A chain with a new insert has a moment for its name and
                // its first note.
                let settled =
                    self.frame.saturating_sub(self.insert_last_used[bus]) >= u64::from(quiet);
                leaves = faded || (settled && silent_for(quiet, false));
            }
            // Each copy of the chain needs a place in the list of the copies
            // that go back.
            let copies = || -> usize {
                let stages = 0..=crate::insert::EFFECT_CHAIN;
                let slots = stages.map(|stage| stage * crate::insert::MAX_INSERT_BUSES + bus);
                let copies = slots.map(|slot| {
                    usize::from(self.orbit_inserts[slot].is_some())
                        + usize::from(self.prepared_inserts[slot].is_some())
                        + usize::from(self.leaving_inserts[slot].is_some())
                });
                copies.sum()
            };
            if !leaves || self.retired_inserts.len() + copies() > self.retired_inserts.capacity() {
                continue;
            }
            for stage in 0..=crate::insert::EFFECT_CHAIN {
                let slot = stage * crate::insert::MAX_INSERT_BUSES + bus;
                if let Some(insert) = self.orbit_inserts[slot].take() {
                    self.retired_inserts.push(insert);
                }
                if let Some(insert) = self.prepared_inserts[slot].take() {
                    self.retired_inserts.push(insert);
                }
                if let Some(insert) = self.leaving_inserts[slot].take() {
                    self.retired_inserts.push(insert);
                }
                self.insert_mixes[slot] = InsertMix::default();
                self.insert_idle_frames[slot] = 0;
            }
            self.insert_chains[bus] = None;
            self.insert_fade_from[bus] = u64::MAX;
            self.effect_order[bus] = [crate::insert::NO_STAGE; crate::insert::EFFECT_CHAIN];
            self.insert_generations[bus] = 0;
            self.insert_fading &= !(1u64 << bus);
            self.instrument_to_effect[bus] = false;
            self.instrument_gains[bus] = InstrumentGain::default();
            self.instrument_filters[bus] = [FilterChain::default(); 2];
            self.insert_postgains[bus] = InsertPostgain::default();
        }
        if std::mem::take(&mut self.insert_bus_wanted) {
            self.fade_for_named_chain();
        }
    }

    /// Starts the fade of the chain with no name and the oldest use, for a
    /// chain the host names that found no free bus. A note with no chain of
    /// its own plays on the chain of its orbit, and such a chain keeps its
    /// bus. Each voice of the chain and each note of its instrument end with
    /// the fade. The bus is free after the fade: see
    /// [`Self::reclaim_insert_chains`].
    fn fade_for_named_chain(&mut self) {
        let from = self.frame;
        // A free bus, or a chain in its fade now, gives a bus soon.
        let fading = self.insert_fade_from.iter().any(|at| *at <= from);
        if fading || self.insert_chains.contains(&None) {
            return;
        }
        let chains = self.insert_chains.iter().enumerate();
        // The chain of an orbit has a number below MAX_ORBITS.
        let unnamed = chains.filter(|(_, chain)| {
            chain.is_some_and(|chain| {
                chain >= MAX_ORBITS as u64 && !self.insert_named.contains(&chain)
            })
        });
        let Some((bus, _)) = unnamed.min_by_key(|(bus, _)| self.insert_last_used[*bus]) else {
            return;
        };
        self.insert_fade_from[bus] = from;
        for voice in self.voices.iter_mut() {
            if voice.insert_bus == Some(bus) {
                let stop_at = from.saturating_sub(voice.start_frame) as f32
                    / self.sample_rate as f32
                    + CUT_FADE_SECS;
                voice.stop_secs = voice.stop_secs.min(stop_at);
                voice.cut_fade_frame =
                    Some(voice.cut_fade_frame.map_or(from, |fade| fade.min(from)));
            }
        }
        let instrument = crate::insert::EFFECT_CHAIN * crate::insert::MAX_INSERT_BUSES + bus;
        if let Some(instrument) = &mut self.orbit_inserts[instrument] {
            instrument.cut_notes(0);
        }
    }

    /// Ends each fade that is over. The copy in a leaving place goes back. A
    /// stage at the level 0 leaves the order, and on a chain of its own its
    /// copy goes back too. On the chain of an orbit the copy sleeps in its
    /// slot. A copy with no place in the list of the copies that go back
    /// waits for the next segment, and its chain stays in the fading set.
    pub(super) fn finish_insert_fades(&mut self) {
        let mut buses = self.insert_fading;
        while buses != 0 {
            let bus = buses.trailing_zeros() as usize;
            buses &= buses - 1;
            // The chain of an orbit has a number below MAX_ORBITS.
            let own = self.insert_chains[bus].is_some_and(|chain| chain >= MAX_ORBITS as u64);
            let mut ended = true;
            for stage in 0..=crate::insert::EFFECT_CHAIN {
                let slot = stage * crate::insert::MAX_INSERT_BUSES + bus;
                let full = self.retired_inserts.len() == self.retired_inserts.capacity();
                if self.insert_mixes[slot].moving()
                    || (full && self.leaving_inserts[slot].is_some())
                {
                    ended = false;
                    continue;
                }
                if let Some(leaving) = self.leaving_inserts[slot].take() {
                    self.retired_inserts.push(leaving);
                }
                let out = stage < crate::insert::EFFECT_CHAIN
                    && self.insert_mixes[slot].active.target() == 0.0;
                if !out {
                    continue;
                }
                if own && self.orbit_inserts[slot].is_some() {
                    if self.retired_inserts.len() == self.retired_inserts.capacity() {
                        ended = false;
                        continue;
                    }
                    if let Some(insert) = self.orbit_inserts[slot].take() {
                        self.retired_inserts.push(insert);
                    }
                    self.insert_idle_frames[slot] = 0;
                }
                leave_order(&mut self.effect_order[bus], stage as u8);
                self.insert_mixes[slot] = InsertMix::default();
            }
            if ended {
                self.insert_fading &= !(1u64 << bus);
            }
        }
    }

    /// Gives back the copy with `key` in `slot`, a copy the score no longer
    /// has. A copy that waits for its first note goes back. The active copy
    /// goes back when it is not busy and its stage is not heard: an effect
    /// stage leaves the order, and an instrument loses its level and its
    /// filters. An effect stage is heard while its chain sounds, and while a
    /// voice goes through the chain, also in a silence of its note. Else the
    /// copy stays, and the output asks again later. A copy with no place in
    /// the list of the copies that go back stays too. A `stopped` output past
    /// its stop ramp makes no sound, so its active copy goes back also when
    /// it is busy or heard.
    pub(crate) fn release_insert(
        &mut self,
        slot: crate::insert::InsertSlot,
        key: crate::insert::InsertKey,
        stopped: bool,
    ) {
        let held = |chain: &Option<u64>| *chain == Some(slot.chain);
        let bus = slot
            .is_valid()
            .then(|| self.insert_chains.iter().position(held))
            .flatten();
        let Some(bus) = bus else { return };
        if self.retired_inserts.len() == self.retired_inserts.capacity() {
            return;
        }
        let at = slot.physical(bus);
        if let Some(waiting) = self.prepared_inserts[at].take_if(|waiting| waiting.key() == key) {
            self.retired_inserts.push(waiting);
            return;
        }
        let Some(active) = self.orbit_inserts[at]
            .as_ref()
            .filter(|active| active.key() == key)
        else {
            return;
        };
        let instrument = usize::from(slot.stage) == crate::insert::EFFECT_CHAIN;
        let heard = if instrument {
            self.instrument_gains[bus].quiet < INSERT_QUIET_FRAMES
        } else {
            let level = &self.insert_postgains[bus];
            // A voice that goes through the chain feeds it until the voice
            // ends, also in a silence of its note.
            let fed = |voice: &Voice| voice.through_insert && voice.insert_bus == Some(bus);
            self.effect_order[bus].contains(&slot.stage)
                && (level.quiet.min(level.unfed) < INSERT_QUIET_FRAMES
                    || self.voices.iter().any(fed))
        };
        if !stopped && (heard || active.busy()) {
            return;
        }
        if let Some(active) = self.orbit_inserts[at].take() {
            self.retired_inserts.push(active);
        }
        self.insert_mixes[at] = InsertMix::default();
        self.insert_idle_frames[at] = 0;
        if instrument {
            self.instrument_gains[bus] = InstrumentGain::default();
            self.instrument_filters[bus] = [FilterChain::default(); 2];
        } else {
            leave_order(&mut self.effect_order[bus], slot.stage);
        }
    }

    /// Offline renders build a missing insert at activation with this
    /// provider. The live path ignores the provider and waits for an install.
    pub fn set_insert_provider(&mut self, provider: Arc<crate::insert::InsertProvider>) {
        self.insert_provider = Some(provider);
    }

    /// Notes that asked for an insert their chain did not hold.
    pub fn missing_insert_events(&self) -> u64 {
        self.missing_insert_events
    }

    /// Takes the plugin workers of the output. From the next sub-block on,
    /// each chain of inserts that run on any thread posts its blocks to
    /// them.
    pub(crate) fn set_plugin_pool(&mut self, shared: Arc<crate::pool::PoolShared>) {
        self.plugin_pool = crate::pool::PoolHandle::new(shared);
    }

    /// Ends a sub-block of a buffer of the callback, `frames` into the
    /// buffer. `busy` gives the time the callback took so far. It runs only
    /// while the gate of the workers is closed, and not in the first buffer.
    #[cfg(feature = "device-audio")]
    pub(crate) fn end_pool_sub_block(&self, frames: usize, busy: impl FnOnce() -> u64) {
        if let Some(pool) = &self.plugin_pool {
            pool.end_sub_block(Self::frames_nanos(frames, self.sample_rate), busy);
        }
    }

    /// Ends a buffer of the callback of `frames` that took `busy_nanos`, and
    /// gives the figures of its plugin jobs. A `stopped` buffer leaves the
    /// gate of the workers as it was. `None` with no plugin workers.
    #[cfg(feature = "device-audio")]
    pub(crate) fn end_pool_buffer(
        &mut self,
        frames: usize,
        busy_nanos: u64,
        stopped: bool,
    ) -> Option<crate::pool::PoolFigures> {
        let pool = self.plugin_pool.as_mut()?;
        let period_nanos = Self::frames_nanos(frames, self.sample_rate);
        Some(pool.end_buffer(busy_nanos, period_nanos, stopped))
    }

    /// The time of `frames` at `rate`, in nanoseconds.
    #[cfg(feature = "device-audio")]
    fn frames_nanos(frames: usize, rate: u32) -> u64 {
        u64::try_from(frames as u128 * 1_000_000_000 / u128::from(rate.max(1))).unwrap_or(u64::MAX)
    }

    /// Test support: each chain of inserts that run on any thread posts its
    /// blocks to `workers` threads with no real-time class, and the gate
    /// stays open. The threads stop when the returned pool drops.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn start_test_pool(&mut self, workers: usize) -> Option<impl Sized + use<>> {
        let pool = crate::pool::PluginPool::start_test_pool(workers, true)?;
        self.set_plugin_pool(Arc::clone(pool.shared()));
        Some(pool)
    }

    /// Test support: the callback leaves each block it posts to the
    /// threads of [`Self::start_test_pool`], and waits for them at the
    /// join. So a worker runs each block of 32 frames or more of a chain
    /// that runs on any thread.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn leave_plugin_blocks_to_workers(&self) {
        if let Some(pool) = &self.plugin_pool {
            pool.leave_jobs_to_workers();
        }
    }

    #[cfg(feature = "device-audio")]
    fn finite_insert_lead(&self) -> u64 {
        if self
            .orbit_inserts
            .iter()
            .flatten()
            .any(|insert| insert.renders_ahead())
        {
            // Leave time beyond the worker's 512-frame rendering runway.
            1024
        } else {
            0
        }
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn prime_finite_inserts(&mut self) {
        let lead = self.finite_insert_lead();
        if lead == 0 {
            return;
        }
        self.notify_pending_insert_window(0, lead);
        for insert in self.orbit_inserts.iter_mut().flatten() {
            if insert.renders_ahead() {
                insert.prime(lead as usize);
            }
        }
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn notify_pending_inserts_early(&mut self, frames: usize) {
        let lead = self.finite_insert_lead();
        if lead == 0 {
            return;
        }
        let start = self.frame.saturating_add(lead);
        self.notify_pending_insert_window(start, start.saturating_add(frames as u64));
    }

    #[cfg(feature = "device-audio")]
    fn notify_pending_insert_window(&mut self, start: u64, end: u64) {
        // Adjacent windows admit each pending note once, without sending
        // the whole finite score into a worker's bounded command queue.
        for position in 0..self.pending.order.len() {
            let index = self.pending.order[position];
            let event = self.pending.slots[index]
                .event
                .as_ref()
                .filter(|event| (start..end).contains(&event.onset_frame))
                .copied();
            if let Some(event) = event {
                self.notify_inserts_early(&event, self.frame);
            }
        }
    }

    /// Gives the inserts that render ahead of the callback their note now,
    /// when the event is admitted, a schedule lead before the frame it
    /// sounds. Such an insert has the frame of an onset ready before the
    /// callback asks for it, so it needs the note that early to place the
    /// onset on its own frame. The activation of the onset delivers the
    /// note again to the insert that plays it, and a prepared score plays
    /// once, so nothing sounds twice. Nothing here claims a bus, swaps an
    /// insert or builds one: activation does that, and an insert that held
    /// no matching box receives the note there.
    pub(crate) fn notify_inserts_early<K>(&mut self, event: &OnsetEvent<K>, start_frame: u64) {
        self.init_insert_pool();
        let chain = event.controls.insert_chain();
        let Some(bus) = self
            .insert_chains
            .iter()
            .position(|held| *held == Some(chain))
        else {
            return;
        };
        let frames = event
            .onset_frame
            .saturating_sub(start_frame)
            .min(u64::from(u32::MAX)) as u32;
        for (place, wanted) in event.controls.effects.iter().enumerate() {
            if let Some(wanted) = wanted {
                self.notify_one_insert(
                    crate::insert::effect_slot(chain, wanted.stage(place)),
                    wanted,
                    event.generation,
                    bus,
                    frames,
                );
            }
        }
        if let Some(wanted) = &event.controls.instrument {
            self.notify_one_insert(
                crate::insert::instrument_slot(chain),
                wanted,
                event.generation,
                bus,
                frames,
            );
        }
    }

    /// Delivers one note of `generation` to the box that will play it: the
    /// held box when it is the wanted one, else the prepared box activation
    /// swaps in.
    fn notify_one_insert(
        &mut self,
        logical: crate::insert::InsertSlot,
        wanted: &crate::insert::InsertControls,
        generation: u64,
        bus: usize,
        frames: u32,
    ) {
        let slot = logical.physical(bus);
        let holds = |inserts: &Vec<Option<Box<dyn crate::insert::OrbitInsert>>>| {
            inserts
                .get(slot)
                .and_then(|insert| insert.as_ref())
                .is_some_and(|insert| insert.key() == wanted.key && insert.renders_ahead())
        };
        let (inserts, matched) = match (holds(&self.orbit_inserts), holds(&self.prepared_inserts)) {
            (true, _) => (&mut self.orbit_inserts, true),
            (_, true) => (&mut self.prepared_inserts, true),
            _ => (&mut self.orbit_inserts, false),
        };
        if !matched {
            return;
        }
        let Some(insert) = inserts.get_mut(slot).and_then(|insert| insert.as_mut()) else {
            return;
        };
        let values = &self.insert_values;
        deliver(insert.as_mut(), logical, wanted, generation, values, frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod insert_routing_tests {
        use super::*;
        use crate::insert::{InsertControls, InsertKey, InsertNote, InsertParam, OrbitInsert};
        use crate::live_control::Ramp;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        struct StatefulInsert {
            key: InsertKey,
            memory: [f32; 2],
            held: u32,
            resets: Arc<AtomicUsize>,
        }

        impl OrbitInsert for StatefulInsert {
            fn key(&self) -> InsertKey {
                self.key
            }

            fn set_param(&mut self, _param: InsertParam, _frames: u32) {}

            fn note(&mut self, note: InsertNote, frames: u32) {
                assert_eq!(frames, 0);
                self.held = note.frames;
            }

            fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                for (left, right) in left.iter_mut().zip(right) {
                    let instrument = if self.held > 0 { 0.125 } else { 0.0 };
                    self.held = self.held.saturating_sub(1);
                    for (channel, sample) in [left, right].into_iter().enumerate() {
                        self.memory[channel] = *sample + instrument + self.memory[channel] * 0.5;
                        *sample = self.memory[channel];
                    }
                }
            }

            fn reset(&mut self) {
                self.resets.fetch_add(1, Ordering::Relaxed);
                self.memory = [0.0; 2];
                self.held = 0;
            }
        }

        #[test]
        fn instrument_events_keep_their_offsets_across_replacements() {
            struct Scores {
                key: InsertKey,
                events: [(u64, u32); 2],
                count: usize,
                frame: u32,
                level: f32,
            }

            impl OrbitInsert for Scores {
                fn key(&self) -> InsertKey {
                    self.key
                }

                fn set_param(&mut self, _: InsertParam, _: u32) {}

                fn score(&mut self, score: u64, frames: u32) {
                    assert_eq!(self.key.plugin, u32::MAX);
                    self.events[self.count] = (score, self.frame + frames);
                    self.count += 1;
                }

                fn note(&mut self, note: InsertNote, frames: u32) {
                    assert_ne!(self.key.plugin, u32::MAX);
                    self.events[self.count] = (note.pitch as u64, self.frame + frames);
                    self.count += 1;
                }

                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for (left, right) in left.iter_mut().zip(right) {
                        for (score, frame) in &self.events[..self.count] {
                            if *frame == self.frame {
                                self.level = *score as f32 / 8.0;
                            }
                        }
                        (*left, *right) = (self.level, self.level);
                        self.frame += 1;
                    }
                }

                fn any_thread(&self) -> bool {
                    true
                }
            }

            let score_key = InsertKey {
                plugin: u32::MAX,
                preset: 1,
            };
            let next_score_key = InsertKey {
                plugin: u32::MAX,
                preset: 2,
            };
            let midi_key = InsertKey {
                plugin: 1,
                preset: 0,
            };
            let build = |key| {
                Box::new(Scores {
                    key,
                    events: [(0, 0); 2],
                    count: 0,
                    frame: 0,
                    level: 0.0,
                }) as Box<dyn OrbitInsert>
            };
            for (first_key, next_key, first_frame, held_key) in [
                (score_key, next_score_key, 37, None),
                (score_key, midi_key, 37, None),
                (midi_key, score_key, 37, None),
                (midi_key, score_key, 0, Some(score_key)),
            ] {
                // Each case also runs with its instrument on plugin workers.
                let cases = [(false, false), (false, true), (true, false), (true, true)];
                for (live, routed, pooled) in cases
                    .into_iter()
                    .flat_map(|(live, routed)| [(live, routed, false), (live, routed, true)])
                {
                    if live && held_key.is_some() {
                        continue;
                    }
                    let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
                    let _pool = pooled.then(|| test_pool(&mut backend, 2));
                    backend.orbit_pairs[0] = u8::from(routed);
                    let slot = crate::insert::instrument_slot(0);
                    if live {
                        assert!(backend.install_insert(slot, build(first_key)).is_none());
                        assert!(backend.prepare_insert(slot, build(next_key)).is_none());
                        backend.allow_inline_reverb = false;
                    } else {
                        backend.set_insert_provider(Arc::new(move |key, _, _| Some(build(key))));
                        if let Some(held_key) = held_key {
                            assert!(backend.install_insert(slot, build(held_key)).is_none());
                        }
                    }
                    for (score, frame, key) in [
                        (1, first_frame, first_key),
                        (2, 91, next_key),
                        (3, 191, next_key),
                    ] {
                        let mut event = OnsetEvent::new(frame, 440.0, 1.0, 0.02);
                        event.controls.orbit = 0;
                        let controls = InsertControls::new(key);
                        event.controls.instrument = Some(if key.plugin == u32::MAX {
                            controls.with_score(score)
                        } else {
                            controls.with_note(InsertNote {
                                pitch: score as f32,
                                velocity: 1.0,
                                frames: 960,
                            })
                        });
                        backend.note(event);
                    }
                    let mut output = [0.0; 512];
                    backend.process_block(&mut output, 256);
                    let samples = if routed {
                        backend.orbit_mix(0, 256)
                    } else {
                        &output
                    };
                    // From frame 91 the first instrument fades out in 5 ms
                    // under the next one.
                    let mut fade = Ramp::new(0, 0.0);
                    fade.declick_from(Ramp::new(0, 1.0), 48_000);
                    for (frame, samples) in samples.as_chunks::<2>().0.iter().enumerate() {
                        let level = if frame < first_frame as usize {
                            0.0
                        } else if frame < 91 {
                            0.125
                        } else if frame < 191 {
                            0.25 + 0.125 * fade.next()
                        } else {
                            0.375 + 0.125 * fade.next()
                        };
                        assert_eq!(
                            *samples, [level; 2],
                            "frame {frame}, live={live}, routed={routed}, pooled={pooled}, first={first_key:?}, next={next_key:?}, held={held_key:?}"
                        );
                    }
                }
            }

            /// Renders ahead at its plugin number / 8. A frame plays once
            /// `flush` made it ready, and a frame that is not ready is silent.
            struct Ahead(InsertKey, usize);
            impl OrbitInsert for Ahead {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    self.collect(left, right);
                }
                fn submit(&mut self, _: &mut [f32], _: &mut [f32]) {}
                fn collect(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for (left, right) in left.iter_mut().zip(right) {
                        let ready = self.1 > 0;
                        self.1 = self.1.saturating_sub(1);
                        let level = if ready {
                            self.0.plugin as f32 / 8.0
                        } else {
                            0.0
                        };
                        (*left, *right) = (level, level);
                    }
                }
                fn flush(&mut self, frames: usize) {
                    self.1 += frames;
                }
                fn renders_ahead(&self) -> bool {
                    true
                }
            }
            // An offline render waits for each block of an instrument that
            // renders ahead, also of the instrument that leaves in a fade.
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            backend.render_offline();
            backend.set_insert_provider(Arc::new(|key, _, _| {
                Some(Box::new(Ahead(key, 0)) as Box<dyn OrbitInsert>)
            }));
            for (frame, plugin) in [(0, 1), (64, 2)] {
                let mut event = OnsetEvent::new(frame, 440.0, 1.0, 0.02);
                event.controls.orbit = 0;
                event.controls.instrument =
                    Some(InsertControls::new(InsertKey { plugin, preset: 0 }));
                backend.note(event);
            }
            let mut output = [0.0; 512];
            backend.process_block(&mut output, 256);
            let mut fade = Ramp::new(0, 0.0);
            fade.declick_from(Ramp::new(0, 1.0), 48_000);
            for (frame, samples) in output.as_chunks::<2>().0.iter().enumerate() {
                let level = if frame < 64 {
                    0.125
                } else {
                    0.25 + 0.125 * fade.next()
                };
                assert_eq!(*samples, [level; 2], "frame {frame}");
            }

            /// Renders ahead as `Ahead` does, and plays each score for its
            /// length in frames, as a Csound note plays for its p3. It keeps
            /// its last 2 notes.
            struct Notes {
                ahead: Ahead,
                frame: u64,
                notes: [(u64, u64); 2],
            }
            impl OrbitInsert for Notes {
                fn key(&self) -> InsertKey {
                    self.ahead.key()
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn score(&mut self, length: u64, frames: u32) {
                    let start = self.frame + u64::from(frames);
                    self.notes = [self.notes[1], (start, start + length)];
                }
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    self.collect(left, right);
                }
                fn submit(&mut self, _: &mut [f32], _: &mut [f32]) {}
                fn collect(&mut self, left: &mut [f32], right: &mut [f32]) {
                    self.ahead.collect(left, right);
                    for (left, right) in left.iter_mut().zip(right) {
                        let frame = self.frame;
                        let sounds = self
                            .notes
                            .iter()
                            .any(|&(start, end)| (start..end).contains(&frame));
                        if !sounds {
                            (*left, *right) = (0.0, 0.0);
                        }
                        self.frame += 1;
                    }
                }
                fn flush(&mut self, frames: usize) {
                    self.ahead.flush(frames);
                }
                fn renders_ahead(&self) -> bool {
                    true
                }
            }
            // The swaps of a recorded set with Csound, at 8 kHz. The second
            // note of plugin 1 ends on the frame of the swap to plugin 2, or
            // sounds 64 frames past it. Both swaps are heard, and the
            // instrument that leaves fades out in 5 ms (40 frames).
            for length in [2000, 2064] {
                let mut backend = ScalarBackend::prepared(8_000, 8).unwrap();
                backend.render_offline();
                backend.set_insert_provider(Arc::new(|key, _, _| {
                    let notes = Notes {
                        ahead: Ahead(key, 0),
                        frame: 0,
                        notes: [(0, 0); 2],
                    };
                    Some(Box::new(notes) as Box<dyn OrbitInsert>)
                }));
                for (frame, plugin, frames) in [
                    (0, 1, 2000),
                    (2000, 1, length),
                    (4000, 2, 2000),
                    (4064, 3, 2000),
                ] {
                    let mut event = OnsetEvent::new(frame, 440.0, 1.0, 0.02);
                    event.controls.orbit = 0;
                    let controls = InsertControls::new(InsertKey { plugin, preset: 0 });
                    event.controls.instrument = Some(controls.with_score(frames));
                    backend.note(event);
                }
                let fade = || {
                    let mut fade = Ramp::new(0, 0.0);
                    fade.declick_from(Ramp::new(0, 1.0), 8_000);
                    fade
                };
                let (mut first, mut second) = (fade(), fade());
                // Plugin 1 plays in its fade only while its note lasts.
                let before = if length > 2000 { 0.125 } else { 0.0 };
                // Checks the output of a render that starts on frame `start`.
                let mut check = |output: &[f32], start: usize| {
                    for (frame, samples) in output.as_chunks::<2>().0.iter().enumerate() {
                        let frame = start + frame;
                        let level = if frame < 4000 {
                            0.125
                        } else if frame < 4064 {
                            0.25 + before * first.next()
                        } else {
                            0.375 + 0.25 * second.next()
                        };
                        assert_eq!(*samples, [level; 2], "frame {frame}, length {length}");
                    }
                };
                let mut output = vec![0.0; 2 * 4032];
                backend.process_block(&mut output, 4032);
                check(&output, 0);
                // The swap at 4000 is heard, so plugin 1 fades out in the
                // leaving place, also when its note ended.
                let slot = crate::insert::instrument_slot(0).physical(0);
                let leaving = backend.leaving_inserts[slot].as_ref();
                let plugin = leaving.map(|insert| insert.key().plugin);
                let mix = backend.insert_mixes[slot];
                assert_eq!(plugin, Some(1), "length {length}");
                assert!(mix.leaving.is_moving(), "length {length}");
                let mut output = vec![0.0; 2 * 192];
                backend.process_block(&mut output, 192);
                check(&output, 4032);
            }

            let ring = Arc::new(crate::input::InputRing::new());
            ring.set_sample_rate(24_000);
            ring.set_channels(1);
            ring.write(
                &(0..8192)
                    .map(|frame| frame as f32 / 8192.0)
                    .collect::<Vec<_>>(),
                1,
            );
            let render_input = |replace| {
                let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
                backend.set_input(Some(Arc::clone(&ring)));
                backend.orbit_pairs[0] = 1;
                backend.set_insert_provider(Arc::new(move |key, _, _| Some(build(key))));
                let mut input = OnsetEvent::new(0, 440.0, 1.0, 1.0);
                input.synth = Some(crate::SynthSource::Input { channel: 0 });
                backend.note(input);
                if replace {
                    for (frame, key) in [(37, score_key), (91, next_score_key)] {
                        let mut event = OnsetEvent::new(frame, 440.0, 1.0, 0.02);
                        event.controls.orbit = 0;
                        event.controls.instrument = Some(InsertControls::new(key).with_score(1));
                        backend.note(event);
                    }
                }
                let mut output = vec![0.0; 8192];
                backend.process_block(&mut output, 4096);
                output
            };
            let input = render_input(false);
            assert!(input.iter().any(|sample| *sample != 0.0));
            assert_eq!(render_input(true), input);
        }

        #[test]
        fn independent_insert_chains_sum_into_one_orbit_without_processing_dry_siblings() {
            struct Gain(InsertKey);
            impl OrbitInsert for Gain {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for sample in left.iter_mut().chain(right) {
                        *sample *= self.0.plugin as f32 * 0.25;
                    }
                }
                fn any_thread(&self) -> bool {
                    true
                }
            }
            let builds = Arc::new(AtomicUsize::new(0));
            let make_backend = |tracks: &[usize]| {
                let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
                let builds = Arc::clone(&builds);
                backend.set_insert_provider(Arc::new(move |key, _, _| {
                    builds.fetch_add(1, Ordering::Relaxed);
                    Some(Box::new(Gain(key)))
                }));
                for &track in tracks {
                    for frame in [0, 128, 512] {
                        let mut event =
                            OnsetEvent::new(frame, 220.0 + track as f32 * 110.0, 0.03, 0.02);
                        event.controls.orbit = 1;
                        if track < 2 {
                            event.controls.insert_chain = Some(100 + track as u64);
                            event.controls.effects[0] = Some(InsertControls::new(InsertKey {
                                plugin: 1 + track as u32 * 5,
                                preset: 0,
                            }));
                            if track == 1 {
                                event.controls.effects[1] = Some(InsertControls::new(InsertKey {
                                    plugin: 2,
                                    preset: 0,
                                }));
                            }
                        }
                        backend.note(event);
                    }
                }
                backend
            };
            let mut combined = make_backend(&[0, 1, 2]);
            let mut stems = [make_backend(&[0]), make_backend(&[1]), make_backend(&[2])];
            // The same chains on 3 plugin workers give the same samples.
            let mut pooled = make_backend(&[0, 1, 2]);
            let _pool = test_pool(&mut pooled, 3);
            let mut peak = 0.0f32;
            for _ in 0..16 {
                let mut expected = [0.0; 256];
                for stem in &mut stems {
                    let mut samples = [0.0; 256];
                    stem.process_block(&mut samples, 128);
                    for (sum, sample) in expected.iter_mut().zip(samples) {
                        *sum += sample;
                    }
                }
                let mut actual = [0.0; 256];
                combined.process_block(&mut actual, 128);
                for (actual, expected) in actual.into_iter().zip(expected) {
                    peak = peak.max(actual.abs());
                    assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
                }
                let mut on_workers = [0.0; 256];
                pooled.process_block(&mut on_workers, 128);
                assert_eq!(on_workers, actual);
            }
            assert!(peak > 0.001);
            assert_eq!(builds.load(Ordering::Relaxed), 9);
            assert_eq!(combined.missing_insert_events(), 0);
            assert_eq!(pooled.missing_insert_events(), 0);
            assert_eq!(combined.insert_chains.iter().flatten().count(), 2);
        }

        /// The output of a plugin of the split chains: a level, a square or
        /// a half, by the plugin number.
        fn split_render(plugin: u32, left: &mut [f32], right: &mut [f32]) {
            for (channel, samples) in [left, right].into_iter().enumerate() {
                for sample in samples {
                    *sample = match plugin % 3 {
                        1 => plugin as f32 * 0.02 * (channel + 1) as f32,
                        2 => *sample * *sample,
                        _ => *sample * 0.5,
                    };
                }
            }
        }

        /// The split chains on orbit 1, with three notes each: an instrument
        /// through the effects at stages 1 and 0, in this order, one through
        /// stages 0 and 3, a native chain through each stage, and with
        /// `include_dry` two instruments with no effect. Each note sends to a
        /// reverb and a delay.
        fn split_chains(
            include_dry: bool,
            provider: Arc<crate::insert::InsertProvider>,
        ) -> ScalarBackend {
            let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
            backend.set_insert_provider(provider);
            for (chain, plugin_base) in [9_u32, 0, 3, 6, 12].into_iter().enumerate() {
                if !include_dry && (chain == 0 || chain == 4) {
                    continue;
                }
                let controls = |offset| {
                    InsertControls::new(InsertKey {
                        plugin: plugin_base + offset,
                        preset: 0,
                    })
                };
                for frame in [0, 63, 128] {
                    let mut event = OnsetEvent::new(frame, 440.0, 0.5, 0.02);
                    event.controls.orbit = 1;
                    event.controls.insert_chain = Some(100 + chain as u64);
                    event.controls.postgain = if frame == 63 { 0.25 } else { 2.0 };
                    event.controls.reverb = Some(crate::backend::ReverbControls {
                        wet: 0.25,
                        size_secs: 0.04,
                        fade_secs: 0.01,
                        lp_start_hz: 8_000.0,
                        lp_end_hz: 1_000.0,
                        ir: None,
                    });
                    event.controls.delay = Some(crate::backend::DelayControls {
                        wet: 0.3,
                        time_secs: 128.0 / 48_000.0,
                        feedback: 0.25,
                    });
                    if chain != 3 {
                        event.controls.instrument = Some(controls(1).with_note(InsertNote {
                            pitch: 69.0,
                            velocity: 1.0,
                            frames: 512,
                        }));
                    } else {
                        event.controls.pan = Some(0.3);
                        event.controls.effects[2] = Some(controls(5));
                        event.controls.effects[3] = Some(controls(6));
                    }
                    if (1..=3).contains(&chain) {
                        let (mut first, mut second) = (controls(2), controls(3));
                        if chain == 1 {
                            first.set_stage(1);
                            second.set_stage(0);
                        }
                        event.controls.effects[0] = Some(first);
                        event.controls.effects[if chain == 2 { 3 } else { 1 }] = Some(second);
                    }
                    backend.note(event);
                }
            }
            backend
        }

        #[test]
        fn independent_inserts_pipeline_snapshots_before_collecting() {
            type Calls = Arc<std::sync::Mutex<Vec<(u32, bool, usize)>>>;
            struct SplitInsert {
                key: InsertKey,
                deferred: bool,
                pending: usize,
                output: [[f32; crate::reverb::REVERB_BLOCK]; 2],
                calls: Calls,
            }
            impl OrbitInsert for SplitInsert {
                fn key(&self) -> InsertKey {
                    self.key
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    split_render(self.key.plugin, left, right);
                }
                fn submit(&mut self, left: &mut [f32], right: &mut [f32]) {
                    if !self.deferred {
                        self.process(left, right);
                        return;
                    }
                    assert_eq!(self.pending, 0, "only one block may wait");
                    self.pending = left.len();
                    self.output[0][..left.len()].copy_from_slice(left);
                    self.output[1][..right.len()].copy_from_slice(right);
                    let (captured_left, captured_right) = self.output.split_at_mut(1);
                    split_render(
                        self.key.plugin,
                        &mut captured_left[0][..left.len()],
                        &mut captured_right[0][..right.len()],
                    );
                    self.calls
                        .lock()
                        .unwrap()
                        .push((self.key.plugin, false, left.len()));
                }
                fn collect(&mut self, left: &mut [f32], right: &mut [f32]) {
                    if !self.deferred {
                        return;
                    }
                    assert_eq!(std::mem::take(&mut self.pending), left.len());
                    self.calls
                        .lock()
                        .unwrap()
                        .push((self.key.plugin, true, left.len()));
                    left.copy_from_slice(&self.output[0][..left.len()]);
                    right.copy_from_slice(&self.output[1][..right.len()]);
                }
            }
            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            let make = |deferred, include_dry: bool| {
                let calls = Arc::clone(&calls);
                split_chains(
                    include_dry,
                    Arc::new(move |key, _, _| {
                        Some(Box::new(SplitInsert {
                            key,
                            deferred,
                            pending: 0,
                            output: [[0.0; crate::reverb::REVERB_BLOCK]; 2],
                            calls: Arc::clone(&calls),
                        }))
                    }),
                )
            };
            for include_dry in [false, true] {
                let mut synchronous = make(false, include_dry);
                let mut deferred = make(true, include_dry);
                for frames in [1, 127, 129, 256] {
                    let mut expected = vec![0.0; frames * 2];
                    let mut actual = vec![0.0; frames * 2];
                    synchronous.process_block(&mut expected, frames);
                    deferred.process_block(&mut actual, frames);
                    assert_eq!(actual, expected);
                    assert!(actual.iter().any(|sample| *sample > 0.0));
                    let calls = std::mem::take(&mut *calls.lock().unwrap());
                    let chunks = calls.chunks_exact(if include_dry { 24 } else { 20 });
                    assert!(chunks.remainder().is_empty());
                    assert_eq!(chunks.len(), frames.div_ceil(crate::reverb::REVERB_BLOCK));
                    for (index, calls) in chunks.enumerate() {
                        let count = (frames - index * crate::reverb::REVERB_BLOCK)
                            .min(crate::reverb::REVERB_BLOCK);
                        assert!(calls.iter().all(|call| call.2 == count));
                        let at = |plugin, collected| {
                            let mut matching = calls
                                .iter()
                                .enumerate()
                                .filter(|(_, call)| call.0 == plugin && call.1 == collected);
                            let index = matching.next().unwrap().0;
                            assert!(matching.next().is_none(), "each block is exchanged once");
                            index
                        };
                        // Each chain in signal order. Plugin 2 at stage 1
                        // comes before plugin 3 at stage 0.
                        for chain in [&[1, 2, 3][..], &[4, 5, 6][..], &[8, 9, 11, 12][..]] {
                            for plugin in chain {
                                assert!(at(*plugin, false) < at(*plugin, true));
                            }
                            for stages in chain.windows(2) {
                                assert!(at(stages[0], true) < at(stages[1], false));
                            }
                        }
                        if include_dry {
                            for dry in [10, 13] {
                                assert!(
                                    at(dry, false) < at(1, true),
                                    "all instruments start first"
                                );
                                for effect in [3, 6, 12] {
                                    assert!(
                                        at(effect, true) < at(dry, true),
                                        "dry instruments wait last"
                                    );
                                }
                            }
                            assert!(
                                at(10, true) < at(13, true),
                                "dry instruments keep bus order"
                            );
                        }
                        assert!(at(8, false) < at(1, true), "native FX overlap instruments");
                        assert!(at(9, false) < at(2, true), "longer chains advance first");
                        assert!(at(11, false) < at(3, true), "terminal effects wait last");
                        assert!(at(2, true) < at(5, true), "equal depths keep bus order");
                        assert!(at(3, true) < at(6, true), "stage gaps do not add depth");
                        assert!(at(5, true) < at(9, true), "a bus advances once per round");
                        assert!(at(6, true) < at(11, true), "a bus advances once per round");
                    }
                }
                assert_eq!(deferred.missing_insert_events(), 0);
                assert!(deferred.orbit_reverbs[1].is_some());
            }
        }

        #[test]
        fn drained_insert_chains_release_capacity_without_dropping_on_the_callback() {
            struct Probe {
                key: InsertKey,
                busy: bool,
                drops: Arc<AtomicUsize>,
                cuts: Arc<AtomicUsize>,
            }
            impl OrbitInsert for Probe {
                fn key(&self) -> InsertKey {
                    self.key
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
                fn cut_notes(&mut self, _: u32) {
                    self.cuts.fetch_add(1, Ordering::Relaxed);
                }
                fn busy(&self) -> bool {
                    self.busy
                }
            }
            impl Drop for Probe {
                fn drop(&mut self) {
                    self.drops.fetch_add(1, Ordering::Relaxed);
                }
            }
            let drops = Arc::new(AtomicUsize::new(0));
            let cuts = Arc::new(AtomicUsize::new(0));
            let key = InsertKey {
                plugin: 1,
                preset: 0,
            };
            let make = |busy| {
                Box::new(Probe {
                    key,
                    busy,
                    drops: Arc::clone(&drops),
                    cuts: Arc::clone(&cuts),
                }) as Box<dyn OrbitInsert>
            };
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            backend.forbid_inline_reverb();
            for bus in 0..crate::insert::MAX_INSERT_BUSES {
                let slot = crate::insert::effect_slot(1000 + bus as u64, 0);
                let refused = if bus == 1 {
                    backend.install_insert(slot, make(true))
                } else {
                    backend.prepare_insert(slot, make(false))
                };
                assert!(refused.is_none());
            }
            let extra = crate::insert::effect_slot(9000, 0);
            let refused = backend
                .prepare_insert(extra, make(false))
                .expect("pool is full");
            assert_eq!(backend.insert_refusals(), 1);
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            let mut pending = OnsetEvent::new(2_000_000, 440.0, 0.1, 0.01);
            pending.controls.insert_chain = Some(1000);
            pending.controls.effects[0] = Some(InsertControls::new(key));
            backend.note(pending);
            backend.frame = u64::from(48_000 * INSERT_IDLE_SECONDS) + 1;
            backend
                .insert_idle_frames
                .fill(48_000 * INSERT_IDLE_SECONDS);
            backend.effect_order[1][0] = 0;
            backend.reclaim_insert_chains();
            assert_eq!(backend.insert_chains.iter().flatten().count(), 2);
            assert_eq!(backend.insert_chains[0], Some(1000));
            assert_eq!(backend.insert_chains[1], Some(1001));
            assert_eq!(
                backend.retired_inserts.len(),
                crate::insert::MAX_INSERT_BUSES - 2
            );
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            // A release gives back the copy of a chain that waits for its
            // first note, and no copy of another key. The busy copy of bus 1
            // stays.
            let other = InsertKey {
                plugin: 2,
                preset: 0,
            };
            let waiting = crate::insert::effect_slot(1000, 0);
            let before = crate::tripwire::Violations::capture();
            crate::tripwire::audio_scope(|| backend.release_insert(waiting, other, false));
            assert_eq!(
                backend.retired_inserts.len(),
                crate::insert::MAX_INSERT_BUSES - 2
            );
            crate::tripwire::audio_scope(|| {
                for chain in [1000, 1001] {
                    let slot = crate::insert::effect_slot(chain, 0);
                    backend.release_insert(slot, key, false);
                }
            });
            assert!(crate::tripwire::Violations::capture().since(before).clean());
            assert_eq!(
                backend.retired_inserts.len(),
                crate::insert::MAX_INSERT_BUSES - 1
            );
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            assert!(backend.prepared_inserts[0].is_none());
            assert!(backend.orbit_inserts[1].is_some());
            assert!(backend.prepare_insert(extra, refused).is_none());
            assert_eq!(backend.insert_chains[2], Some(9000));
            while let Some(retired) = backend.take_retired_insert() {
                drop(retired);
            }
            let drained = crate::insert::MAX_INSERT_BUSES - 1;
            assert_eq!(drops.load(Ordering::Relaxed), drained);

            // A chain the list names takes the bus of the chain with no name
            // and the oldest use. That chain fades, its voice and the notes of
            // its instrument end with the fade, and its bus is free 10 ms
            // later. Nothing allocates or frees on the callback.
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            backend.forbid_inline_reverb();
            backend.set_named_insert_chains(2, &[9000, 9002, 9004]);
            for bus in 0..crate::insert::MAX_INSERT_BUSES {
                let slot = crate::insert::effect_slot(1000 + bus as u64, 0);
                assert!(backend.prepare_insert(slot, make(false)).is_none());
            }
            let instrument = crate::insert::instrument_slot(1003);
            assert!(backend.prepare_insert(instrument, make(false)).is_none());
            let note = |chain: u64, frame: u64| {
                let mut note = OnsetEvent::new(frame, 440.0, 0.1, 1.0);
                note.controls.insert_chain = Some(chain);
                note.controls.effects[0] = Some(InsertControls::new(key));
                note
            };
            let mut sounding = note(1003, 0);
            sounding.controls.instrument = Some(InsertControls::new(key).with_note(InsertNote {
                pitch: 69.0,
                velocity: 1.0,
                frames: 48_000,
            }));
            backend.note(sounding);
            let mut output = [0.0; 256];
            backend.process_block(&mut output, 128);
            // The chain on bus 3 has the oldest use.
            backend.insert_last_used.fill(128);
            backend.insert_last_used[3] = 0;
            let fading = |backend: &ScalarBackend| -> Vec<usize> {
                let buses = 0..crate::insert::MAX_INSERT_BUSES;
                buses
                    .filter(|bus| backend.insert_fade_from[*bus] <= backend.frame)
                    .collect()
            };
            let unnamed = crate::insert::effect_slot(9001, 0);
            let _unnamed = backend
                .prepare_insert(unnamed, make(false))
                .expect("no bus is free");
            backend.reclaim_insert_chains();
            assert!(fading(&backend).is_empty(), "a chain with no name");
            let named = crate::insert::effect_slot(9000, 0);
            let refused = backend
                .prepare_insert(named, make(false))
                .expect("no bus is free");
            let before = crate::tripwire::Violations::capture();
            crate::tripwire::audio_scope(|| backend.reclaim_insert_chains());
            let fade_from = backend.frame;
            assert_eq!(fading(&backend), [3]);
            assert_eq!(backend.voices[0].cut_fade_frame, Some(fade_from));
            assert_eq!(cuts.load(Ordering::Relaxed), 1);
            // One chain fades at a time, from its first frame.
            crate::tripwire::audio_scope(|| backend.process_block(&mut output, 128));
            let refused = backend
                .prepare_insert(named, refused)
                .expect("no bus is free");
            crate::tripwire::audio_scope(|| backend.reclaim_insert_chains());
            assert_eq!(fading(&backend), [3]);
            assert_eq!(backend.insert_fade_from[3], fade_from);
            // The fade lasts 480 frames. The bus then waits for room for its 2
            // copies in the list of the copies that go back.
            crate::tripwire::audio_scope(|| {
                for _ in 0..2 {
                    backend.process_block(&mut output, 128);
                }
            });
            assert_eq!(backend.insert_chains[3], Some(1003));
            while backend.retired_inserts.len() + 1 < backend.retired_inserts.capacity() {
                backend.retired_inserts.push(make(false));
            }
            crate::tripwire::audio_scope(|| backend.process_block(&mut output, 128));
            assert_eq!(backend.insert_chains[3], Some(1003));
            let fillers = backend.retired_inserts.len();
            backend.retired_inserts.clear();
            crate::tripwire::audio_scope(|| backend.process_block(&mut output, 128));
            assert!(crate::tripwire::Violations::capture().since(before).clean());
            assert_eq!(backend.insert_chains[3], None);
            assert!(backend.voices.is_empty());
            assert_eq!(backend.retired_inserts.len(), 2);
            assert_eq!(drops.load(Ordering::Relaxed), drained + fillers);
            assert!(backend.prepare_insert(named, refused).is_none());
            assert_eq!(backend.insert_chains[3], Some(9000));

            // A bus the reclaim frees serves the chain the list names, and no
            // chain fades for it.
            backend.insert_fade_from[7] = 0;
            let refused = backend
                .prepare_insert(crate::insert::effect_slot(9004, 0), make(false))
                .expect("no bus is free");
            backend.reclaim_insert_chains();
            assert_eq!(backend.insert_chains[7], None);
            assert!(fading(&backend).is_empty(), "a free bus");
            let slot = crate::insert::effect_slot(9004, 0);
            assert!(backend.prepare_insert(slot, refused).is_none());

            // A note of a chain the list names with no bus fades a chain
            // too. A chain the list names keeps its bus, also with the
            // oldest use.
            backend.insert_last_used[3] = 0;
            backend.note(note(9003, backend.frame));
            backend.process_block(&mut output, 128);
            assert!(fading(&backend).is_empty(), "a note with no name");
            backend.note(note(9002, backend.frame));
            backend.process_block(&mut output, 128);
            assert_eq!(fading(&backend), [0]);

            // A note with no chain of its own plays on the chain of its
            // orbit. That chain keeps its bus, also with no name and the
            // oldest use, and its note plays on. A chain with no name and a
            // number of its own fades in its place.
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            backend.forbid_inline_reverb();
            let names: Vec<u64> = (1001..1064).collect();
            backend.set_named_insert_chains(2, &names);
            let orbit = crate::insert::instrument_slot(2);
            assert!(backend.prepare_insert(orbit, make(false)).is_none());
            for chain in 1000..1063 {
                let slot = crate::insert::effect_slot(chain, 0);
                assert!(backend.prepare_insert(slot, make(false)).is_none());
            }
            let mut drone = OnsetEvent::new(0, 440.0, 0.1, 1.0);
            drone.controls.orbit = 2;
            drone.controls.instrument = Some(InsertControls::new(key).with_score(1));
            backend.note(drone);
            backend.process_block(&mut output, 128);
            backend.insert_last_used.fill(128);
            backend.insert_last_used[2] = 0;
            let cut = cuts.load(Ordering::Relaxed);
            let _refused = backend
                .prepare_insert(crate::insert::effect_slot(1063, 0), make(false))
                .expect("no bus is free");
            backend.reclaim_insert_chains();
            // Bus 0 has the chain 1000.
            assert_eq!(fading(&backend), [0]);
            assert_eq!(backend.voices[0].cut_fade_frame, None);
            assert_eq!(cuts.load(Ordering::Relaxed), cut);

            // The active copy goes back when it is not busy and its stage is
            // not heard: an effect stage leaves the order, and an instrument
            // loses its level and its filters. A copy of another key stays.
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            backend.forbid_inline_reverb();
            // Chain 1000 takes bus 0, with 3 stages and an instrument. Its
            // order is [1, 0].
            let effect = |stage| crate::insert::effect_slot(1000, stage);
            let instrument = crate::insert::instrument_slot(1000);
            for slot in [effect(0), effect(1), effect(2), instrument] {
                assert!(backend.install_insert(slot, make(false)).is_none());
            }
            backend.effect_order[0][..2].copy_from_slice(&[1, 0]);
            let at = |slot: crate::insert::InsertSlot| slot.physical(0);
            let held = |backend: &ScalarBackend, slot| backend.orbit_inserts[at(slot)].is_some();
            let sounds = |backend: &mut ScalarBackend, quiet, unfed| {
                let level = &mut backend.insert_postgains[0];
                (level.quiet, level.unfed) = (quiet, unfed);
            };
            // Each release runs as on the callback: it allocates and frees
            // nothing.
            let release = |backend: &mut ScalarBackend, slot, key, stopped| {
                crate::tripwire::audio_scope(|| backend.release_insert(slot, key, stopped));
            };
            let before = crate::tripwire::Violations::capture();
            // A stage in the order is heard while the chain sounds, and while
            // a voice feeds it. A stage the chain does not run is not.
            for (quiet, unfed) in [(0, u16::MAX), (u16::MAX, 0)] {
                sounds(&mut backend, quiet, unfed);
                release(&mut backend, effect(1), key, false);
                assert!(held(&backend, effect(1)), "{quiet} {unfed}");
            }
            // A voice that goes through the chain keeps the stage heard in a
            // silence of its note, until the voice ends.
            let mut through = OnsetEvent::new(backend.frame, 440.0, 0.1, 0.01);
            through.controls.insert_chain = Some(1000);
            for (place, stage) in [1, 0].into_iter().enumerate() {
                let mut controls = InsertControls::new(key);
                controls.set_stage(stage);
                through.controls.effects[place] = Some(controls);
            }
            backend.note(through);
            backend.process_block(&mut output, 128);
            sounds(&mut backend, u16::MAX, u16::MAX);
            release(&mut backend, effect(1), key, false);
            assert!(held(&backend, effect(1)), "a voice goes through the chain");
            while !backend.voices.is_empty() {
                backend.process_block(&mut output, 128);
            }
            sounds(&mut backend, 0, 0);
            release(&mut backend, effect(2), key, false);
            assert!(!held(&backend, effect(2)));
            sounds(&mut backend, u16::MAX, u16::MAX);
            release(&mut backend, effect(1), other, false);
            assert!(held(&backend, effect(1)), "a copy of another key");
            backend.insert_mixes[at(effect(1))].active = Ramp::new(0, 0.0);
            backend.insert_idle_frames[at(effect(1))] = 64;
            release(&mut backend, effect(1), key, false);
            assert!(!held(&backend, effect(1)));
            assert_eq!(backend.effect_order[0][..2], [0, crate::insert::NO_STAGE]);
            assert_eq!(backend.insert_mixes[at(effect(1))].active.target(), 1.0);
            assert_eq!(backend.insert_idle_frames[at(effect(1))], 0);
            let lowpass = crate::FilterControls {
                lowpass: Some(crate::StaticBiquad {
                    frequency_hz: 800.0,
                    q: 1.0,
                }),
                ..Default::default()
            };
            backend.instrument_filters[0] = [FilterChain::new(lowpass, 48_000); 2];
            backend.instrument_gains[0].key = key;
            backend.instrument_gains[0].quiet = INSERT_QUIET_FRAMES - 1;
            release(&mut backend, instrument, key, false);
            assert!(held(&backend, instrument), "an instrument that sounds");
            backend.instrument_gains[0].quiet = INSERT_QUIET_FRAMES;
            release(&mut backend, instrument, key, false);
            assert!(!held(&backend, instrument));
            assert_eq!(backend.instrument_gains[0].key, InsertKey::default());
            assert!(!backend.instrument_filters[0][0].is_active());
            assert_eq!(backend.retired_inserts.len(), 3);

            // A stopped output past its ramp makes no sound: a busy copy of a
            // heard stage goes back at once, when the list of the copies that
            // go back has a place. A slot with no stage gives nothing back.
            sounds(&mut backend, 0, 0);
            backend.orbit_inserts[at(effect(0))] = Some(make(true));
            while backend.retired_inserts.len() < backend.retired_inserts.capacity() {
                backend.retired_inserts.push(make(false));
            }
            release(&mut backend, effect(0), key, true);
            assert!(held(&backend, effect(0)), "no place");
            backend.retired_inserts.clear();
            let unknown = crate::insert::InsertSlot {
                chain: 1000,
                stage: crate::insert::NO_STAGE,
            };
            release(&mut backend, unknown, key, true);
            release(&mut backend, effect(0), key, true);
            assert!(crate::tripwire::Violations::capture().since(before).clean());
            assert!(!held(&backend, effect(0)));
            assert_eq!(backend.effect_order[0][0], crate::insert::NO_STAGE);
            assert_eq!(backend.retired_inserts.len(), 1);
        }

        /// With a list of names from the host, a chain the score has keeps its
        /// insert through a long silence, also when its plugin is never silent.
        /// A chain with no name leaves after a short silence, or after the
        /// limit and its fade.
        #[test]
        fn named_chains_stay_and_the_others_leave_after_their_tail() {
            struct Still(InsertKey);
            impl OrbitInsert for Still {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
            }
            let key = InsertKey {
                plugin: 1,
                preset: 0,
            };
            let limit = 48_000 * INSERT_IDLE_SECONDS;
            let fade = (CUT_FADE_SECS * 48_000.0) as u64;
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            backend.forbid_inline_reverb();
            // Buses 0 to 3: named and silent, named and never silent, no name
            // and silent, no name and never silent.
            for chain in 1000..1004u64 {
                let slot = crate::insert::effect_slot(chain, 0);
                assert!(backend.install_insert(slot, Box::new(Still(key))).is_none());
            }
            for order in &mut backend.effect_order[..4] {
                order[0] = 0;
            }
            backend.set_named_insert_chains(2, &[1000, 1001]);
            let held = |backend: &ScalarBackend| -> Vec<u64> {
                backend.insert_chains.iter().flatten().copied().collect()
            };
            // A long time with no note. The silent inserts sleep.
            backend.frame = u64::from(limit) * 2;
            backend.insert_idle_frames.fill(limit);
            backend.insert_idle_frames[1] = 0;
            backend.insert_idle_frames[3] = 0;
            // A chain with no name leaves after 1 s of silence, not before.
            backend.insert_idle_frames[2] = 48_000 * INSERT_QUIET_SECONDS - 1;
            backend.reclaim_insert_chains();
            assert_eq!(held(&backend), [1000, 1001, 1002, 1003]);
            backend.insert_idle_frames[2] += 1;
            backend.reclaim_insert_chains();
            assert_eq!(held(&backend), [1000, 1001, 1003]);
            let fade_from = backend.frame + u64::from(limit);
            assert_eq!(
                backend.insert_fade_from[..4],
                [u64::MAX, u64::MAX, u64::MAX, fade_from]
            );
            // The chain that still sounds has the time of the limit.
            backend.frame = fade_from + fade - 1;
            backend.reclaim_insert_chains();
            assert_eq!(held(&backend), [1000, 1001, 1003]);
            backend.frame = fade_from + fade;
            backend.reclaim_insert_chains();
            assert_eq!(held(&backend), [1000, 1001]);
            assert_eq!(backend.retired_inserts.len(), 2);
            // A chain with a new insert has a moment for its name.
            for chain in [1005, 1006] {
                let late = crate::insert::effect_slot(chain, 0);
                assert!(backend.prepare_insert(late, Box::new(Still(key))).is_none());
            }
            backend.reclaim_insert_chains();
            assert_eq!(held(&backend), [1000, 1001, 1005, 1006]);
            backend.set_named_insert_chains(4, &[1000, 1001, 1006]);
            backend.frame += 48_000;
            backend.reclaim_insert_chains();
            assert_eq!(held(&backend), [1000, 1001, 1006]);
            // With each bus held, a named chain with no use for the limit gives
            // its bus to the next chain.
            backend.frame += u64::from(limit);
            for chain in 2000..2061u64 {
                let slot = crate::insert::effect_slot(chain, 0);
                assert!(backend.prepare_insert(slot, Box::new(Still(key))).is_none());
            }
            let names: Vec<u64> = held(&backend);
            assert_eq!(names.len(), crate::insert::MAX_INSERT_BUSES);
            backend.set_named_insert_chains(6, &names);
            backend.reclaim_insert_chains();
            // The chain that is never silent stays: its sound is in use.
            let mut stay = vec![1001];
            stay.extend(2000..2061u64);
            assert_eq!(held(&backend), stay);
        }

        /// A full list has each chain of the score. A chain the list does not
        /// name leaves after a short silence, as with a shorter list. The
        /// chain of an orbit that a full list leaves out stays, as with no
        /// list.
        #[test]
        fn a_full_list_of_names_takes_the_other_chains_after_a_short_silence() {
            struct Still(InsertKey);
            impl OrbitInsert for Still {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
            }
            let key = InsertKey {
                plugin: 1,
                preset: 0,
            };
            let held = |slot: crate::insert::InsertSlot, names: &[u64]| -> bool {
                let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
                backend.forbid_inline_reverb();
                assert!(backend.install_insert(slot, Box::new(Still(key))).is_none());
                backend.effect_order[0][0] = 0;
                backend.set_named_insert_chains(2, names);
                backend.frame = 48_000 * 2;
                backend.insert_idle_frames.fill(48_000 * 2);
                backend.reclaim_insert_chains();
                backend.insert_chains.contains(&Some(slot.chain))
            };
            let plugin = crate::insert::effect_slot(5000, 0);
            let others: Vec<u64> = (6000..6064).collect();
            assert_eq!(others.len(), crate::insert::MAX_INSERT_BUSES);
            assert!(!held(plugin, &others), "a full list");
            assert!(!held(plugin, &others[..63]), "a shorter list");
            let named: Vec<u64> = std::iter::once(5000).chain(6000..6063).collect();
            assert!(held(plugin, &named), "a full list with the chain");
            // A full list leaves out the chain of an orbit until its next
            // Csound note. That chain stays, as with no list.
            let orbit = crate::insert::instrument_slot(2);
            assert!(held(orbit, &others), "the chain of an orbit");
            assert!(
                !held(orbit, &others[..63]),
                "the chain of an orbit and a shorter list"
            );
        }

        /// An instrument that is never silent, in a chain the score no longer
        /// has: the sound has the time of the limit, then a short fade, and the
        /// chain leaves. A note in that time takes the chain out of its fade.
        #[test]
        fn a_chain_with_no_name_and_no_silence_fades_and_leaves() {
            struct Drone(InsertKey);
            impl OrbitInsert for Drone {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    left.fill(0.5);
                    right.fill(0.5);
                }
            }
            let key = InsertKey {
                plugin: 3,
                preset: 0,
            };
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            backend.forbid_inline_reverb();
            let slot = crate::insert::instrument_slot(3000);
            assert!(backend.install_insert(slot, Box::new(Drone(key))).is_none());
            let note = |frame: u64| {
                let mut event = OnsetEvent::new(frame, 440.0, 1.0, 0.001);
                event.controls.postgain = 1.0;
                event.controls.insert_chain = Some(3000);
                event.controls.instrument = Some(InsertControls::new(key));
                event
            };
            backend.note(note(0));
            // The score has no chain now.
            backend.set_named_insert_chains(2, &[]);
            let mut output = vec![0.0; 1024];
            let bus = backend
                .insert_chains
                .iter()
                .position(|chain| *chain == Some(3000));
            let bus = bus.expect("the chain has a bus");
            // The note ends, and the chain has no use.
            while backend.insert_fade_from[bus] == u64::MAX {
                backend.process_block(&mut output, 512);
                assert!(backend.frame < 48_000, "the chain never was free");
            }
            assert_eq!(output[1022..], [0.5; 2]);
            let limit = u64::from(48_000 * INSERT_IDLE_SECONDS);
            assert!(backend.insert_fade_from[bus] >= limit);
            // The test does not wait 30 seconds: the fade starts 256 frames
            // from now.
            backend.insert_fade_from[bus] = backend.frame + 256;
            backend.process_block(&mut output, 512);
            let left: Vec<f32> = output
                .as_chunks::<2>()
                .0
                .iter()
                .map(|frame| frame[0])
                .collect();
            assert_eq!(left[..257], [0.5; 257]);
            assert!(left[257..].windows(2).all(|pair| pair[1] < pair[0]));
            assert!((left[256 + 240] - 0.25).abs() < 1e-4, "{}", left[256 + 240]);
            // A note takes the chain out of its fade, at the full level.
            backend.note(note(backend.frame));
            backend.process_block(&mut output, 512);
            assert_eq!(output[..2], [0.5; 2]);
            assert!(backend.insert_fade_from[bus] > backend.frame);
            // With no more note, the fade runs to its end and the chain leaves.
            while backend.insert_fade_from[bus] == u64::MAX {
                backend.process_block(&mut output, 512);
            }
            backend.insert_fade_from[bus] = backend.frame;
            backend.process_block(&mut output, 512);
            assert_eq!(output[1022..], [0.0; 2]);
            assert_eq!(backend.insert_chains[bus], None);
            assert_eq!(backend.retired_inserts.len(), 1);
        }

        #[test]
        fn an_orbit_move_keeps_the_insert_state_and_old_native_delay() {
            for instrument in [false, true] {
                let key = InsertKey {
                    plugin: 7,
                    preset: 0,
                };
                let builds = Arc::new(AtomicUsize::new(0));
                let resets = Arc::new(AtomicUsize::new(0));
                let make_backend = |moved: bool| {
                    let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
                    backend.orbit_pairs[2] = u8::from(moved);
                    let builds = Arc::clone(&builds);
                    let resets = Arc::clone(&resets);
                    backend.set_insert_provider(Arc::new(move |wanted, _, _| {
                        assert_eq!(wanted, key);
                        builds.fetch_add(1, Ordering::Relaxed);
                        Some(Box::new(StatefulInsert {
                            key,
                            memory: [0.0; 2],
                            held: 0,
                            resets: Arc::clone(&resets),
                        }))
                    }));
                    for (frame, orbit) in [(0, 1), (639, 2), (1024, 1), (1536, 2)] {
                        let mut event = OnsetEvent::new(frame, 440.0, 0.25, 0.04);
                        event.controls.orbit = if moved { orbit } else { 1 };
                        event.controls.insert_chain = Some(1);
                        let mut insert = InsertControls::new(key);
                        if instrument {
                            if frame == 0 {
                                insert = insert.with_note(InsertNote {
                                    pitch: 69.0,
                                    velocity: 0.5,
                                    frames: 768,
                                });
                            }
                            event.controls.instrument = Some(insert);
                        } else {
                            event.controls.effects[0] = Some(insert);
                        }
                        if frame == 0 && !instrument {
                            event.controls.delay = Some(crate::backend::DelayControls {
                                wet: 0.3,
                                time_secs: 256.0 / 48_000.0,
                                feedback: 0.5,
                            });
                        } else if frame != 0 && !instrument {
                            event.gain = 0.0;
                        }
                        backend.note(event);
                    }
                    backend
                };
                let mut baseline = make_backend(false);
                let mut moved = make_backend(true);
                let slot = if instrument {
                    crate::insert::instrument_slot(1)
                } else {
                    crate::insert::effect_slot(1, 0)
                };
                let mut identity = None;
                let mut expected = [0.0; 256];
                let mut actual = [0.0; 256];
                for block in 0..16 {
                    baseline.process_block(&mut expected, 128);
                    moved.process_block(&mut actual, 128);
                    for (actual, routed) in actual.iter_mut().zip(moved.orbit_mix(2, 128)) {
                        *actual += routed;
                    }
                    let pointer = moved.orbit_inserts[slot.physical(1)].as_deref().unwrap()
                        as *const dyn OrbitInsert;
                    let pointer = pointer as *const ();
                    assert_eq!(*identity.get_or_insert(pointer), pointer);
                    for (expected, actual) in expected.iter().zip(actual) {
                        assert!((expected - actual).abs() < 1e-6, "block {block}");
                    }
                    assert_eq!(moved.orbit_delays[1].left, baseline.orbit_delays[1].left);
                    assert_eq!(moved.orbit_delays[1].right, baseline.orbit_delays[1].right);
                    assert!(!moved.orbit_delays[2].active);
                    if block == 3 {
                        assert_eq!(moved.insert_outputs[1], 1);
                        assert!(
                            moved.orbit_blocks[2]
                                .iter()
                                .flatten()
                                .all(|value| *value == 0.0)
                        );
                    }
                    if block == 4 {
                        assert_eq!(moved.insert_outputs[1], 2);
                        // Route selection follows block activation: this onset
                        // at 639 moves the old sound from frame 512, 127 early.
                        assert!(moved.orbit_mix(2, 128)[0].abs() > 0.01);
                        assert!(
                            moved.orbit_blocks[2]
                                .iter()
                                .flatten()
                                .any(|value| value.abs() > 0.01)
                        );
                        if !instrument {
                            assert!(
                                moved.orbit_blocks[1]
                                    .iter()
                                    .flatten()
                                    .any(|value| value.abs() > 0.001)
                            );
                            let old = moved
                                .voices
                                .iter()
                                .find(|voice| voice.start_frame == 0)
                                .unwrap();
                            assert_eq!((old.orbit, old.insert_bus), (1, Some(1)));
                        }
                    }
                    if block == 8 {
                        assert_eq!(moved.insert_outputs[1], 1);
                    }
                }
                assert_eq!(builds.load(Ordering::Relaxed), 2);
                assert_eq!(resets.load(Ordering::Relaxed), 0);
                assert_eq!(moved.missing_insert_events(), 0);
            }

            // SBD connects its graph 100 ms before its source onset. The insert
            // follows the same graph while the oscillator phase remains parked.
            let mut backend = ScalarBackend::prepared(48_000, 1).unwrap();
            let key = InsertKey {
                plugin: 7,
                preset: 0,
            };
            backend.set_insert_provider(Arc::new(move |_, _, _| {
                Some(Box::new(StatefulInsert {
                    key,
                    memory: [0.0; 2],
                    held: 0,
                    resets: Arc::new(AtomicUsize::new(0)),
                }))
            }));
            let mut event = OnsetEvent::new(6336, 43.653_53, 0.25, 1.0);
            event.synth = Some(crate::backend::SynthSource::Sbd {
                decay_secs: 0.5,
                pdecay_secs: 0.5,
                penv_semitones: 36.0,
                stop_secs: 0.51,
            });
            event.controls.orbit = 2;
            event.controls.insert_chain = Some(1);
            event.controls.effects[0] = Some(InsertControls::new(key));
            backend.note(event);
            let mut output = [0.0; 256];
            for _ in 0..12 {
                backend.process_block(&mut output, 128);
            }
            assert_eq!(backend.insert_outputs[1], 1);
            backend.process_block(&mut output, 128);
            assert_eq!(backend.insert_outputs[1], 2);
            let voice = &backend.voices[0];
            assert_eq!(voice.graph_start_frame, 1536);
            assert_eq!(voice.start_frame - voice.graph_start_frame, 4800);
            assert!(matches!(&voice.source, VoiceSource::Sbd { phase, .. } if *phase == 0.0));
        }

        #[test]
        fn native_postgain_follows_nonlinear_inserts_at_each_onset() {
            struct SquareTail {
                key: InsertKey,
                memory: [f32; 2],
            }
            impl OrbitInsert for SquareTail {
                fn key(&self) -> InsertKey {
                    self.key
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for (left, right) in left.iter_mut().zip(right) {
                        for (channel, sample) in [left, right].into_iter().enumerate() {
                            self.memory[channel] = *sample * *sample + self.memory[channel] * 0.5;
                            *sample = self.memory[channel];
                        }
                    }
                }
            }
            let changes = [
                (0, 0.5),
                (63, 0.0),
                (127, 2.0),
                (128, 0.25),
                (191, 3.0),
                (191, 0.4),
                (255, 1.5),
            ];
            for stereo in [false, true] {
                for chunks in [&[512][..], &[64, 64, 128, 256][..]] {
                    let render = |postgain: bool, gain: f32| {
                        let mut backend = ScalarBackend::prepared(48_000, changes.len()).unwrap();
                        backend.set_insert_provider(Arc::new(|key, _, _| {
                            Some(Box::new(SquareTail {
                                key,
                                memory: [0.0; 2],
                            }))
                        }));
                        for (frame, post) in changes {
                            let duration = if frame == 0 { 0.04 } else { 0.001 };
                            let mut event = OnsetEvent::new(frame, 440.0, 0.03 * gain, duration);
                            event.controls.envelope.release_secs = 0.0;
                            event.controls.postgain = if postgain { post } else { 1.0 };
                            event.controls.effects[0] = Some(InsertControls::new(InsertKey {
                                plugin: 1,
                                preset: 0,
                            }));
                            if stereo {
                                event.synth = Some(crate::backend::SynthSource::Supersaw {
                                    voices: 3.0,
                                    freqspread: 0.18,
                                    panspread: 0.6,
                                });
                            }
                            backend.note(event);
                        }
                        let mut output = vec![0.0; 1024];
                        let mut cursor = 0;
                        for &frames in chunks {
                            backend.process_block(
                                &mut output[cursor * 2..(cursor + frames) * 2],
                                frames,
                            );
                            cursor += frames;
                        }
                        assert_eq!(backend.missing_insert_events(), 0);
                        output
                    };
                    let baseline = render(false, 1.0);
                    let actual = render(true, 1.0);
                    let half_gain = render(false, 0.5);
                    assert!(baseline.iter().any(|sample| sample.abs() > 0.0001));
                    // The first note sets the level of the chain. A later note
                    // that comes while the chain sounds keeps what the level
                    // had of the level before, and loses that part in 5 ms. A
                    // note that comes in silence sets the level at once.
                    let mut value = changes[0].1;
                    let (mut applied, mut offset) = (value, Ramp::new(0, 0.0));
                    let mut silent = INSERT_QUIET_FRAMES;
                    for (frame, ((samples, baseline), half)) in actual
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .zip(baseline.as_chunks::<2>().0)
                        .zip(half_gain.as_chunks::<2>().0)
                        .enumerate()
                    {
                        let mut now = changes
                            .iter()
                            .skip(1)
                            .filter(|change| change.0 == frame as u64);
                        if let Some((_, postgain)) = now.next_back() {
                            offset = Ramp::new(0, 0.0);
                            if silent < INSERT_QUIET_FRAMES {
                                offset.declick_from(Ramp::new(0, applied - postgain), 48_000);
                            }
                            value = *postgain;
                        }
                        applied = value + offset.next();
                        silent = match baseline[0] == 0.0 && baseline[1] == 0.0 {
                            true => silent.saturating_add(1),
                            false => 0,
                        };
                        for channel in 0..2 {
                            let expected = baseline[channel] * applied;
                            assert!(
                                (samples[channel] - expected).abs() < 1e-6,
                                "stereo {stereo}, frame {frame}: {} expected {expected}",
                                samples[channel]
                            );
                            assert!((half[channel] - baseline[channel] * 0.25).abs() < 1e-6);
                        }
                    }
                }
            }
        }

        /// The level after a chain of effects is a ramp only while the chain
        /// still sounds: a note after silence has its level on its first frame.
        /// The level also moves when only the tail of an effect still sounds.
        /// So does a new stage. A new copy of a stage fades in, and the copy
        /// before fades out.
        #[test]
        fn a_chain_level_moves_on_a_tail_and_is_exact_after_silence() {
            /// Passes its input, with a tail of `decay` for each frame.
            struct Tail(InsertKey, f32, [f32; 2]);
            impl OrbitInsert for Tail {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for (left, right) in left.iter_mut().zip(right) {
                        for (channel, sample) in [left, right].into_iter().enumerate() {
                            self.2[channel] = *sample + self.2[channel] * self.1;
                            *sample = self.2[channel];
                        }
                    }
                }
            }
            /// Halves its input.
            struct Half(InsertKey);
            impl OrbitInsert for Half {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for sample in left.iter_mut().chain(right) {
                        *sample *= 0.5;
                    }
                }
            }
            let effect = InsertKey {
                plugin: 1,
                preset: 0,
            };
            let half = InsertKey {
                plugin: 2,
                preset: 0,
            };
            // Two native notes through the effect: the second note has
            // `postgain`, and starts at frame 1024. With `halves` the second
            // note has an effect that halves at that place. At place 1 it is a
            // new stage. At place 0 it is a new copy of stage 0.
            let native = |decay: f32, first_secs: f32, postgain: f32, halves: Option<usize>| {
                let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
                backend.set_insert_provider(Arc::new(move |key, _, _| {
                    let insert: Box<dyn OrbitInsert> = match key == half {
                        true => Box::new(Half(key)),
                        false => Box::new(Tail(key, decay, [0.0; 2])),
                    };
                    Some(insert)
                }));
                for (frame, seconds, post) in [(0, first_secs, 1.0), (1024, 0.01, postgain)] {
                    let mut event = OnsetEvent::new(frame, 440.0, 0.1, seconds);
                    event.controls.envelope.release_secs = 0.0;
                    event.controls.postgain = post;
                    event.controls.effects[0] = Some(InsertControls::new(effect));
                    if let Some(place) = halves.filter(|_| frame == 1024) {
                        event.controls.effects[place] = Some(InsertControls::new(half));
                    }
                    backend.note(event);
                }
                let mut output = vec![0.0; 4096];
                for chunk in output.chunks_mut(1024) {
                    backend.process_block(chunk, 512);
                }
                output
            };
            // The first note and its tail end a long time before the second
            // note: the second note has its level at once, and so does the
            // new stage.
            let (plain, quiet) = (
                native(0.5, 0.001, 1.0, None),
                native(0.5, 0.001, 0.25, None),
            );
            let halved = native(0.5, 0.001, 1.0, Some(1));
            assert!(plain[2048..3000].iter().any(|sample| sample.abs() > 0.01));
            assert_eq!(plain[1900..2048], [0.0; 148], "silence before the note");
            for (frame, (plain, quiet)) in plain.iter().zip(&quiet).enumerate().skip(2048) {
                assert!((quiet - plain * 0.25).abs() < 1e-6, "frame {}", frame / 2);
                assert_eq!(halved[frame], plain * 0.5, "frame {}", frame / 2);
            }
            // The tail of the first note still sounds: the level moves, and
            // the new stage fades in.
            let plain = native(0.9995, 0.001, 1.0, None);
            let (moved, halved) = (
                native(0.9995, 0.001, 0.25, None),
                native(0.9995, 0.001, 1.0, Some(1)),
            );
            assert!(plain[2040].abs() > 0.01, "the tail sounds");
            assert!((moved[2048] - plain[2048]).abs() < plain[2048].abs() * 0.01);
            assert!((halved[2048] - plain[2048]).abs() < plain[2048].abs() * 0.01);
            let after = 2048 + 2 * 240;
            assert!((moved[after] - plain[after] * 0.25).abs() < 1e-6);
            assert_eq!(halved[after], plain[after] * 0.5);
            // A new copy at stage 0 while the tail sounds: the copy before
            // fades out in 5 ms, and the new copy fades in. The input of the
            // stage has the rest of each frame. Then only the new copy sounds.
            // An effect with no tail passes the input of the stage.
            let (dry, swapped) = (
                native(0.0, 0.001, 1.0, None),
                native(0.9995, 0.001, 1.0, Some(0)),
            );
            let (mut fade_in, mut fade_out) = (Ramp::new(0, 1.0), Ramp::new(0, 0.0));
            fade_in.declick_from(Ramp::new(0, 0.0), 48_000);
            fade_out.declick_from(Ramp::new(0, 1.0), 48_000);
            for frame in 1024..2048 {
                let (active, level) = (fade_in.next(), fade_out.next());
                for at in [2 * frame, 2 * frame + 1] {
                    let rest = dry[at] * (1.0 - active - level);
                    let expected = dry[at] * 0.5 * active + plain[at] * level + rest;
                    assert!((swapped[at] - expected).abs() < 1e-6, "frame {frame}");
                }
            }
            assert_eq!(swapped[after], dry[after] * 0.5);

            // A synthetic bass drum has frames before its onset. They give a
            // plugin chain no value, so the chain is silent at the onset, and
            // the first frame of the note has its level.
            let drum = |postgain: f32| {
                let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
                backend.set_insert_provider(Arc::new(move |key, _, _| {
                    Some(Box::new(Tail(key, 0.0, [0.0; 2])))
                }));
                for (frame, post) in [(0, 1.0), (9600, postgain)] {
                    let mut event = OnsetEvent::new(frame, 43.653_53, 0.8, 0.02);
                    event.synth = Some(crate::backend::SynthSource::Sbd {
                        decay_secs: 0.01,
                        pdecay_secs: 0.01,
                        penv_semitones: 36.0,
                        stop_secs: 0.02,
                    });
                    event.controls.postgain = post;
                    event.controls.effects[0] = Some(InsertControls::new(effect));
                    backend.note(event);
                }
                let mut output = vec![0.0; 2 * 12_288];
                for chunk in output.chunks_mut(1024) {
                    backend.process_block(chunk, 512);
                }
                output
            };
            let (plain, quiet) = (drum(1.0), drum(0.25));
            let onset = 2 * 9600;
            assert_eq!(plain[onset - 512..onset], [0.0; 512], "before the onset");
            assert!(
                plain[onset..onset + 512]
                    .iter()
                    .any(|sample| sample.abs() > 0.01)
            );
            for (frame, (plain, quiet)) in plain.iter().zip(&quiet).enumerate().skip(onset) {
                assert!((quiet - plain * 0.25).abs() < 1e-6, "frame {}", frame / 2);
            }

            // The same with an instrument: its own sound ended, and the tail of
            // the effect after it still sounds.
            let key = InsertKey {
                plugin: 7,
                preset: 0,
            };
            let instrument = |postgain: f32| {
                let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
                backend.set_insert_provider(Arc::new(move |wanted: InsertKey, _, _| {
                    let insert: Box<dyn OrbitInsert> = match wanted == key {
                        true => Box::new(StatefulInsert {
                            key,
                            memory: [0.0; 2],
                            held: 0,
                            resets: Arc::new(AtomicUsize::new(0)),
                        }),
                        false => Box::new(Tail(wanted, 0.9995, [0.0; 2])),
                    };
                    Some(insert)
                }));
                let mut output = vec![0.0; 2048];
                for (block, post) in [(0, 1.0), (1, postgain)] {
                    let mut event = OnsetEvent::new(block * 512, 440.0, 1.0, 0.001);
                    event.controls.instrument =
                        Some(InsertControls::new(key).with_note(InsertNote {
                            pitch: 69.0,
                            velocity: 1.0,
                            frames: 4,
                        }));
                    event.controls.effects[0] = Some(InsertControls::new(effect));
                    event.controls.postgain = post;
                    backend.note(event);
                    let at = block as usize * 1024;
                    backend.process_block(&mut output[at..at + 1024], 512);
                }
                output
            };
            let (plain, moved) = (instrument(1.0), instrument(0.25));
            assert!(plain[1022].abs() > 0.01, "the tail sounds");
            assert!((moved[1024] - plain[1024]).abs() < plain[1024].abs() * 0.01);
            let after = 1024 + 2 * 240;
            assert!((moved[after] - plain[after] * 0.25).abs() < 1e-6);
        }

        /// A level change is a ramp only while the notes before still sound. A
        /// note after silence has its full level and its place on its first
        /// frame. A new instrument while the notes before sound fades the
        /// instrument before out.
        #[test]
        fn an_instrument_note_after_silence_takes_its_level_at_once() {
            let key = InsertKey {
                plugin: 7,
                preset: 0,
            };
            let make = || {
                let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
                backend.set_insert_provider(Arc::new(|key, _, _| {
                    Some(Box::new(StatefulInsert {
                        key,
                        memory: [0.0; 2],
                        held: 0,
                        resets: Arc::new(AtomicUsize::new(0)),
                    }))
                }));
                backend
            };
            let mut backend = make();
            let note = |frame: u64, gain: f32, postgain: f32, pan: Option<f32>| {
                let mut event = OnsetEvent::new(frame, 440.0, gain, 0.001);
                event.controls.instrument = Some(InsertControls::new(key).with_note(InsertNote {
                    pitch: 69.0,
                    velocity: 1.0,
                    frames: 8,
                }));
                event.controls.postgain = postgain;
                event.controls.pan = pan;
                event
            };
            let mut output = vec![0.0; 1024];
            backend.note(note(0, 1.0, 1.0, None));
            backend.process_block(&mut output, 512);
            assert_eq!(output[..2], [0.125; 2]);
            assert_eq!(output[1022..], [0.0; 2], "the first note ended");
            // A different level and a hard pan to the left, after the silence.
            backend.note(note(512, 0.5, 0.5, Some(0.0)));
            backend.process_block(&mut output, 512);
            assert_eq!(output[..2], [0.125 * 0.5 * 0.5 * 2.0, 0.0]);
            // The same change while the note sounds is no step.
            let held = || {
                let mut held = note(0, 1.0, 1.0, None);
                held.controls.instrument = Some(InsertControls::new(key).with_note(InsertNote {
                    pitch: 69.0,
                    velocity: 1.0,
                    frames: 1024,
                }));
                held
            };
            let mut backend = make();
            backend.note(held());
            backend.process_block(&mut output, 512);
            let before = output[1022];
            assert!(before > 0.24);
            let mut quiet = note(512, 0.5, 0.5, Some(0.0));
            quiet.controls.instrument = Some(InsertControls::new(key));
            backend.note(quiet);
            backend.process_block(&mut output, 512);
            assert!(
                (output[0] - before).abs() < 0.01 && output[1] > 0.2,
                "a step: {before} to {:?}",
                &output[..2]
            );
            // After 5 ms the note has its level and its place.
            assert!((output[480] - 0.25 * 0.5 * 0.5 * 2.0).abs() < 1e-6);
            assert_eq!(output[481], 0.0);
            // The same change with a new instrument with no note is no step:
            // the instrument before fades out in 5 ms, and is then silent.
            let mut backend = make();
            backend.note(held());
            backend.process_block(&mut output, 512);
            let before = output[1022];
            let mut next = note(512, 0.5, 0.5, Some(0.0));
            next.controls.instrument = Some(InsertControls::new(InsertKey { plugin: 8, ..key }));
            backend.note(next);
            backend.process_block(&mut output, 512);
            assert!(
                (output[0] - before).abs() < 0.01 && output[1] > 0.2,
                "a step: {before} to {:?}",
                &output[..2]
            );
            assert!(output[480..].iter().all(|sample| *sample == 0.0));
        }

        #[test]
        fn instrument_filters_keep_history_and_pan_precedes_insert_effects() {
            let render = |filters, changes: &[(u64, Option<f32>)], effect, chunks: &[usize]| {
                let mut backend = ScalarBackend::prepared(48_000, changes.len()).unwrap();
                backend.set_insert_provider(Arc::new(|key, _, _| {
                    Some(Box::new(StatefulInsert {
                        key,
                        memory: [0.0; 2],
                        held: 0,
                        resets: Arc::new(AtomicUsize::new(0)),
                    }))
                }));
                for &(frame, pan) in changes {
                    let mut event = OnsetEvent::new(frame, 440.0, 0.5, 0.02);
                    let mut insert = InsertControls::new(InsertKey {
                        plugin: 7,
                        preset: 0,
                    });
                    if frame == 0 {
                        insert = insert.with_note(InsertNote {
                            pitch: 69.0,
                            velocity: 1.0,
                            frames: 256,
                        });
                    }
                    event.controls.instrument = Some(insert);
                    event.controls.filters = filters;
                    event.controls.pan = pan;
                    event.controls.postgain = 0.25;
                    if effect {
                        event.controls.effects[0] = Some(InsertControls::new(InsertKey {
                            plugin: 8,
                            preset: 0,
                        }));
                    }
                    backend.note(event);
                }
                let mut output = vec![0.0; 1024];
                let mut cursor = 0;
                for &frames in chunks {
                    backend.process_block(&mut output[cursor * 2..(cursor + frames) * 2], frames);
                    cursor += frames;
                }
                assert_eq!(cursor, 512);
                assert_eq!(backend.missing_insert_events(), 0);
                output
            };
            let repeated = [(0, None), (63, None), (127, None), (128, None), (191, None)];
            let panned = [
                (0, None),
                (63, Some(0.0)),
                (127, Some(1.0)),
                (128, Some(0.0)),
                (191, Some(1.0)),
            ];
            let unfiltered = render(
                crate::FilterControls::default(),
                &[(0, None)],
                false,
                &[512],
            );
            for kind in 0..3 {
                let mut filters = crate::FilterControls::default();
                let stage = Some(crate::StaticBiquad {
                    frequency_hz: 600.0,
                    q: 0.707,
                });
                match kind {
                    0 => filters.lowpass = stage,
                    1 => filters.highpass = stage,
                    _ => filters.bandpass = stage,
                }
                let baseline = render(filters, &[(0, None)], false, &[512]);
                assert!(baseline.iter().all(|sample| sample.is_finite()));
                assert!(baseline.iter().any(|sample| sample.abs() > 0.0001));
                assert!(
                    baseline
                        .iter()
                        .zip(&unfiltered)
                        .any(|(a, b)| (a - b).abs() > 0.001)
                );
                for chunks in [&[512][..], &[64, 64, 128, 256][..]] {
                    let retained = render(filters, &repeated, false, chunks);
                    assert!(
                        retained
                            .iter()
                            .zip(&baseline)
                            .all(|(a, b)| (a - b).abs() < 1e-6)
                    );
                    for effect in [false, true] {
                        let actual = render(filters, &panned, effect, chunks);
                        let mut memory = [0.0; 2];
                        // A note with no pan has the middle. Each later note
                        // comes while the instrument sounds, and moves the pan
                        // in 5 ms.
                        let (mut place, mut pan) = (Ramp::new(0, 0.5), None);
                        for (frame, (actual, input)) in actual
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .zip(baseline.as_chunks::<2>().0)
                            .enumerate()
                        {
                            let now = panned
                                .iter()
                                .skip(1)
                                .filter(|change| change.0 == frame as u64);
                            for (_, to) in now {
                                let mut next = Ramp::new(0, to.unwrap_or(0.5));
                                next.declick_from(place, 48_000);
                                (place, pan) = (next, *to);
                            }
                            let moves = place.is_moving();
                            let at = place.next();
                            let expected = if pan.is_some() || moves {
                                let (left, right) = stereo_pan(input[0], input[1], at * 2.0 - 1.0);
                                [left, right]
                            } else {
                                *input
                            };
                            for channel in 0..2 {
                                memory[channel] = expected[channel] + memory[channel] * 0.5;
                                let expected = if effect {
                                    memory[channel]
                                } else {
                                    expected[channel]
                                };
                                assert!(
                                    (actual[channel] - expected).abs() < 1e-6,
                                    "filter {kind}, effect {effect}, frame {frame}, channel {channel}"
                                );
                            }
                        }
                    }
                }
            }
        }

        #[test]
        fn instrument_live_filters_change_sustained_notes_and_keep_ramps_at_onsets() {
            let render = |control: usize, update: bool, repeated: bool| {
                let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
                backend.set_insert_provider(Arc::new(|key, _, _| {
                    Some(Box::new(StatefulInsert {
                        key,
                        memory: [0.0; 2],
                        held: 0,
                        resets: Arc::new(AtomicUsize::new(0)),
                    }))
                }));
                let mut event = OnsetEvent::new(0, 440.0, 0.5, 0.1);
                let insert = InsertControls::new(InsertKey {
                    plugin: 7,
                    preset: 0,
                });
                event.controls.instrument = Some(insert.with_note(InsertNote {
                    pitch: 69.0,
                    velocity: 1.0,
                    frames: 256,
                }));
                event.controls.filters.lowpass = Some(crate::StaticBiquad {
                    frequency_hz: 200.0,
                    q: 0.707,
                });
                event.controls.live_controls[control] = 77;
                backend.note(event.clone());
                let mut output = vec![0.0; 2048];
                backend.process_block(&mut output[..256], 128);
                if update {
                    backend.set_live_control(crate::live_control::LiveControlUpdate {
                        binding: 77,
                        value: if control == 1 { 800.0 } else { 4.0 },
                        smooth: false,
                    });
                }
                if repeated {
                    event.onset_frame = 192;
                    event.controls.instrument = Some(insert);
                    backend.note(event);
                }
                backend.process_block(&mut output[256..512], 128);
                backend.process_block(&mut output[512..], 768);
                assert_eq!(backend.missing_insert_events(), 0);
                assert!(output.iter().all(|sample| sample.is_finite()));
                output
            };
            for control in [1, 2] {
                let unchanged = render(control, false, false);
                let changed = render(control, true, false);
                assert_eq!(unchanged[..256], changed[..256]);
                assert!(
                    unchanged[256..]
                        .iter()
                        .zip(&changed[256..])
                        .any(|(a, b)| (a - b).abs() > 0.001),
                    "live filter control {control} must change the sounding instrument"
                );
                let repeated = render(control, true, true);
                assert!(
                    repeated
                        .iter()
                        .zip(&changed)
                        .all(|(a, b)| (a - b).abs() < 1e-6),
                    "live filter control {control} must keep its ramp through an onset"
                );
            }
        }

        #[test]
        fn instrument_native_sends_survive_dry_zero_before_insert_effects() {
            for room in [false, true] {
                let render = |gain, postgain, effect, chunks: &[usize]| {
                    let key = InsertKey {
                        plugin: 7,
                        preset: 0,
                    };
                    let mut backend = ScalarBackend::prepared(48_000, 5).unwrap();
                    backend.set_insert_provider(Arc::new(|key, _, _| {
                        Some(Box::new(StatefulInsert {
                            key,
                            memory: [0.0; 2],
                            held: 0,
                            resets: Arc::new(AtomicUsize::new(0)),
                        }))
                    }));
                    for (frame, wet) in [(0, 0.0), (63, 1.0), (127, 0.25), (128, 0.5), (191, 0.0)] {
                        let mut event = OnsetEvent::new(frame, 440.0, gain, 0.02);
                        let mut insert = InsertControls::new(key);
                        if frame == 0 {
                            insert = insert.with_note(InsertNote {
                                pitch: 69.0,
                                velocity: 1.0,
                                frames: 256,
                            });
                        }
                        event.controls.instrument = Some(insert);
                        event.controls.postgain = postgain;
                        event.controls.dry = Some(0.0);
                        if room {
                            event.controls.reverb = Some(crate::backend::ReverbControls {
                                wet,
                                size_secs: 0.04,
                                fade_secs: 0.01,
                                lp_start_hz: 8_000.0,
                                lp_end_hz: 1_000.0,
                                ir: None,
                            });
                        } else {
                            event.controls.delay = Some(crate::backend::DelayControls {
                                wet,
                                time_secs: 128.0 / 48_000.0,
                                feedback: 0.25,
                            });
                        }
                        if effect {
                            event.controls.effects[0] = Some(InsertControls::new(InsertKey {
                                plugin: 8,
                                preset: 0,
                            }));
                        }
                        backend.note(event);
                    }
                    let mut output = vec![0.0; 2048];
                    let mut cursor = 0;
                    for &frames in chunks {
                        backend
                            .process_block(&mut output[cursor * 2..(cursor + frames) * 2], frames);
                        cursor += frames;
                    }
                    assert_eq!(cursor, 1024);
                    assert_eq!(backend.missing_insert_events(), 0);
                    output
                };
                let expected = render(1.0, 1.0, false, &[1024]);
                assert!(expected.iter().any(|sample| sample.abs() > 0.0001));
                let prefix_peak = expected[..63 * 2]
                    .iter()
                    .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
                assert!(
                    prefix_peak <= if room { 1e-8 } else { 0.0 },
                    "room {room}, initial peak {prefix_peak}"
                );
                // A partial first block adds one FFT bridge hop to the room return.
                let chunks = if room {
                    &[128, 128, 768][..]
                } else {
                    &[64, 64, 128, 768][..]
                };
                for (gain, postgain, effect) in
                    [(1.0, 1.0, true), (0.5, 0.25, false), (0.5, 0.25, true)]
                {
                    let actual = render(gain, postgain, effect, chunks);
                    for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                        assert!(
                            (actual - expected * gain * postgain).abs() < 1e-6,
                            "room {room}, effect {effect}, frame {}: {actual} expected {}",
                            index / 2,
                            expected * gain * postgain
                        );
                    }
                }
            }
        }

        #[test]
        fn instrument_gain_precedes_effects_and_postgain_scales_their_tail() {
            let key = InsertKey {
                plugin: 7,
                preset: 0,
            };
            let effect_key = InsertKey {
                plugin: 8,
                preset: 0,
            };
            let changes = [
                (0, 0.5, 0.5),
                (63, 0.0, 1.0),
                (127, 2.0, 0.5),
                (128, 0.25, 2.0),
                (191, 3.0, 0.5),
                (191, 0.4, 1.0),
                (255, 1.0, 1.0),
                (257, 1.0, 0.0),
                (260, 1.0, 2.0),
            ];
            for effect in [false, true] {
                for chunks in [&[512][..], &[64, 64, 128, 256][..]] {
                    let mut backend = ScalarBackend::prepared(48_000, changes.len()).unwrap();
                    backend.set_insert_provider(Arc::new(move |key, _, _| {
                        Some(Box::new(StatefulInsert {
                            key,
                            memory: [0.0; 2],
                            held: 0,
                            resets: Arc::new(AtomicUsize::new(0)),
                        }))
                    }));
                    for (frame, gain, postgain) in changes {
                        let mut event = OnsetEvent::new(frame, 440.0, gain, 0.02);
                        let mut insert = InsertControls::new(key);
                        if frame == 0 {
                            insert = insert.with_note(InsertNote {
                                pitch: 69.0,
                                velocity: 0.01,
                                frames: 256,
                            });
                        }
                        event.controls.instrument = Some(insert);
                        event.controls.postgain = postgain;
                        if effect {
                            event.controls.effects[0] = Some(InsertControls::new(effect_key));
                        }
                        backend.note(event);
                    }
                    let mut actual = vec![0.0; 1024];
                    let mut cursor = 0;
                    for &frames in chunks {
                        backend
                            .process_block(&mut actual[cursor * 2..(cursor + frames) * 2], frames);
                        cursor += frames;
                    }
                    // The first note sets the two levels. Each later note comes
                    // while the instrument sounds, and moves them in 5 ms.
                    let mut level = Ramp::new(0, changes[0].1);
                    let mut post = Ramp::new(0, changes[0].2);
                    let mut source = 0.0;
                    let mut tail = 0.0;
                    let mut before = 0.0f32;
                    for (frame, samples) in actual.as_chunks::<2>().0.iter().enumerate() {
                        source = if frame < 256 { 0.125 } else { 0.0 } + source * 0.5;
                        let now = changes
                            .iter()
                            .skip(1)
                            .filter(|change| change.0 == frame as u64);
                        for (_, gain, postgain) in now {
                            let (mut next_level, mut next_post) =
                                (Ramp::new(0, *gain), Ramp::new(0, *postgain));
                            next_level.declick_from(level, 48_000);
                            next_post.declick_from(post, 48_000);
                            (level, post) = (next_level, next_post);
                        }
                        let (gain, postgain) = (level.next(), post.next());
                        tail = source * gain + tail * 0.5;
                        let expected = if effect { tail } else { source * gain } * postgain;
                        // No note makes a step: the source is steady after its
                        // start, and a level moves 1 part in 240 a frame.
                        assert!(
                            !(16..256).contains(&frame) || (samples[0] - before).abs() < 0.02,
                            "effect {effect}, frame {frame}: a step from {before} to {}",
                            samples[0]
                        );
                        before = samples[0];
                        for sample in samples {
                            assert!(
                                (sample - expected).abs() < 1e-6,
                                "effect {effect}, frame {frame}: {sample} expected {expected}"
                            );
                        }
                    }
                    assert_eq!(backend.missing_insert_events(), 0);
                }
            }

            let mut backend = ScalarBackend::prepared(48_000, 1).unwrap();
            backend.set_insert_provider(Arc::new(move |key, _, _| {
                Some(Box::new(StatefulInsert {
                    key,
                    memory: [0.0; 2],
                    held: 0,
                    resets: Arc::new(AtomicUsize::new(0)),
                }))
            }));
            let mut event = OnsetEvent::new(0, 440.0, 1.0, 1.0);
            event.controls.live_controls[0] = 77;
            event.controls.instrument = Some(InsertControls::new(key).with_note(InsertNote {
                pitch: 69.0,
                velocity: 1.0,
                frames: 48_000,
            }));
            backend.note(event.clone());
            let mut output = [0.0; 512];
            backend.process_block(&mut output, 256);
            assert_eq!(output[510], 0.25);
            assert_eq!(backend.instrument_gains[1].key, key);
            backend.set_live_control(crate::live_control::LiveControlUpdate {
                binding: 77,
                value: 0.0,
                smooth: false,
            });
            // A retrigger keeps the shared instrument's 240-frame live fade.
            backend.process_block(&mut output[..256], 128);
            event.onset_frame = 384;
            backend.note(event);
            backend.process_block(&mut output[256..], 128);
            assert!(output[0] > 0.24 && output[0] < 0.25);
            assert!(
                output[..480]
                    .windows(3)
                    .step_by(2)
                    .all(|pair| pair[0] > pair[2])
            );
            assert!(output[478..].iter().all(|sample| *sample == 0.0));
            backend.reset_at(0);
            // The instrument stays, and its levels are 0 until its next note.
            assert_eq!(backend.instrument_gains[1].key, key);
            assert_eq!(backend.instrument_gains[1].gain.next(), 0.0);
            assert_eq!(backend.instrument_gains[1].postgain.next(), 0.0);
            assert!(backend.instrument_gain_changes.is_empty());
        }

        /// A stop resets each insert, and the output stays open for the next
        /// start. Each chain is silent until its next note. When an insert keeps
        /// a tail through its reset, the next note moves the level up from 0.
        #[test]
        fn a_reset_silences_a_kept_tail_until_the_next_note() {
            /// Rings at 0.5 after a note or any input, also after a reset.
            struct Ringing {
                key: InsertKey,
                tail: f32,
            }
            impl OrbitInsert for Ringing {
                fn key(&self) -> InsertKey {
                    self.key
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn note(&mut self, _: InsertNote, _: u32) {
                    self.tail = 0.5;
                }
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for (left, right) in left.iter_mut().zip(right) {
                        if *left != 0.0 || *right != 0.0 {
                            self.tail = 0.5;
                        }
                        (*left, *right) = (self.tail, self.tail);
                    }
                }
            }
            let key = InsertKey {
                plugin: 3,
                preset: 0,
            };
            let effect_key = InsertKey {
                plugin: 4,
                preset: 0,
            };
            for (instrument, effect) in [(false, true), (true, false), (true, true)] {
                let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
                backend.set_insert_provider(Arc::new(|key, _, _| {
                    Some(Box::new(Ringing { key, tail: 0.0 }))
                }));
                let note = |frame| {
                    let mut event = OnsetEvent::new(frame, 110.0, 1.0, 0.001);
                    if instrument {
                        event.controls.instrument =
                            Some(InsertControls::new(key).with_note(InsertNote {
                                pitch: 69.0,
                                velocity: 1.0,
                                frames: 48,
                            }));
                    }
                    if effect {
                        event.controls.effects[0] = Some(InsertControls::new(effect_key));
                    }
                    event
                };
                backend.note(note(0));
                let mut output = vec![0.0; 1024];
                backend.process_block(&mut output, 512);
                assert!(
                    output[1000] > 0.4,
                    "instrument {instrument}, effect {effect}: the insert rings"
                );
                // The stop ramp ends here: the voices go, and each insert gets
                // its reset.
                backend.reset_at(512);
                backend.process_block(&mut output, 512);
                assert!(
                    output.iter().all(|sample| *sample == 0.0),
                    "instrument {instrument}, effect {effect}: the kept tail sounds after the stop"
                );
                // The note is 128 frames into the block.
                backend.note(note(1024 + 128));
                backend.process_block(&mut output, 512);
                let left: Vec<f32> = output.iter().step_by(2).copied().collect();
                assert!(
                    left[..128].iter().all(|sample| *sample == 0.0),
                    "instrument {instrument}, effect {effect}: the kept tail sounds before the note"
                );
                assert!(
                    left[128].abs() < 0.01,
                    "instrument {instrument}, effect {effect}: {}",
                    left[128]
                );
                assert!(
                    left[128 + 300] > 0.4,
                    "instrument {instrument}, effect {effect}: {}",
                    left[428]
                );
                assert!(
                    left.windows(2).all(|pair| (pair[1] - pair[0]).abs() < 0.1),
                    "instrument {instrument}, effect {effect}: a step at the next note"
                );
                // A note at the first frame after the stop also moves the level up
                // from 0.
                backend.reset_at(1536);
                backend.note(note(1536));
                backend.process_block(&mut output, 512);
                let left: Vec<f32> = output.iter().step_by(2).copied().collect();
                assert!(
                    left[0].abs() < 0.01,
                    "instrument {instrument}, effect {effect}: {}",
                    left[0]
                );
                assert!(
                    left[300] > 0.4,
                    "instrument {instrument}, effect {effect}: {}",
                    left[300]
                );
                assert!(
                    left.windows(2).all(|pair| (pair[1] - pair[0]).abs() < 0.1),
                    "instrument {instrument}, effect {effect}: a step at the first frame"
                );
                assert_eq!(backend.missing_insert_events(), 0);
            }
        }

        /// An insert that renders ahead hears each note at admission, a
        /// schedule lead before the frame it sounds, and the note names the
        /// frame of its onset. The box activation will serve gets it: the
        /// held box when it is the wanted one, else the prepared box. An
        /// effect hears its note at its stage. An insert that renders in the
        /// callback hears nothing here.
        #[test]
        fn an_insert_that_renders_ahead_gets_its_note_at_admission() {
            type Heard = Arc<std::sync::Mutex<Vec<(u64, u64)>>>;
            struct Recorder {
                key: InsertKey,
                ahead: bool,
                frame: u64,
                heard: Heard,
            }
            impl OrbitInsert for Recorder {
                fn key(&self) -> InsertKey {
                    self.key
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn score(&mut self, id: u64, frames: u32) {
                    self.heard
                        .lock()
                        .unwrap()
                        .push((id, self.frame + u64::from(frames)));
                }
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for (left, right) in left.iter_mut().zip(right) {
                        (*left, *right) = (1.0, 1.0);
                        self.frame += 1;
                    }
                }
                fn renders_ahead(&self) -> bool {
                    self.ahead
                }
            }
            let make = |key, ahead| {
                // Capacity ahead, so a score heard inside the audio scope
                // below allocates nothing the scope would report.
                let heard = Heard::default();
                heard.lock().unwrap().reserve(4);
                (
                    Box::new(Recorder {
                        key,
                        ahead,
                        frame: 0,
                        heard: Arc::clone(&heard),
                    }) as Box<dyn OrbitInsert>,
                    heard,
                )
            };
            let held_key = InsertKey {
                plugin: u32::MAX,
                preset: 1,
            };
            let next_key = InsertKey {
                plugin: u32::MAX,
                preset: 2,
            };
            let plain_key = InsertKey {
                plugin: 5,
                preset: 0,
            };
            let effect_key = InsertKey {
                plugin: 6,
                preset: 0,
            };
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            let (held, held_heard) = make(held_key, true);
            let (next, next_heard) = make(next_key, true);
            let (plain, plain_heard) = make(plain_key, false);
            let (effect, effect_heard) = make(effect_key, true);
            assert!(
                backend
                    .install_insert(crate::insert::instrument_slot(1), held)
                    .is_none()
            );
            assert!(
                backend
                    .prepare_insert(crate::insert::instrument_slot(1), next)
                    .is_none()
            );
            assert!(
                backend
                    .install_insert(crate::insert::instrument_slot(2), plain)
                    .is_none()
            );
            assert!(
                backend
                    .install_insert(crate::insert::effect_slot(1, 2), effect)
                    .is_none()
            );
            let note = |chain: u64, key: InsertKey, onset: u64| {
                let mut event: OnsetEvent = OnsetEvent::new(onset, 440.0, 1.0, 0.02);
                event.controls.insert_chain = Some(chain);
                event.controls.instrument = Some(InsertControls::new(key).with_score(9));
                event
            };
            // The held box of the chain plays the note of its own key, at
            // the lead the admission gave the onset, and so does the effect
            // at stage 2. The prepared box plays the note of the next key.
            // An insert that renders in the callback hears nothing here. The
            // whole walk stays inside the audio scope's rules, as it runs on
            // the callback thread.
            let mut held_note = note(1, held_key, 640);
            let mut effect = InsertControls::new(effect_key).with_score(9);
            effect.set_stage(2);
            held_note.controls.effects[0] = Some(effect);
            let next_note = note(1, next_key, 896);
            let plain_note = note(2, plain_key, 384);
            let before = crate::tripwire::Violations::capture();
            crate::tripwire::audio_scope(|| {
                backend.notify_inserts_early(&held_note, 128);
                backend.notify_inserts_early(&next_note, 128);
                backend.notify_inserts_early(&plain_note, 128);
            });
            assert!(crate::tripwire::Violations::capture().since(before).clean());
            assert_eq!(*held_heard.lock().unwrap(), [(9, 512)]);
            assert_eq!(*effect_heard.lock().unwrap(), [(9, 512)]);
            assert_eq!(*next_heard.lock().unwrap(), [(9, 768)]);
            assert!(plain_heard.lock().unwrap().is_empty());
        }

        /// Gives `backend` a pool of `workers` with its gate open.
        fn test_pool(backend: &mut ScalarBackend, workers: usize) -> crate::pool::PluginPool {
            let pool = crate::pool::PluginPool::start_test_pool(workers, true).expect("a pool");
            backend.set_plugin_pool(Arc::clone(pool.shared()));
            pool
        }

        /// The calls on a plugin worker, by the plugin number.
        type Away = Arc<[AtomicUsize; 16]>;

        /// A plugin of the split chains with its answer to `any_thread`. It
        /// renders a block at the submit and gives it at the collect. Two
        /// calls at once fail, and so do a second submit and a collect with
        /// no block. A submit on a worker counts in `away`.
        struct Plugin {
            key: InsertKey,
            any_thread: bool,
            block: [[f32; crate::reverb::REVERB_BLOCK]; 2],
            pending: usize,
            in_call: AtomicBool,
            away: Away,
        }

        impl OrbitInsert for Plugin {
            fn key(&self) -> InsertKey {
                self.key
            }
            fn set_param(&mut self, _: InsertParam, _: u32) {}
            fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                split_render(self.key.plugin, left, right);
            }
            fn submit(&mut self, left: &mut [f32], right: &mut [f32]) {
                assert!(
                    !self.in_call.swap(true, Ordering::SeqCst),
                    "two calls overlap"
                );
                if crate::pool::on_plugin_worker() {
                    self.away[self.key.plugin as usize].fetch_add(1, Ordering::SeqCst);
                }
                assert_eq!(self.pending, 0, "a block waits");
                self.pending = left.len();
                let [block_left, block_right] = &mut self.block;
                let (block_left, block_right) = (
                    &mut block_left[..left.len()],
                    &mut block_right[..right.len()],
                );
                block_left.copy_from_slice(left);
                block_right.copy_from_slice(right);
                split_render(self.key.plugin, block_left, block_right);
                self.in_call.store(false, Ordering::SeqCst);
            }
            fn collect(&mut self, left: &mut [f32], right: &mut [f32]) {
                assert!(
                    !self.in_call.swap(true, Ordering::SeqCst),
                    "two calls overlap"
                );
                assert_eq!(
                    std::mem::take(&mut self.pending),
                    left.len(),
                    "no block waits"
                );
                left.copy_from_slice(&self.block[0][..left.len()]);
                right.copy_from_slice(&self.block[1][..right.len()]);
                self.in_call.store(false, Ordering::SeqCst);
            }
            fn busy(&self) -> bool {
                assert!(!self.in_call.load(Ordering::SeqCst), "a call in a call");
                false
            }
            fn any_thread(&self) -> bool {
                self.any_thread
            }
        }

        /// Builds a [`Plugin`] for each key, with the answer `any_thread`
        /// gives for its plugin number.
        fn plugins(away: &Away, any_thread: fn(u32) -> bool) -> Arc<crate::insert::InsertProvider> {
            let away = Arc::clone(away);
            Arc::new(move |key, _, _| {
                Some(Box::new(Plugin {
                    key,
                    any_thread: any_thread(key.plugin),
                    block: [[0.0; crate::reverb::REVERB_BLOCK]; 2],
                    pending: 0,
                    in_call: AtomicBool::new(false),
                    away: Arc::clone(&away),
                }))
            })
        }

        fn away_calls(away: &Away) -> usize {
            away.iter().map(|calls| calls.load(Ordering::SeqCst)).sum()
        }

        /// Renders `frames` on both backends, and compares their samples and
        /// the idle frames of their inserts. Gives the samples.
        fn render_both(
            serial: &mut ScalarBackend,
            pooled: &mut ScalarBackend,
            frames: usize,
        ) -> Vec<f32> {
            let mut expected = vec![0.0; frames * 2];
            let mut actual = vec![0.0; frames * 2];
            serial.process_block(&mut expected, frames);
            pooled.process_block(&mut actual, frames);
            assert_eq!(actual, expected, "{frames} frames");
            assert_eq!(
                pooled.insert_idle_frames, serial.insert_idle_frames,
                "{frames} frames"
            );
            actual
        }

        fn figures(backend: &ScalarBackend) -> crate::pool::PoolFigures {
            backend.plugin_pool.as_ref().expect("a pool").figures()
        }

        /// The pool keeps the samples of the serial engine: each job ends in
        /// its own sub-block, a job runs its stages in the order of the chain,
        /// also stage 1 before stage 0, and the chains join the orbit in bus
        /// order.
        #[test]
        fn pooled_chains_render_the_samples_of_the_serial_engine() {
            let away = Away::default();
            for include_dry in [false, true] {
                let mut serial = split_chains(include_dry, plugins(&away, |_| true));
                let mut pooled = split_chains(include_dry, plugins(&away, |_| true));
                let pool = test_pool(&mut pooled, 3);
                // Each effect job runs on a worker.
                for bus in 0..crate::insert::MAX_INSERT_BUSES {
                    pool.shared()
                        .claim_before_join(crate::pool::JobKind::Effect, bus);
                }
                for frames in [1, 31, 32, 127, 129, 256, 480] {
                    let samples = render_both(&mut serial, &mut pooled, frames);
                    assert!(samples.iter().any(|sample| *sample > 0.0));
                }
                // The native chain plays its first stage only. Then it goes
                // silent and counts its idle frames.
                for backend in [&mut serial, &mut pooled] {
                    let mut event = OnsetEvent::new(1056, 440.0, 0.5, 0.02);
                    event.controls.orbit = 1;
                    event.controls.insert_chain = Some(103);
                    let key = InsertKey {
                        plugin: 8,
                        preset: 0,
                    };
                    event.controls.effects[0] = Some(InsertControls::new(key));
                    backend.note(event);
                }
                for _ in 0..8 {
                    render_both(&mut serial, &mut pooled, 480);
                }
                assert_eq!(pooled.missing_insert_events(), 0);
            }
            assert!(away_calls(&away) > 0, "no job ran on a worker");
        }

        /// A swap in a pooled chain fades as in the serial engine, bit for
        /// bit. The job of the instrument renders the instrument that leaves,
        /// and the job of the effects mixes the stages in their fades: stage 1
        /// of the stages [1, 0] swaps, and stage 0 leaves. A copy that leaves
        /// keeps the chain on the callback while it fades, when it answers
        /// false to `any_thread`. A later stage that sleeps runs in the job
        /// while its level moves, so its fade ends and its copy goes back.
        #[test]
        fn a_pooled_swap_fades_as_the_serial_engine() {
            for audio_thread in [false, true] {
                let away = Away::default();
                let any_thread: fn(u32) -> bool = match audio_thread {
                    true => |plugin| plugin > 2,
                    false => |_| true,
                };
                let mut serial = split_chains(false, plugins(&away, any_thread));
                let mut pooled = split_chains(false, plugins(&away, any_thread));
                let pool = test_pool(&mut pooled, 3);
                // Each job runs on a worker.
                for kind in [
                    crate::pool::JobKind::Instrument,
                    crate::pool::JobKind::Effect,
                ] {
                    for bus in 0..crate::insert::MAX_INSERT_BUSES {
                        pool.shared().claim_before_join(kind, bus);
                    }
                }
                render_both(&mut serial, &mut pooled, 256);
                // A new instrument, plugin 13, and a new effect at stage 1,
                // plugin 15, on the chain of the instrument 1. Stage 0 leaves.
                let controls = |plugin| InsertControls::new(InsertKey { plugin, preset: 0 });
                for backend in [&mut serial, &mut pooled] {
                    let mut event = OnsetEvent::new(320, 440.0, 0.5, 0.02);
                    event.controls.orbit = 1;
                    event.controls.insert_chain = Some(101);
                    event.controls.instrument = Some(controls(13).with_note(InsertNote {
                        pitch: 69.0,
                        velocity: 1.0,
                        frames: 512,
                    }));
                    let mut effect = controls(15);
                    effect.set_stage(1);
                    event.controls.effects[0] = Some(effect);
                    backend.note(event);
                }
                render_both(&mut serial, &mut pooled, 64);
                let bus = pooled
                    .insert_chains
                    .iter()
                    .position(|chain| *chain == Some(101));
                let bus = bus.expect("the bus of the chain");
                let slots = [
                    crate::insert::effect_slot(101, 1).physical(bus),
                    crate::insert::instrument_slot(101).physical(bus),
                ];
                let leaving = |backend: &ScalarBackend| -> Vec<Option<u32>> {
                    let inserts = slots
                        .iter()
                        .map(|slot| backend.leaving_inserts[*slot].as_ref());
                    inserts
                        .map(|insert| insert.map(|insert| insert.key().plugin))
                        .collect()
                };
                // The copies of plugins 2 and 1 leave their slots and fade for
                // 240 frames, on a worker when they run on any thread.
                let before = [1, 2].map(|plugin| away[plugin].load(Ordering::SeqCst));
                render_both(&mut serial, &mut pooled, 128);
                for backend in [&serial, &pooled] {
                    assert_eq!(leaving(backend), [Some(2), Some(1)]);
                }
                // With no place in the list of the copies that go back, each
                // copy waits after its fade, and stage 0 waits at the level 0
                // for one more segment.
                for backend in [&mut serial, &mut pooled] {
                    let provider = plugins(&away, any_thread);
                    let slot = crate::insert::effect_slot(101, 0);
                    while backend.retired_inserts.len() < backend.retired_inserts.capacity() {
                        let insert = provider(controls(0).key, 48_000, slot).expect("a copy");
                        backend.retired_inserts.push(insert);
                    }
                }
                render_both(&mut serial, &mut pooled, 128);
                let after = [1, 2].map(|plugin| away[plugin].load(Ordering::SeqCst));
                for (before, after) in before.iter().zip(&after) {
                    assert_eq!(after > before, !audio_thread, "audio thread {audio_thread}");
                }
                for backend in [&serial, &pooled] {
                    assert_eq!(leaving(backend), [Some(2), Some(1)]);
                    assert_eq!(backend.effect_order[bus][..2], [1, 0]);
                }
                // A copy that waits after its fade keeps no chain on the
                // callback.
                let before = [13, 15].map(|plugin| away[plugin].load(Ordering::SeqCst));
                render_both(&mut serial, &mut pooled, 128);
                for backend in [&serial, &pooled] {
                    assert_eq!(leaving(backend), [None, None]);
                    assert_eq!(backend.effect_order[bus][..2], [1, crate::insert::NO_STAGE]);
                }
                let after = [13, 15].map(|plugin| away[plugin].load(Ordering::SeqCst));
                assert!(
                    before
                        .iter()
                        .zip(&after)
                        .all(|(before, after)| after > before)
                );
                render_both(&mut serial, &mut pooled, 480);
                assert_eq!(pooled.missing_insert_events(), 0);
                // A later stage that sleeps runs in the job while its level
                // moves. Chain 104 goes from the stages [0, 1] to [0] with no
                // input, and only stage 1 sounds: plugin 14 squares its silent
                // input, and plugin 7 gives its level.
                let note = |backend: &mut ScalarBackend, plugins: &[u32]| {
                    let mut event = OnsetEvent::new(backend.frame, 440.0, 0.0, 0.02);
                    event.controls.orbit = 1;
                    event.controls.insert_chain = Some(104);
                    for (place, plugin) in plugins.iter().enumerate() {
                        event.controls.effects[place] = Some(controls(*plugin));
                    }
                    backend.note(event);
                };
                for backend in [&mut serial, &mut pooled] {
                    note(backend, &[14, 7]);
                }
                render_both(&mut serial, &mut pooled, 128);
                let bus = pooled
                    .insert_chains
                    .iter()
                    .position(|chain| *chain == Some(104));
                let bus = bus.expect("the bus of the chain");
                // Stage 1 sleeps: it has no input, and its idle frames are at
                // the limit.
                let slot = crate::insert::effect_slot(104, 1).physical(bus);
                for backend in [&mut serial, &mut pooled] {
                    backend.insert_idle_frames[slot] = 48_000 * INSERT_IDLE_SECONDS;
                    note(backend, &[14]);
                }
                let before = away[7].load(Ordering::SeqCst);
                for _ in 0..2 {
                    render_both(&mut serial, &mut pooled, 128);
                }
                assert!(
                    away[7].load(Ordering::SeqCst) > before,
                    "audio thread {audio_thread}"
                );
                for backend in [&serial, &pooled] {
                    assert_eq!(backend.effect_order[bus][..2], [0, crate::insert::NO_STAGE]);
                    assert_eq!(
                        backend.orbit_insert_key(crate::insert::effect_slot(104, 1)),
                        None
                    );
                    assert_eq!(backend.insert_fading, 0);
                }
            }
        }

        /// A job no worker claims runs on the callback at its join, and the
        /// callback waits for no worker. With no ready worker, and in a
        /// sub-block under 32 frames, the callback posts no job.
        #[test]
        fn a_job_no_worker_claims_runs_on_the_callback() {
            let away = Away::default();
            let mut serial = split_chains(true, plugins(&away, |_| true));
            let mut pooled = split_chains(true, plugins(&away, |_| true));
            let pool = test_pool(&mut pooled, 2);
            pool.shared().hold_claims(true);
            for frames in [31, 128, 256, 480] {
                render_both(&mut serial, &mut pooled, frames);
            }
            // 7 sub-blocks of 32 frames or more, each with 4 instrument jobs
            // and 3 effect jobs.
            assert_eq!(figures(&pooled).inline_jobs, 49);
            assert_eq!(figures(&pooled).join_wait_nanos, 0);
            assert_eq!(away_calls(&away), 0);
            drop(pool);
            render_both(&mut serial, &mut pooled, 256);
            assert_eq!(figures(&pooled).inline_jobs, 49);
        }

        /// While a worker runs a slow job, the callback does the rest of the
        /// sub-block: the other jobs, its own instrument, and the effect
        /// chains it finishes before the instruments with no effect.
        #[test]
        fn the_callback_runs_open_jobs_while_a_slow_worker_runs() {
            /// An insert at a level. Plugin 1 spins 2 ms on each block, and on
            /// a worker it waits until the callback gave `waits_for` other
            /// blocks of the sub-block. Plugin 10 stays on the callback and
            /// gives its block at the collect.
            struct Level {
                key: InsertKey,
                blocks: usize,
                waits_for: usize,
                others: Arc<AtomicUsize>,
            }
            impl OrbitInsert for Level {
                fn key(&self) -> InsertKey {
                    self.key
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    self.blocks += 1;
                    match self.key.plugin {
                        1 => {
                            let start = std::time::Instant::now();
                            let target = self.waits_for * self.blocks;
                            while start.elapsed() < std::time::Duration::from_millis(2)
                                || (crate::pool::on_plugin_worker()
                                    && self.others.load(Ordering::SeqCst) < target)
                            {
                                assert!(
                                    start.elapsed().as_secs() < 10,
                                    "the callback gave no other block"
                                );
                                std::hint::spin_loop();
                            }
                        }
                        10 => {}
                        _ => {
                            self.others.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    split_render(self.key.plugin, left, right);
                }
                fn collect(&mut self, _: &mut [f32], _: &mut [f32]) {
                    if self.key.plugin == 10 {
                        self.others.fetch_add(1, Ordering::SeqCst);
                    }
                }
                fn any_thread(&self) -> bool {
                    self.key.plugin != 10
                }
            }
            // With no effect: 4 instruments, one on each of the buses 0 to
            // 3, and plugin 1 waits for the other 3. With an effect: plugin 1,
            // and a native chain through plugin 10, which the callback
            // finishes first.
            for effect in [false, true] {
                let make = || {
                    let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
                    let others = Arc::new(AtomicUsize::new(0));
                    let waits_for = if effect { 1 } else { 3 };
                    backend.set_insert_provider(Arc::new(move |key, _, _| {
                        Some(Box::new(Level {
                            key,
                            blocks: 0,
                            waits_for,
                            others: Arc::clone(&others),
                        }))
                    }));
                    for chain in 0..if effect { 2 } else { 4 } {
                        let mut event = OnsetEvent::new(0, 440.0, 0.5, 0.02);
                        event.controls.insert_chain = Some(chain);
                        let plugin = 1 + 3 * chain as u32;
                        let controls = InsertControls::new(InsertKey {
                            plugin: if effect && chain == 1 { 10 } else { plugin },
                            preset: 0,
                        });
                        if effect && chain == 1 {
                            event.controls.effects[0] = Some(controls);
                        } else {
                            event.controls.instrument = Some(controls);
                        }
                        backend.note(event);
                    }
                    backend
                };
                let mut serial = make();
                let mut pooled = make();
                let pool = test_pool(&mut pooled, 1);
                // Only the worker takes the slow job.
                pool.shared()
                    .claim_before_join(crate::pool::JobKind::Instrument, 0);
                for _ in 0..8 {
                    render_both(&mut serial, &mut pooled, 128);
                }
                let figures = figures(&pooled);
                assert_eq!(figures.inline_jobs, if effect { 0 } else { 2 * 8 });
                assert!(figures.worker_nanos >= 8 * 2_000_000);
            }
        }

        /// A chain with an effect that answers false, and an instrument that
        /// renders ahead, post no job. A chain beside them still does.
        #[test]
        fn a_bus_with_a_box_that_answers_false_stays_on_the_callback() {
            /// An instrument that renders ahead, as a native instrument
            /// does. It keeps the answer of `any_thread` an insert has.
            struct Ahead(InsertKey);
            impl OrbitInsert for Ahead {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    assert!(
                        !crate::pool::on_plugin_worker(),
                        "an insert that renders ahead ran on a worker"
                    );
                    split_render(self.0.plugin, left, right);
                }
                fn renders_ahead(&self) -> bool {
                    true
                }
            }
            let away = Away::default();
            let make = |away: &Away| {
                let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
                // Plugin 3 answers false, and plugin 4 renders ahead.
                let provider = plugins(away, |plugin| plugin != 3);
                backend.set_insert_provider(Arc::new(move |key, rate, slot| {
                    if key.plugin == 4 {
                        Some(Box::new(Ahead(key)))
                    } else {
                        provider(key, rate, slot)
                    }
                }));
                for chain in 0..3 {
                    let mut event = OnsetEvent::new(0, 440.0, 0.5, 0.02);
                    event.controls.insert_chain = Some(chain);
                    let controls = |plugin| InsertControls::new(InsertKey { plugin, preset: 0 });
                    match chain {
                        // A native chain: plugin 2, then plugin 3.
                        0 => {
                            event.controls.effects[0] = Some(controls(2));
                            event.controls.effects[1] = Some(controls(3));
                        }
                        1 => event.controls.instrument = Some(controls(4)),
                        _ => event.controls.instrument = Some(controls(7)),
                    }
                    backend.note(event);
                }
                backend
            };
            let mut serial = make(&away);
            let mut pooled = make(&away);
            let pool = test_pool(&mut pooled, 2);
            // The job of plugin 7 runs on a worker.
            pool.shared()
                .claim_before_join(crate::pool::JobKind::Instrument, 2);
            for frames in [128, 256, 480] {
                render_both(&mut serial, &mut pooled, frames);
            }
            for plugin in [2, 3] {
                assert_eq!(away[plugin].load(Ordering::SeqCst), 0, "plugin {plugin}");
            }
            assert_eq!(away[7].load(Ordering::SeqCst), 7);
            assert_eq!(figures(&pooled).inline_jobs, 0);
        }

        /// A newer note sets the copies and the order of its chain, and an
        /// older note changes neither. While the chain is heard, a stage that
        /// leaves fades out in its place, a new stage fades in, and a copy
        /// that leaves its slot fades out in the leaving place. Else each
        /// change acts at once. A stop ends each fade.
        #[test]
        fn a_newer_note_sets_the_stages_and_a_heard_change_fades() {
            /// Passes its input, and adds 0.25 after a note.
            struct Pass(InsertKey, f32);
            impl OrbitInsert for Pass {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn note(&mut self, _: InsertNote, _: u32) {
                    self.1 = 0.25;
                }
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for sample in left.iter_mut().chain(right) {
                        *sample += self.1;
                    }
                }
            }
            let key = |plugin| InsertKey { plugin, preset: 0 };
            let copy = |plugin| Box::new(Pass(key(plugin), 0.0)) as Box<dyn OrbitInsert>;
            let effect = |stage| crate::insert::effect_slot(1000, stage);
            // Chain 1000 takes bus 0.
            let slot = |stage: usize| stage * crate::insert::MAX_INSERT_BUSES;
            let chain = |copies: &[(u32, usize)]| {
                let mut backend = ScalarBackend::prepared(48_000, 8).unwrap();
                backend.forbid_inline_reverb();
                for (plugin, stage) in copies {
                    assert!(
                        backend
                            .install_insert(effect(*stage), copy(*plugin))
                            .is_none()
                    );
                }
                backend
            };
            // A note of chain 1000 through each plugin at its stage.
            let note = |frame: u64, generation: u64, gain: f32, effects: &[(u32, u8)]| {
                let mut event =
                    OnsetEvent::new(frame, 440.0, gain, 0.01).with_generation(generation);
                event.controls.insert_chain = Some(1000);
                for (place, (plugin, stage)) in effects.iter().enumerate() {
                    let mut controls = InsertControls::new(key(*plugin));
                    controls.set_stage(*stage);
                    event.controls.effects[place] = Some(controls);
                }
                event
            };
            // The note on chain 1000, or on the chain of orbit 0, which has
            // the number 0 and takes bus 0 too.
            let number = |own: bool| if own { 1000 } else { 0 };
            let on = |own: bool, mut event: OnsetEvent| {
                event.controls.orbit = 0;
                event.controls.insert_chain = own.then_some(1000);
                event
            };
            let block = |backend: &mut ScalarBackend| {
                let mut output = [0.0; 256];
                backend.process_block(&mut output, 128);
                output
            };
            let order = |stages: &[u8]| {
                let mut order = [crate::insert::NO_STAGE; crate::insert::EFFECT_CHAIN];
                order[..stages.len()].copy_from_slice(stages);
                order
            };
            let leaving = |backend: &ScalarBackend, slot: usize| {
                let insert = backend.leaving_inserts[slot].as_ref();
                insert.map(|insert| insert.key().plugin)
            };
            let retired = |backend: &ScalarBackend| -> Vec<u32> {
                let inserts = backend.retired_inserts.iter();
                inserts.map(|insert| insert.key().plugin).collect()
            };
            let fill = |backend: &mut ScalarBackend| {
                while backend.retired_inserts.len() < backend.retired_inserts.capacity() {
                    backend.retired_inserts.push(copy(0));
                }
            };

            // An older note swaps no copy back, does not set the order back,
            // and goes only through the stages the chain runs.
            let mut backend = chain(&[(1, 1), (2, 0), (3, 2)]);
            backend.note(note(0, 2, 0.5, &[(1, 1), (2, 0)]));
            block(&mut backend);
            assert!(backend.prepare_insert(effect(0), copy(4)).is_none());
            for effects in [[(4, 0)], [(2, 0)], [(3, 2)]] {
                backend.note(note(128, 1, 0.5, &effects));
            }
            block(&mut backend);
            assert_eq!(backend.orbit_insert_key(effect(0)), Some(key(2)));
            assert!(backend.prepared_inserts[slot(0)].is_some());
            assert_eq!(backend.effect_order[0], order(&[1, 0]));
            assert_eq!(
                (backend.insert_generations[0], backend.insert_fading),
                (2, 0)
            );
            // The notes of plugins 4 and 3 play dry.
            assert_eq!(backend.missing_insert_events(), 2);

            // [X, A, B] to [A, B] while the chain sounds: X fades out in its
            // place, then leaves the order. On a chain of its own its copy
            // goes back, when the list of the copies that go back has a place.
            // On the chain of an orbit the copy sleeps in its slot.
            for own in [true, false] {
                let mut backend = chain(&[]);
                for (plugin, stage) in [(5, 2), (6, 0), (7, 1)] {
                    let slot = crate::insert::effect_slot(number(own), stage);
                    assert!(backend.install_insert(slot, copy(plugin)).is_none());
                }
                backend.note(on(own, note(0, 1, 0.5, &[(5, 2), (6, 0), (7, 1)])));
                block(&mut backend);
                backend.note(on(own, note(128, 1, 0.5, &[(6, 0), (7, 1)])));
                block(&mut backend);
                assert_eq!(backend.effect_order[0], order(&[2, 0, 1]));
                let fade = backend.insert_mixes[slot(2)].active;
                assert!(fade.is_moving() && fade.target() == 0.0);
                fill(&mut backend);
                block(&mut backend);
                let waits: &[u8] = if own { &[2, 0, 1] } else { &[0, 1] };
                assert_eq!(backend.effect_order[0], order(waits), "own {own}");
                backend.retired_inserts.clear();
                block(&mut backend);
                assert_eq!(backend.effect_order[0], order(&[0, 1]));
                let kept = crate::insert::effect_slot(number(own), 2);
                let kept = backend.orbit_insert_key(kept);
                assert_eq!(kept, (!own).then_some(key(5)), "own {own}");
                let back = if own { vec![5] } else { vec![] };
                assert_eq!(retired(&backend), back, "own {own}");
                assert_eq!(backend.insert_fading, 0);
            }

            // [A, B] to [C, A] while the chain sounds: C fades in first, and B
            // fades out after A, where it was.
            let mut backend = chain(&[(45, 0), (46, 1)]);
            backend.note(note(0, 1, 0.5, &[(45, 0), (46, 1)]));
            block(&mut backend);
            assert!(backend.prepare_insert(effect(2), copy(47)).is_none());
            backend.note(note(128, 1, 0.5, &[(47, 2), (45, 0)]));
            block(&mut backend);
            assert_eq!(backend.effect_order[0], order(&[2, 0, 1]));
            let fade = backend.insert_mixes[slot(1)].active;
            assert!(fade.is_moving() && fade.target() == 0.0);

            // [A, B] to [B, A] has no fade. A new stage fades in, and the old
            // copy of its slot goes back at once: the chain did not run it.
            let mut backend = chain(&[(8, 0), (9, 1), (10, 2)]);
            backend.note(note(0, 1, 0.5, &[(8, 0), (9, 1)]));
            block(&mut backend);
            backend.note(note(128, 1, 0.5, &[(9, 1), (8, 0)]));
            block(&mut backend);
            assert_eq!(backend.effect_order[0], order(&[1, 0]));
            assert_eq!(backend.insert_fading, 0);
            assert!(backend.prepare_insert(effect(2), copy(11)).is_none());
            backend.note(note(256, 1, 0.5, &[(9, 1), (8, 0), (11, 2)]));
            block(&mut backend);
            assert_eq!(backend.effect_order[0], order(&[1, 0, 2]));
            let fade = backend.insert_mixes[slot(2)].active;
            assert!(fade.is_moving() && fade.target() == 1.0);
            assert_eq!(retired(&backend), [10]);
            assert_eq!(leaving(&backend, slot(2)), None);

            // A silent chain: a stage that leaves goes now, and so does a
            // copy that leaves its slot. Both copies go back.
            let mut backend = chain(&[(12, 1), (13, 0)]);
            backend.note(note(0, 1, 0.0, &[(12, 1), (13, 0)]));
            block(&mut backend);
            assert!(backend.prepare_insert(effect(0), copy(14)).is_none());
            backend.note(note(128, 1, 0.0, &[(14, 0)]));
            block(&mut backend);
            assert_eq!(backend.effect_order[0], order(&[0]));
            assert_eq!(backend.orbit_insert_key(effect(0)), Some(key(14)));
            assert_eq!(retired(&backend), [13, 12]);
            assert!(backend.leaving_inserts.iter().all(Option::is_none));
            assert_eq!(backend.insert_fading, 0);

            // A new copy fades in while the chain sounds, and the copy it
            // displaces fades out in the leaving place. A stop ends each
            // fade: that copy goes back, and the stage that fades out leaves
            // the order. The callback allocates and frees nothing.
            let mut backend = chain(&[(15, 0), (16, 1)]);
            backend.note(note(0, 1, 0.5, &[(15, 0), (16, 1)]));
            block(&mut backend);
            assert!(backend.prepare_insert(effect(0), copy(17)).is_none());
            backend.note(note(128, 2, 0.5, &[(17, 0)]));
            let before = crate::tripwire::Violations::capture();
            crate::tripwire::audio_scope(|| block(&mut backend));
            assert_eq!(leaving(&backend, slot(0)), Some(15));
            assert_eq!(backend.effect_order[0], order(&[0, 1]));
            crate::tripwire::audio_scope(|| backend.reset_at(256));
            assert_eq!(leaving(&backend, slot(0)), None);
            assert_eq!(backend.effect_order[0], order(&[0]));
            assert_eq!(retired(&backend), [15, 16]);
            assert_eq!(
                (backend.insert_generations[0], backend.insert_fading),
                (0, 0)
            );
            // After the start an older generation sets the copies, and the
            // leaving place is free.
            backend.note(note(256, 1, 0.5, &[(17, 0)]));
            crate::tripwire::audio_scope(|| block(&mut backend));
            assert!(backend.prepare_insert(effect(0), copy(18)).is_none());
            backend.note(note(384, 1, 0.5, &[(18, 0)]));
            crate::tripwire::audio_scope(|| block(&mut backend));
            assert_eq!(leaving(&backend, slot(0)), Some(17));
            assert_eq!(retired(&backend), [15, 16]);
            // At the end of its fade the copy waits for a place in the list
            // of the copies that go back.
            fill(&mut backend);
            crate::tripwire::audio_scope(|| block(&mut backend));
            assert_eq!(leaving(&backend, slot(0)), Some(17));
            assert_ne!(backend.insert_fading, 0);
            backend.retired_inserts.clear();
            crate::tripwire::audio_scope(|| block(&mut backend));
            assert!(crate::tripwire::Violations::capture().since(before).clean());
            assert_eq!(retired(&backend), [17]);
            assert_eq!(backend.insert_fading, 0);

            // Each slot keeps an active, a prepared and a leaving copy. A swap
            // in the fade of a swap gives back the copy in the leaving place,
            // and the copy it displaces fades out in that place. With no place
            // in the list of the copies that go back, a copy that goes back
            // stays, and the note plays dry. A heard copy takes a free leaving
            // place all the same. The callback allocates and frees nothing.
            for full in [false, true] {
                let mut backend = chain(&[(19, 0)]);
                assert_eq!(
                    backend.retired_inserts.capacity(),
                    3 * crate::insert::INSERT_SLOTS
                );
                backend.note(note(0, 1, 0.5, &[(19, 0)]));
                block(&mut backend);
                if full {
                    fill(&mut backend);
                }
                let before = crate::tripwire::Violations::capture();
                for (frame, plugin) in [(128, 20), (256, 21)] {
                    assert!(backend.prepare_insert(effect(0), copy(plugin)).is_none());
                    backend.note(note(frame, 1, 0.5, &[(plugin, 0)]));
                    crate::tripwire::audio_scope(|| block(&mut backend));
                }
                assert!(crate::tripwire::Violations::capture().since(before).clean());
                let (active, left) = if full { (20, 19) } else { (21, 20) };
                assert_eq!(backend.orbit_insert_key(effect(0)), Some(key(active)));
                assert_eq!(leaving(&backend, slot(0)), Some(left), "full {full}");
                assert_eq!(backend.missing_insert_events(), u64::from(full));
                if !full {
                    assert_eq!(retired(&backend), [19]);
                    assert!(backend.insert_mixes[slot(0)].leaving.is_moving());
                }
            }
            let mut backend = chain(&[(22, 0)]);
            backend.note(note(0, 1, 0.0, &[(22, 0)]));
            block(&mut backend);
            fill(&mut backend);
            assert!(backend.prepare_insert(effect(0), copy(23)).is_none());
            backend.note(note(128, 1, 0.0, &[(23, 0)]));
            block(&mut backend);
            assert_eq!(backend.orbit_insert_key(effect(0)), Some(key(22)));
            assert_eq!(backend.missing_insert_events(), 1);

            // The chain is heard while its output sounds, and while a voice
            // feeds it.
            for (quiet, unfed) in [(u16::MAX, 0), (0, u16::MAX)] {
                let mut backend = chain(&[(36, 0)]);
                backend.note(note(0, 1, 0.5, &[(36, 0)]));
                block(&mut backend);
                assert!(backend.prepare_insert(effect(0), copy(37)).is_none());
                let level = &mut backend.insert_postgains[0];
                (level.quiet, level.unfed) = (quiet, unfed);
                backend.note(note(128, 1, 0.5, &[(37, 0)]));
                block(&mut backend);
                assert_eq!(leaving(&backend, slot(0)), Some(36), "{quiet} {unfed}");
            }

            // Once the chain is silent, a newer note ends the fade of the
            // copy in a leaving place. A stage that leaves takes that copy
            // with it, and a stage that stays has its level at once.
            for stays in [false, true] {
                let mut backend = chain(&[(24, 0), (25, 1)]);
                let mut ends = note(0, 1, 0.5, &[(24, 0), (25, 1)]);
                ends.duration_secs = 128.0 / 48_000.0;
                ends.controls.envelope.release_secs = 0.0;
                backend.note(ends);
                block(&mut backend);
                assert!(backend.prepare_insert(effect(1), copy(26)).is_none());
                backend.note(note(128, 1, 0.0, &[(24, 0), (26, 1)]));
                backend.process_block(&mut [0.0; 128], 64);
                assert_eq!(leaving(&backend, slot(1)), Some(25));
                let last: &[(u32, u8)] = if stays {
                    &[(24, 0), (26, 1)]
                } else {
                    &[(24, 0)]
                };
                backend.note(note(192, 1, 0.0, last));
                backend.process_block(&mut [0.0; 128], 64);
                assert_eq!(leaving(&backend, slot(1)), None, "stays {stays}");
                assert!(!backend.insert_mixes[slot(1)].moving(), "stays {stays}");
                let back: &[u32] = if stays { &[25] } else { &[25, 26] };
                assert_eq!(retired(&backend), back, "stays {stays}");
            }

            // A chain that leaves in a fade gives back its leaving copy too,
            // and its levels and its generation reset. Its 2 copies need 2
            // places.
            let mut backend = chain(&[]);
            for (frame, generation, plugin) in [(0, 1, 27), (128, 2, 28)] {
                assert!(backend.prepare_insert(effect(0), copy(plugin)).is_none());
                let mut event = note(frame, generation, 0.5, &[(plugin, 0)]);
                event.duration_secs = 0.002;
                event.controls.envelope.release_secs = 0.0;
                backend.note(event);
                block(&mut backend);
            }
            assert!(backend.voices.is_empty());
            assert_eq!(leaving(&backend, slot(0)), Some(27));
            backend.frame += u64::from(48_000 * INSERT_IDLE_SECONDS);
            backend
                .insert_idle_frames
                .fill(48_000 * INSERT_IDLE_SECONDS);
            fill(&mut backend);
            backend
                .retired_inserts
                .truncate(backend.retired_inserts.capacity() - 1);
            backend.reclaim_insert_chains();
            assert_eq!(backend.insert_chains[0], Some(1000));
            backend.retired_inserts.pop();
            backend.reclaim_insert_chains();
            assert_eq!(backend.insert_chains[0], None);
            assert_eq!(leaving(&backend, slot(0)), None);
            assert!(!backend.insert_mixes[slot(0)].moving());
            assert_eq!(
                (backend.insert_generations[0], backend.insert_fading),
                (0, 0)
            );

            // A new instrument starts at its level, and the instrument it
            // displaces fades out in the leaving place.
            let mut backend = chain(&[]);
            let instrument = crate::insert::instrument_slot(1000);
            assert!(backend.install_insert(instrument, copy(29)).is_none());
            assert!(backend.prepare_insert(instrument, copy(30)).is_none());
            for (frame, plugin) in [(0, 29), (128, 30)] {
                let mut event = note(frame, 1, 0.5, &[]);
                let controls = InsertControls::new(key(plugin)).with_note(InsertNote {
                    pitch: 69.0,
                    velocity: 1.0,
                    frames: 4800,
                });
                event.controls.instrument = Some(controls);
                backend.note(event);
                block(&mut backend);
            }
            let mix = backend.insert_mixes[instrument.physical(0)];
            assert_eq!(leaving(&backend, instrument.physical(0)), Some(29));
            assert!(!mix.active.is_moving() && mix.leaving.is_moving());
            block(&mut backend);
            assert_eq!(retired(&backend), [29]);

            // A chain with no stage is not heard, also when the stage that
            // left still sounded at its end.
            let mut backend = chain(&[(38, 0)]);
            assert!(backend.install_insert(instrument, copy(39)).is_none());
            let mut ringing = note(0, 1, 0.0, &[]);
            // The effect gets a note, so it sounds with no input.
            let controls = InsertControls::new(key(38)).with_note(InsertNote {
                pitch: 69.0,
                velocity: 1.0,
                frames: 1,
            });
            ringing.controls.effects[0] = Some(controls);
            let mut empty = note(128, 1, 0.0, &[]);
            empty.controls.instrument = Some(InsertControls::new(key(39)));
            for event in [ringing, empty] {
                backend.note(event);
                block(&mut backend);
            }
            block(&mut backend);
            assert_eq!(backend.effect_order[0], order(&[]));
            assert!(backend.install_insert(effect(1), copy(40)).is_none());
            backend.note(note(384, 1, 0.0, &[(40, 1)]));
            block(&mut backend);
            assert_eq!(backend.effect_order[0], order(&[1]));
            assert!(!backend.insert_mixes[slot(1)].moving());

            // An offline render builds the copy of a newer note, and the copy
            // it displaces fades out in the leaving place. An older note builds
            // nothing.
            let mut backend = ScalarBackend::prepared(48_000, 8).unwrap();
            backend.set_insert_provider(Arc::new(|key, _, _| {
                Some(Box::new(Pass(key, 0.0)) as Box<dyn OrbitInsert>)
            }));
            for (frame, plugin) in [(0, 31), (128, 32)] {
                backend.note(note(frame, 2, 0.5, &[(plugin, 0)]));
                block(&mut backend);
            }
            assert_eq!(leaving(&backend, slot(0)), Some(31));
            backend.note(note(256, 1, 0.5, &[(33, 0)]));
            block(&mut backend);
            assert_eq!(backend.orbit_insert_key(effect(0)), Some(key(32)));
            assert_eq!(backend.missing_insert_events(), 1);

            // An instrument note with no effect empties the order of a chain
            // of its own: the stage fades out, and the instrument goes
            // through it while it fades. The voices that still feed the chain
            // then pass dry. The chain of an orbit keeps its order.
            let voice = || {
                let mut voice = note(0, 1, 0.5, &[(34, 0)]);
                voice.duration_secs = 0.05;
                voice
            };
            let mut alone = chain(&[]);
            let mut dry = voice();
            dry.controls.effects = [None; crate::insert::EFFECT_CHAIN];
            alone.note(on(true, dry));
            let dry = (0..4).map(|_| block(&mut alone)).last();
            for own in [true, false] {
                let mut backend = chain(&[]);
                let slots = [
                    crate::insert::effect_slot(number(own), 0),
                    crate::insert::instrument_slot(number(own)),
                ];
                for (slot, plugin) in slots.into_iter().zip([34, 35]) {
                    assert!(backend.install_insert(slot, copy(plugin)).is_none());
                }
                let mut quiet = note(128, 1, 0.5, &[]);
                quiet.controls.instrument = Some(InsertControls::new(key(35)));
                for event in [voice(), quiet] {
                    backend.note(on(own, event));
                    block(&mut backend);
                }
                assert_eq!(backend.effect_order[0], order(&[0]));
                assert_eq!(backend.instrument_to_effect[0], own);
                block(&mut backend);
                let output = block(&mut backend);
                let stages: &[u8] = if own { &[] } else { &[0] };
                assert_eq!(backend.effect_order[0], order(stages), "own {own}");
                assert_eq!(Some(output), dry, "own {own}");
            }

            // A stage whose level moves runs, also a stage that sleeps: [A, B]
            // to [B] while only B sounds, with no input.
            let sounding = |plugin| {
                InsertControls::new(key(plugin)).with_note(InsertNote {
                    pitch: 69.0,
                    velocity: 1.0,
                    frames: 1,
                })
            };
            let mut backend = chain(&[(41, 0), (42, 1)]);
            let mut first = note(0, 1, 0.0, &[(41, 0)]);
            first.controls.effects[1] = Some(sounding(42));
            backend.note(first);
            block(&mut backend);
            backend.insert_idle_frames[slot(0)] = 48_000 * INSERT_IDLE_SECONDS;
            backend.note(note(128, 1, 0.0, &[(42, 1)]));
            for _ in 0..2 {
                block(&mut backend);
            }
            assert_eq!(backend.effect_order[0], order(&[1]));
            assert_eq!(backend.insert_fading, 0);

            // A stage at the level 0 does not run while its copy waits for a
            // place in the list of the copies that go back. Its input passes,
            // as after the copy went back.
            let outputs = [false, true].map(|full| {
                let mut backend = chain(&[(43, 0), (44, 1)]);
                let mut first = note(0, 1, 0.5, &[(43, 0), (44, 1)]);
                first.controls.effects[0] = Some(sounding(43));
                backend.note(first);
                block(&mut backend);
                backend.note(note(128, 1, 0.5, &[(44, 1)]));
                block(&mut backend);
                if full {
                    fill(&mut backend);
                }
                block(&mut backend);
                let stages: &[u8] = if full { &[0, 1] } else { &[1] };
                assert_eq!(backend.effect_order[0], order(stages), "full {full}");
                block(&mut backend)
            });
            assert_eq!(outputs[0], outputs[1]);
        }

        /// A moved value goes at once to the copy of its slot that has its
        /// key, and a note of its generation or older takes it: at
        /// activation, and at admission for an insert that renders ahead. A
        /// newer note keeps its own value. A reset forgets each move. A full
        /// table drops the value of the oldest move. A slot with no stage
        /// moves no copy. A backend that is not prepared keeps no move.
        #[test]
        fn a_moved_value_reaches_its_copy_now_and_each_older_note() {
            type Heard = Arc<std::sync::Mutex<Vec<(u32, f32, u32)>>>;
            /// Notes each value it gets: the parameter, the value and the
            /// frames from now.
            struct Values(InsertKey, bool, Heard);
            impl OrbitInsert for Values {
                fn key(&self) -> InsertKey {
                    self.0
                }
                fn set_param(&mut self, param: InsertParam, frames: u32) {
                    self.2.lock().unwrap().push((param.id, param.value, frames));
                }
                fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
                fn renders_ahead(&self) -> bool {
                    self.1
                }
            }
            let key = |plugin| InsertKey { plugin, preset: 0 };
            let slot = |stage| crate::insert::effect_slot(1000, stage);
            // Plugin 1 at stage 0 renders in the callback, and plugin 2 at
            // stage 1 renders ahead.
            let (played, ahead) = (Heard::default(), Heard::default());
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            for (stage, heard) in [(0, &played), (1, &ahead)] {
                let copy = Values(key(stage as u32 + 1), stage == 1, Arc::clone(heard));
                assert!(
                    backend
                        .install_insert(slot(stage), Box::new(copy))
                        .is_none()
                );
            }
            let moved = |stage, plugin, value, generation| crate::insert::InsertValue {
                slot: slot(stage),
                key: key(plugin),
                param: InsertParam { id: 3, value },
                generation,
            };
            // A note through both copies, with 0.75 for parameter 3 and 0.5
            // for parameter 4.
            let note = |frame, generation| {
                let mut event: OnsetEvent =
                    OnsetEvent::new(frame, 440.0, 0.5, 0.01).with_generation(generation);
                event.controls.insert_chain = Some(1000);
                for place in 0..2 {
                    let mut controls = InsertControls::new(key(place as u32 + 1));
                    for (id, value) in [(3, 0.75), (4, 0.5)] {
                        assert!(controls.push(InsertParam { id, value }));
                    }
                    event.controls.effects[place] = Some(controls);
                }
                event
            };
            let take = |heard: &Heard| std::mem::take(&mut *heard.lock().unwrap());
            let block = |backend: &mut ScalarBackend| backend.process_block(&mut [0.0; 256], 128);

            // A copy with another key takes no value. A new move of a
            // parameter takes the place of the move before, and a value that
            // is not finite goes nowhere.
            backend.set_insert_value(moved(0, 9, 0.1, 1));
            backend.set_insert_value(moved(0, 1, 0.1, 1));
            backend.set_insert_value(moved(0, 1, 0.25, 1));
            backend.set_insert_value(moved(0, 1, f32::NAN, 1));
            backend.set_insert_value(moved(1, 2, 0.4, 1));
            assert_eq!(take(&played), [(3, 0.1, 0), (3, 0.25, 0)]);
            assert_eq!(take(&ahead), [(3, 0.4, 0)]);
            assert_eq!(backend.insert_values.len(), 3);
            // A slot with no stage moves no copy.
            let mut unknown = moved(0, 1, 0.5, 1);
            unknown.slot.stage = crate::insert::NO_STAGE;
            backend.set_insert_value(unknown);
            assert!(take(&played).is_empty() && take(&ahead).is_empty());
            // A backend that is not prepared keeps no move.
            let mut unprepared = ScalarBackend::new();
            unprepared.set_insert_value(moved(0, 1, 0.5, 1));
            assert!(unprepared.insert_values.is_empty());

            // A note of the generation of the moves takes them at admission
            // and at activation.
            let older = note(64, 1);
            backend.notify_inserts_early(&older, 0);
            assert_eq!(take(&ahead), [(3, 0.4, 64), (4, 0.5, 64)]);
            backend.note(older);
            block(&mut backend);
            assert_eq!(take(&played), [(3, 0.25, 64), (4, 0.5, 64)]);
            assert_eq!(take(&ahead), [(3, 0.4, 64), (4, 0.5, 64)]);

            // A newer note keeps its own values at admission and at
            // activation.
            let newer = note(192, 2);
            backend.notify_inserts_early(&newer, 128);
            assert_eq!(take(&ahead), [(3, 0.75, 64), (4, 0.5, 64)]);
            backend.note(newer);
            block(&mut backend);
            assert_eq!(take(&played), [(3, 0.75, 64), (4, 0.5, 64)]);

            backend.reset_at(256);
            assert!(backend.insert_values.is_empty());
            backend.note(note(320, 1));
            block(&mut backend);
            assert_eq!(take(&played), [(3, 0.75, 64), (4, 0.5, 64)]);

            // A full table drops the value of the oldest move. Parameter 3
            // moves first and again after 63 others, so the 65th value
            // drops parameter 4.
            let moved_id = |id, value| crate::insert::InsertValue {
                slot: slot(0),
                key: key(1),
                param: InsertParam { id, value },
                generation: 1,
            };
            for id in [3, 4].into_iter().chain(100..162) {
                backend.set_insert_value(moved_id(id, 0.1));
            }
            backend.set_insert_value(moved_id(3, 0.25));
            backend.set_insert_value(moved_id(162, 0.1));
            take(&played);
            backend.note(note(448, 1));
            block(&mut backend);
            assert_eq!(take(&played), [(3, 0.25, 64), (4, 0.5, 64)]);
        }
    }
}
