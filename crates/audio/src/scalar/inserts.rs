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

impl ScalarBackend {
    pub(super) fn init_insert_pool(&mut self) {
        if self.orbit_inserts.is_empty() {
            self.orbit_inserts = (0..crate::insert::INSERT_SLOTS).map(|_| None).collect();
            self.prepared_inserts = (0..crate::insert::INSERT_SLOTS).map(|_| None).collect();
            self.retired_inserts = Vec::with_capacity(crate::insert::INSERT_SLOTS * 2);
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
        let bus = slot
            .is_valid()
            .then(|| claim_insert_bus(&mut self.insert_chains, slot.chain))
            .flatten();
        let Some(bus) = bus else {
            self.insert_refusals += 1;
            return Some(insert);
        };
        self.insert_last_used[bus] = self.frame;
        self.orbit_inserts[slot.physical(bus)].replace(insert)
    }

    /// A live copy waits for a matching accepted onset before replacing one.
    pub(crate) fn prepare_insert(
        &mut self,
        slot: crate::insert::InsertSlot,
        insert: Box<dyn crate::insert::OrbitInsert>,
    ) -> Option<Box<dyn crate::insert::OrbitInsert>> {
        let bus = slot
            .is_valid()
            .then(|| claim_insert_bus(&mut self.insert_chains, slot.chain))
            .flatten();
        let Some(bus) = bus else {
            self.insert_refusals += 1;
            return Some(insert);
        };
        self.insert_last_used[bus] = self.frame;
        self.prepared_inserts[slot.physical(bus)].replace(insert)
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

    /// Preparations refused because all physical chain buses were occupied.
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
    /// A full list has the chains last in use, and says nothing of the
    /// other chains of the score: it counts as no list.
    pub(super) fn reclaim_insert_chains(&mut self) {
        let limit = self.sample_rate.saturating_mul(INSERT_IDLE_SECONDS);
        let quiet = self.sample_rate.saturating_mul(INSERT_QUIET_SECONDS);
        let fade = (CUT_FADE_SECS * self.sample_rate as f32) as u64;
        let listed = self.insert_named_list != 0 && self.insert_named.contains(&u64::MAX);
        // With no list, or with no free bus, a chain with no use for a long
        // time gives its bus back, named or not.
        let by_use = !listed || self.insert_chains.iter().all(Option::is_some);
        for bus in 0..self.insert_chains.len() {
            let Some(chain) = self.insert_chains[bus] else {
                continue;
            };
            let named = !listed || self.insert_named.contains(&chain);
            let unused = self.frame.saturating_sub(self.insert_last_used[bus]) >= u64::from(limit);
            // Only the rule by use takes a named chain, and only after the
            // limit: most turns end here.
            if named && !(by_use && unused) {
                self.insert_fade_from[bus] = u64::MAX;
                continue;
            }
            if self
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
                    })
            {
                self.insert_fade_from[bus] = u64::MAX;
                continue;
            }
            let mut copies = 0;
            // True when each insert the chain uses was silent for `frames`.
            let mut silent_for = |frames: u32, awake: bool| {
                copies = 0;
                (0..=crate::insert::EFFECT_CHAIN).all(|stage| {
                    let slot = stage * crate::insert::MAX_INSERT_BUSES + bus;
                    copies += usize::from(self.orbit_inserts[slot].is_some())
                        + usize::from(self.prepared_inserts[slot].is_some());
                    let used = stage == crate::insert::EFFECT_CHAIN
                        || self.effect_stages[bus] & (1 << stage) != 0;
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
            if !leaves || self.retired_inserts.len() + copies > self.retired_inserts.capacity() {
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
                self.insert_idle_frames[slot] = 0;
            }
            self.insert_chains[bus] = None;
            self.insert_fade_from[bus] = u64::MAX;
            self.effect_stages[bus] = 0;
            self.instrument_to_effect[bus] = false;
            self.instrument_gains[bus] = InstrumentGain::default();
            self.instrument_filters[bus] = [FilterChain::default(); 2];
            self.insert_postgains[bus] = InsertPostgain::default();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    mod insert_routing_tests {
        use super::*;
        use crate::insert::{InsertControls, InsertKey, InsertNote, InsertParam, OrbitInsert};
        use crate::live_control::Ramp;
        use std::sync::atomic::{AtomicUsize, Ordering};

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
                for (live, routed) in [(false, false), (false, true), (true, false), (true, true)] {
                    if live && held_key.is_some() {
                        continue;
                    }
                    let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
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
                    for (frame, samples) in samples.as_chunks::<2>().0.iter().enumerate() {
                        let level = if frame < first_frame as usize {
                            0.0
                        } else if frame < 91 {
                            0.125
                        } else if frame < 191 {
                            0.25
                        } else {
                            0.375
                        };
                        assert_eq!(
                            *samples, [level; 2],
                            "frame {frame}, live={live}, routed={routed}, first={first_key:?}, next={next_key:?}, held={held_key:?}"
                        );
                    }
                }
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
            }
            assert!(peak > 0.001);
            assert_eq!(builds.load(Ordering::Relaxed), 6);
            assert_eq!(combined.missing_insert_events(), 0);
            assert_eq!(combined.insert_chains.iter().flatten().count(), 2);
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
            impl SplitInsert {
                fn render(plugin: u32, left: &mut [f32], right: &mut [f32]) {
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
            }
            impl OrbitInsert for SplitInsert {
                fn key(&self) -> InsertKey {
                    self.key
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    Self::render(self.key.plugin, left, right);
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
                    Self::render(
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
                let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
                let calls = Arc::clone(&calls);
                backend.set_insert_provider(Arc::new(move |key, _, _| {
                    Some(Box::new(SplitInsert {
                        key,
                        deferred,
                        pending: 0,
                        output: [[0.0; crate::reverb::REVERB_BLOCK]; 2],
                        calls: Arc::clone(&calls),
                    }))
                }));
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
                            event.controls.effects[0] = Some(controls(2));
                            let second_stage = if chain == 2 { 3 } else { 1 };
                            event.controls.effects[second_stage] = Some(controls(3));
                        }
                        backend.note(event);
                    }
                }
                backend
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
            }
            impl OrbitInsert for Probe {
                fn key(&self) -> InsertKey {
                    self.key
                }
                fn set_param(&mut self, _: InsertParam, _: u32) {}
                fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
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
            let make = |busy| {
                Box::new(Probe {
                    key: InsertKey {
                        plugin: 1,
                        preset: 0,
                    },
                    busy,
                    drops: Arc::clone(&drops),
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
            pending.controls.effects[0] = Some(InsertControls::new(InsertKey {
                plugin: 1,
                preset: 0,
            }));
            backend.note(pending);
            backend.frame = u64::from(48_000 * INSERT_IDLE_SECONDS) + 1;
            backend
                .insert_idle_frames
                .fill(48_000 * INSERT_IDLE_SECONDS);
            backend.effect_stages[1] = 1;
            backend.reclaim_insert_chains();
            assert_eq!(backend.insert_chains.iter().flatten().count(), 2);
            assert_eq!(backend.insert_chains[0], Some(1000));
            assert_eq!(backend.insert_chains[1], Some(1001));
            assert_eq!(
                backend.retired_inserts.len(),
                crate::insert::MAX_INSERT_BUSES - 2
            );
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            assert!(backend.prepare_insert(extra, refused).is_none());
            assert_eq!(backend.insert_chains[2], Some(9000));
            while let Some(retired) = backend.take_retired_insert() {
                drop(retired);
            }
            assert_eq!(
                drops.load(Ordering::Relaxed),
                crate::insert::MAX_INSERT_BUSES - 2
            );
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
            backend.effect_stages[..4].fill(1);
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

        /// A full list has the 64 chains last in use, and names no other chain
        /// of the score. With a full list, a chain keeps its insert through a
        /// short silence, as with no list.
        #[test]
        fn a_full_list_of_names_takes_no_chain_after_a_short_silence() {
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
            let held = |names: &[u64]| -> bool {
                let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
                backend.forbid_inline_reverb();
                let slot = crate::insert::effect_slot(5000, 0);
                assert!(backend.install_insert(slot, Box::new(Still(key))).is_none());
                backend.effect_stages[0] = 1;
                backend.set_named_insert_chains(2, names);
                backend.frame = 48_000 * 2;
                backend.insert_idle_frames.fill(48_000 * 2);
                backend.reclaim_insert_chains();
                backend.insert_chains[0] == Some(5000)
            };
            let others: Vec<u64> = (6000..6064).collect();
            assert_eq!(others.len(), crate::insert::MAX_INSERT_BUSES);
            assert!(held(&others), "a full list");
            assert!(!held(&others[..63]), "a list with each chain of the score");
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
            let effect = InsertKey {
                plugin: 1,
                preset: 0,
            };
            // Two native notes through the effect: the second note has
            // `postgain`, and starts at frame 1024.
            let native = |decay: f32, first_secs: f32, postgain: f32| {
                let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
                backend.set_insert_provider(Arc::new(move |key, _, _| {
                    Some(Box::new(Tail(key, decay, [0.0; 2])))
                }));
                for (frame, seconds, post) in [(0, first_secs, 1.0), (1024, 0.01, postgain)] {
                    let mut event = OnsetEvent::new(frame, 440.0, 0.1, seconds);
                    event.controls.envelope.release_secs = 0.0;
                    event.controls.postgain = post;
                    event.controls.effects[0] = Some(InsertControls::new(effect));
                    backend.note(event);
                }
                let mut output = vec![0.0; 4096];
                for chunk in output.chunks_mut(1024) {
                    backend.process_block(chunk, 512);
                }
                output
            };
            // The first note and its tail end a long time before the second
            // note: the second note has its level at once.
            let (plain, quiet) = (native(0.5, 0.001, 1.0), native(0.5, 0.001, 0.25));
            assert!(plain[2048..3000].iter().any(|sample| sample.abs() > 0.01));
            assert_eq!(plain[1900..2048], [0.0; 148], "silence before the note");
            for (frame, (plain, quiet)) in plain.iter().zip(&quiet).enumerate().skip(2048) {
                assert!((quiet - plain * 0.25).abs() < 1e-6, "frame {}", frame / 2);
            }
            // The tail of the first note still sounds: the level moves.
            let (plain, moved) = (native(0.9995, 0.001, 1.0), native(0.9995, 0.001, 0.25));
            assert!(plain[2040].abs() > 0.01, "the tail sounds");
            assert!((moved[2048] - plain[2048]).abs() < plain[2048].abs() * 0.01);
            let after = 2048 + 2 * 240;
            assert!((moved[after] - plain[after] * 0.25).abs() < 1e-6);

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
        /// frame.
        #[test]
        fn an_instrument_note_after_silence_takes_its_level_at_once() {
            let key = InsertKey {
                plugin: 7,
                preset: 0,
            };
            let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
            backend.set_insert_provider(Arc::new(|key, _, _| {
                Some(Box::new(StatefulInsert {
                    key,
                    memory: [0.0; 2],
                    held: 0,
                    resets: Arc::new(AtomicUsize::new(0)),
                }))
            }));
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
            let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
            backend.set_insert_provider(Arc::new(|key, _, _| {
                Some(Box::new(StatefulInsert {
                    key,
                    memory: [0.0; 2],
                    held: 0,
                    resets: Arc::new(AtomicUsize::new(0)),
                }))
            }));
            let mut held = note(0, 1.0, 1.0, None);
            held.controls.instrument = Some(InsertControls::new(key).with_note(InsertNote {
                pitch: 69.0,
                velocity: 1.0,
                frames: 1024,
            }));
            backend.note(held);
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
    }
}
