use super::*;

/// Takes a lock of the producer state. The insert provider is host code and
/// runs with the insert locks. A panic in that code does not end the output:
/// the next turn reads the value as the panic left it.
fn locked<T>(lock: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Default for LiveReverbBatch {
    fn default() -> Self {
        Self {
            requested: [None; crate::scalar::MAX_ORBITS],
            publication: None,
            // A turn with no plugin makes no allocation here.
            inserts: Vec::new(),
            refused: 0,
        }
    }
}

impl LiveReverbBatch {
    pub(super) fn request_orbit(&mut self, orbit: usize, params: crate::reverb::ReverbParams) {
        self.requested[orbit.min(crate::scalar::MAX_ORBITS - 1)] = Some(params);
    }

    /// Observe one event without constructing an orbit impulse response.
    /// Per-voice `.FX()` reverbs keep their distinct bounded cache.
    pub fn observe(
        &mut self,
        device: &LiveScalarDevice,
        event: &crate::AudioEvent,
    ) -> Result<(), String> {
        let publication = device.ensure_kabel(event)?;
        if publication.is_active() && self.publication.is_none() {
            self.publication = Some(publication);
        }
        self.observe_prepared(device, event);
        Ok(())
    }

    /// Observe an event whose worklet resources were prepared with its complete
    /// scheduling window before generation publication.
    pub fn observe_prepared(&mut self, device: &LiveScalarDevice, event: &crate::AudioEvent) {
        self.observe_inserts(event);
        if let Some(reverb) = event.controls.reverb {
            self.request_orbit(
                usize::from(event.controls.orbit),
                crate::reverb::ReverbParams {
                    ir: None,
                    size_secs: reverb.size_secs,
                    fade_secs: reverb.fade_secs,
                    lp_start_hz: reverb.lp_start_hz,
                    lp_end_hz: reverb.lp_end_hz,
                },
            );
        }
        for stage in event.controls.fx_stages.iter().flatten() {
            let Some(room) = stage.room else { continue };
            device.ensure_fx_reverb(crate::reverb::ReverbParams {
                ir: room.ir,
                size_secs: room.size_secs,
                fade_secs: room.fade_secs,
                lp_start_hz: room.lp_start_hz,
                lp_end_hz: room.lp_end_hz,
            });
        }
    }

    fn request_insert(&mut self, slot: crate::insert::InsertSlot, key: crate::insert::InsertKey) {
        if let Some((_, requested)) = self.inserts.iter_mut().find(|(held, _)| *held == slot) {
            *requested = key;
        } else if self.inserts.len() < crate::insert::INSERT_SLOTS {
            self.inserts.push((slot, key));
        } else {
            self.refused += 1;
        }
    }

    fn observe_inserts(&mut self, event: &crate::AudioEvent) {
        let chain = event.controls.insert_chain();
        for (place, effect) in event.controls.effects.iter().enumerate() {
            if let Some(effect) = effect {
                let slot = crate::insert::effect_slot(chain, effect.stage(place));
                self.request_insert(slot, effect.key);
            }
        }
        if let Some(instrument) = event.controls.instrument {
            self.request_insert(crate::insert::instrument_slot(chain), instrument.key);
        }
    }

    /// Install an accepted batch's inserts before publishing its events.
    /// The provider must return immediately when an insert is not ready.
    pub fn install_inserts(device: &LiveScalarDevice, events: &[crate::AudioEvent]) {
        let mut batch = Self::default();
        for event in events {
            batch.observe_inserts(event);
        }
        batch.flush_inserts(device);
    }

    /// Prepare and ship each requested reverb and logical insert slot.
    pub fn flush(&mut self, device: &LiveScalarDevice) {
        for (orbit, request) in self.requested.iter_mut().enumerate() {
            if let Some(params) = request.take() {
                device.ensure_reverb(orbit, params);
            }
        }
        self.flush_inserts(device);
    }

    pub(super) fn flush_inserts(&mut self, device: &LiveScalarDevice) {
        // A turn with no request also frees the copies the callback gave back.
        device.reclaim_assets();
        // The callback knows a chain before the first insert of the chain.
        let names = locked(&device.insert_names).clone();
        if let Some(names) = names {
            let shared = &device.shared;
            let turn = crate::insert::InsertTurn {
                refusals: shared.samples.insert_refusals.load(Ordering::Acquire),
                late_jobs: shared.plugin_late_jobs.load(Ordering::Acquire),
                inline_jobs: shared.plugin_jobs_inline.load(Ordering::Acquire),
            };
            let chains = names(turn);
            device.set_named_insert_chains(&chains.named);
            device.release_inserts(&chains.gone);
        }
        // The moved values fill the room of the ring. The rest waits in the
        // source for the next turn.
        let values = locked(&device.insert_values).clone();
        if let Some(values) = values {
            let ring = &device.shared.insert_values;
            for value in values(ring.capacity() - ring.len()) {
                if ring.push(value).is_err() {
                    break;
                }
            }
        }
        // A copy that was not ready gets another try on the next turn.
        let waiting = {
            let mut waiting = locked(&device.insert_waiting);
            // Removed chains must not occupy retry slots forever. Active
            // onsets refresh this age when they request the chain again.
            waiting.retain(|(_, _, requested)| requested.elapsed() < Duration::from_secs(30));
            waiting.clone()
        };
        for (slot, key, requested) in waiting {
            if !self.inserts.iter().any(|(current, _)| *current == slot) {
                device.ensure_insert_since(slot, key, requested);
            }
        }
        for (slot, key) in self.inserts.drain(..) {
            device.ensure_insert(slot, key);
        }
        device
            .shared
            .samples
            .insert_refusals
            .fetch_add(self.refused, Ordering::Relaxed);
        self.refused = 0;
        device.ship_plugin_pool();
    }
}

/// The plugin workers an output starts: the count its options ask for,
/// else the count `env` gives, the value of [`PLUGIN_THREADS_ENV`], else the
/// cores of the process minus 2.
pub(super) fn plugin_threads(asked: Option<u16>, env: Option<&str>) -> usize {
    asked
        .map(usize::from)
        .or_else(|| env?.parse().ok())
        .unwrap_or_else(crate::pool::plugin_workers)
}

impl LiveScalarDevice {
    /// Deliver a decoded sample to the callback's bank. Reclaims returned
    /// (displaced) samples first, so a full drain can never overflow the
    /// return ring; a full INSTALL ring hands the sample back for retry.
    pub fn install_sample(
        &self,
        id: crate::sample::SampleId,
        sample: crate::sample::DecodedSample,
    ) -> Result<(), crate::sample::DecodedSample> {
        self.reclaim_assets();
        let pointer = Box::into_raw(Box::new(sample));
        match self
            .shared
            .samples
            .installs
            .push(crate::assets::SampleInstall {
                id,
                sample: pointer,
            }) {
            Ok(()) => Ok(()),
            Err(install) => {
                // SAFETY: never left this thread; unique owner.
                Err(*unsafe { Box::from_raw(install.sample) })
            }
        }
    }

    /// Empty a live slot. A null install is the uninstall baton; the
    /// callback clears the bank and returns whatever it displaced. The
    /// bundled `bd` is refused here so a policy cannot silence the
    /// fallback hit.
    pub fn uninstall_sample(&self, id: crate::sample::SampleId) -> bool {
        if id == crate::sample::BUNDLED_BD_SAMPLE_ID {
            return true;
        }
        self.reclaim_assets();
        self.shared
            .samples
            .installs
            .push(crate::assets::SampleInstall {
                id,
                sample: std::ptr::null_mut(),
            })
            .is_ok()
    }

    /// Free any samples the callback displaced. Call only from the single
    /// producer thread that also drives `install_sample` / `ensure_reverb`;
    /// the return rings are SPSC.
    pub fn reclaim_samples(&self) {
        self.reclaim_assets();
    }

    /// Make sure `orbit`'s reverb matches `params`, generating and shipping
    /// a new one when it does not. Producer-side and potentially SLOW (IR
    /// synthesis + FFT partitioning, tens of ms) - the schedule lead absorbs
    /// it, and a repeat call with unchanged params is a cache hit.
    pub fn ensure_reverb(&self, orbit: usize, params: crate::reverb::ReverbParams) {
        let orbit = orbit.min(crate::scalar::MAX_ORBITS - 1);
        self.reclaim_assets();
        let mut cache = self.reverb_cache.lock().expect("producer reverb cache");
        if cache[orbit].is_some_and(|(cached, _)| cached == params) {
            return;
        }
        let reverb = Box::new(crate::reverb::OrbitReverb::generate_with_dispatch(
            self.sample_rate,
            params,
            self.dispatch(),
        ));
        let bytes = reverb.approx_bytes();
        let pointer = Box::into_raw(reverb);
        match self
            .shared
            .samples
            .reverb_installs
            .push(crate::assets::ReverbInstall {
                orbit: orbit as u8,
                reverb: pointer,
            }) {
            Ok(()) => {
                // The orbit's previous reverb goes back to be freed, so
                // this one takes its place in the count.
                cache[orbit] = Some((params, bytes));
            }
            Err(install) => {
                // Ring full: free and retry on a later batch.
                // SAFETY: never left this thread; unique owner.
                drop(unsafe { Box::from_raw(install.reverb) });
            }
        }
    }

    /// Set what builds the orbit inserts, for example a plugin host. The
    /// provider runs on the producer thread and answers fast for a key that
    /// has no effect.
    pub fn set_insert_provider(&self, provider: Arc<crate::insert::InsertProvider>) {
        *locked(&self.insert_provider) = Some(provider);
    }

    /// Prepare and ship a logical slot off the callback. Repeated requests
    /// reuse the shipped copy until the callback returns it.
    pub fn ensure_insert(&self, slot: crate::insert::InsertSlot, key: crate::insert::InsertKey) {
        self.ensure_insert_since(slot, key, Instant::now());
    }

    fn ensure_insert_since(
        &self,
        slot: crate::insert::InsertSlot,
        key: crate::insert::InsertKey,
        requested: Instant,
    ) {
        if !slot.is_valid() {
            self.shared
                .samples
                .insert_refusals
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.reclaim_assets();
        let mut cache = locked(&self.insert_cache);
        let mut waiting = locked(&self.insert_waiting);
        waiting.retain(|(held, _, _)| *held != slot);
        if cache
            .iter()
            .any(|(held, cached, _)| *held == slot && *cached == key)
        {
            return;
        }
        let provider = locked(&self.insert_provider).clone();
        let Some(provider) = provider else { return };
        if waiting.len() == crate::insert::INSERT_SLOTS
            || cache.len() == crate::insert::INSERT_SLOTS * 3
        {
            self.shared
                .samples
                .insert_refusals
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Not ready, or no room in the ring: the next turn tries again.
        waiting.push((slot, key, requested));
        // The provider keeps a copy the ring has no room for.
        let installs = &self.shared.samples.insert_installs;
        if installs.len() == installs.capacity() {
            return;
        }
        let Some(insert) = provider(key, self.sample_rate, slot) else {
            return;
        };
        if insert.any_thread() {
            self.start_plugin_pool();
        }
        let pointer = Box::into_raw(insert);
        match installs.push(crate::assets::InsertInstall {
            slot,
            sample_rate: self.sample_rate,
            insert: pointer,
        }) {
            Ok(()) => {
                cache.push((slot, key, pointer as *mut () as usize));
                waiting.retain(|(held, _, _)| *held != slot);
            }
            Err(install) => {
                // Only this thread fills the ring, and a place was free.
                // SAFETY: never left this thread. Unique owner.
                drop(unsafe { Box::from_raw(install.insert) });
            }
        }
    }

    /// Starts the plugin workers of this output, when it has none, for a
    /// copy that runs on any thread. The workers take a real-time class for
    /// the buffer and the rate of the stream.
    fn start_plugin_pool(&self) {
        let mut pool = locked(&self.plugin_pool);
        if pool.is_some() {
            return;
        }
        #[cfg(any(test, feature = "test-support"))]
        if let Some(workers) = crate::pool::test_workers() {
            *pool = crate::pool::PluginPool::start_test_pool(workers, true);
            return;
        }
        let env = std::env::var(PLUGIN_THREADS_ENV).ok();
        let workers = plugin_threads(self.options.plugin_threads(), env.as_deref());
        let frames = self
            .reported_buffer_frames
            .unwrap_or(self.requested_buffer_frames);
        *pool = crate::pool::PluginPool::start(workers, frames, self.sample_rate);
    }

    /// Gives the plugin workers to the backend of the stream, once for each
    /// backend, with the buffer and the rate of that stream. The workers go
    /// once one of them has its real-time class. Each turn also gives the
    /// workers the power mode.
    fn ship_plugin_pool(&self) {
        let mut pool = locked(&self.plugin_pool);
        let Some(pool) = pool.as_mut() else { return };
        pool.follow_power_mode();
        if self.plugin_pool_shipped.load(Ordering::Relaxed) || pool.ready_workers() == 0 {
            return;
        }
        let frames = self
            .reported_buffer_frames
            .unwrap_or(self.requested_buffer_frames);
        pool.shared().follow_stream(frames, self.sample_rate);
        // The flag goes before the workers. A new pool reaches the callback
        // with its gate closed, and the gate opens at the end of its first
        // buffer at the earliest. So the callback has the class before its
        // first job. After a recycle the flag is already set.
        self.shared.plugin_real_time.store(true, Ordering::Release);
        let shared = Arc::into_raw(Arc::clone(pool.shared()));
        match self
            .shared
            .samples
            .pool_installs
            .push(crate::assets::PoolInstall(shared))
        {
            Ok(()) => self.plugin_pool_shipped.store(true, Ordering::Relaxed),
            Err(install) => {
                // SAFETY: the count never left this thread.
                drop(unsafe { Arc::from_raw(install.0) });
            }
        }
    }

    /// The plugin workers that have their real-time class.
    pub(super) fn ready_plugin_workers(&self) -> u64 {
        locked(&self.plugin_pool)
            .as_ref()
            .map_or(0, |pool| pool.ready_workers() as u64)
    }

    /// True when this logical slot has a shipped copy not yet returned.
    pub fn holds_insert(
        &self,
        slot: crate::insert::InsertSlot,
        key: crate::insert::InsertKey,
    ) -> bool {
        locked(&self.insert_cache)
            .iter()
            .any(|(held, cached, _)| *held == slot && *cached == key)
    }

    /// Generate a `.FX()` stage reverb off the audio thread and ship it into
    /// the callback's stage pool. The eight most recent fingerprints avoid
    /// duplicate synthesis; older ones can be regenerated after eviction.
    pub fn ensure_fx_reverb(&self, params: crate::reverb::ReverbParams) {
        self.reclaim_assets();
        let mut cache = self
            .fx_reverb_cache
            .lock()
            .expect("producer fx reverb cache");
        if let Some(index) = cache.iter().position(|(cached, _, _)| cached == &params) {
            let hit = cache.remove(index);
            cache.push(hit);
            return;
        }
        let reverb = Box::new(
            crate::reverb::OrbitReverb::generate_streaming_with_dispatch(
                self.sample_rate,
                crate::reverb::ReverbParams { ir: None, ..params },
                self.dispatch(),
            ),
        );
        let bytes = reverb.approx_bytes();
        if bytes > crate::scalar::MAX_FX_REVERB_BYTES {
            // The callback refuses a room above its byte budget. Keep the
            // fingerprint, or each later event generates the room again.
            self.shared
                .samples
                .fx_reverb_refusals
                .fetch_add(1, Ordering::Relaxed);
            if cache.len() >= 8 {
                cache.remove(0);
            }
            cache.push((params, 0, 0));
            return;
        }
        let pointer = Box::into_raw(reverb);
        match self
            .shared
            .samples
            .fx_reverb_installs
            .push(crate::assets::FxReverbInstall { reverb: pointer })
        {
            Ok(()) => {
                if cache.len() >= 8 {
                    cache.remove(0);
                }
                cache.push((params, bytes, pointer as usize));
            }
            Err(install) => {
                self.shared
                    .samples
                    .fx_reverb_refusals
                    .fetch_add(1, Ordering::Relaxed);
                // SAFETY: never left this thread; unique owner.
                drop(unsafe { Box::from_raw(install.reverb) });
            }
        }
    }

    /// Prepare independent native graph state for this event on the producer.
    /// Cache entries reserve processors through their whole sounding interval.
    /// Pitch changes reuse storage while overlapping voices remain independent.
    pub fn ensure_kabel(
        &self,
        event: &crate::AudioEvent,
    ) -> Result<crate::KabelPublication, String> {
        self.prepare_kabel_events(std::slice::from_ref(event))
    }

    /// Prepare a complete scheduling window transactionally before publishing
    /// its generation. Failure leaves the current cache and install queue intact.
    /// Keep the returned reservation until all events are published or discarded.
    pub fn prepare_kabel_events(
        &self,
        events: &[crate::AudioEvent],
    ) -> Result<crate::KabelPublication, String> {
        if !events.iter().any(|event| {
            crate::scalar::kabel_controls(&event.controls)
                .next()
                .is_some()
        }) {
            return Ok(crate::KabelPublication::default());
        }
        let publication = crate::KabelPublication::new(self.shared.samples.clone());
        self.reclaim_assets();
        let mut cache = self.kabel_cache.lock().expect("producer KabelSalat cache");
        let mut program_cache = self
            .program_cache
            .lock()
            .expect("producer KabelSalat program cache");
        let mut staged_programs = program_cache.clone();
        let mut prepared_programs: Vec<Box<crate::assets::PreparedKabelProgram>> = Vec::new();
        let mut staged = cache.clone();
        let mut prepared = Vec::new();
        let mut resident: usize = staged.iter().map(|entry| entry.bytes).sum();
        let sr = f64::from(self.sample_rate);
        // A callback activates all its onsets before retiring finished voices.
        // Reserve one host block plus the downstream filter-ring allowance.
        let block_frames = self
            .reported_buffer_frames
            .unwrap_or(self.requested_buffer_frames)
            .max(4096);
        for event in events {
            let start = event.target_frame as f64 / sr;
            let end = crate::scalar::kabel_controls(&event.controls)
                .map(|controls| controls.end_secs)
                .fold(start, f64::max)
                + 0.01
                + (f64::from(block_frames) + 384.0) / sr;
            let until_frame = (end * sr).ceil() as u64;
            for controls in crate::scalar::kabel_controls(&event.controls) {
                let id = controls.program.id();
                let existing = staged_programs.iter().position(|entry| entry.id == id);
                // Republish code in every window. A callback may already have
                // retired a cached box before this reservation started.
                {
                    if existing.is_none()
                        && staged_programs.len() >= crate::assets::MAX_KABEL_PROGRAMS
                    {
                        self.shared
                            .samples
                            .kabel_refusals
                            .fetch_add(1, Ordering::Relaxed);
                        return Err(
                            "KabelSalat program storage exceeds the 4096-program limit".into()
                        );
                    }
                    // One install per identity in this window. Extending an already
                    // published lease replaces its box and returns the old baton.
                    let pointer = if let Some(prepared) = prepared_programs
                        .iter_mut()
                        .find(|prepared| prepared.program.id() == id)
                    {
                        prepared.until_frame = prepared.until_frame.max(until_frame);
                        (&**prepared as *const crate::assets::PreparedKabelProgram) as usize
                    } else {
                        let prepared = Box::new(crate::assets::PreparedKabelProgram {
                            program: *controls.program,
                            until_frame,
                        });
                        let pointer =
                            (&*prepared as *const crate::assets::PreparedKabelProgram) as usize;
                        prepared_programs.push(prepared);
                        pointer
                    };
                    let reservation = KabelProgramReservation { id, pointer };
                    if let Some(index) = existing {
                        staged_programs[index] = reservation;
                    } else {
                        staged_programs.push(reservation);
                    }
                }
                let delays = controls.program.delay_count();
                if let Some(entry) = staged
                    .iter_mut()
                    .filter(|entry| entry.until_secs <= start && entry.delays >= delays)
                    .min_by_key(|entry| entry.delays)
                {
                    entry.until_secs = end;
                    continue;
                }
                let graph = Box::new(rustel_kabel::Graph::new(&controls.program, sr)?);
                let bytes = crate::scalar::kabel_graph_bytes(&graph);
                if staged.len() >= crate::MAX_KABEL_GRAPHS
                    || bytes > crate::MAX_KABEL_BYTES.saturating_sub(resident)
                {
                    self.shared
                        .samples
                        .kabel_refusals
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(
                        "KabelSalat graph storage exceeds the 128-graph or 64 MiB limit".into(),
                    );
                }
                resident += bytes;
                staged.push(KabelReservation {
                    delays,
                    bytes,
                    pointer: (&*graph as *const rustel_kabel::Graph) as usize,
                    until_secs: end,
                });
                prepared.push(graph);
            }
        }
        let queue = &self.shared.samples.kabel_installs;
        if prepared.len() > queue.capacity().saturating_sub(queue.len()) {
            self.shared
                .samples
                .kabel_refusals
                .fetch_add(1, Ordering::Relaxed);
            return Err(
                "KabelSalat preparation queue is full; reduce simultaneous worklets".into(),
            );
        }
        let programs = &self.shared.samples.program_installs;
        if prepared_programs.len() > programs.capacity().saturating_sub(programs.len()) {
            self.shared
                .samples
                .kabel_refusals
                .fetch_add(1, Ordering::Relaxed);
            return Err("KabelSalat program preparation queue is full; reduce worklets in the scheduling window".into());
        }
        for program in prepared_programs {
            let pointer = Box::into_raw(program);
            // This producer reserved the entire window above. The consumer
            // can only increase the available queue capacity.
            if let Err(install) = programs.push(crate::assets::ProgramInstall { program: pointer })
            {
                // SAFETY: the rejected baton never left this producer.
                drop(unsafe { Box::from_raw(install.program) });
                return Err("KabelSalat program queue ownership changed unexpectedly".into());
            }
        }
        for graph in prepared {
            let pointer = Box::into_raw(graph);
            // Only this producer adds installs. The consumer can only make
            // more room after the complete-window capacity check above.
            if let Err(install) = queue.push(crate::assets::KabelInstall { graph: pointer }) {
                // SAFETY: the rejected install never left the producer.
                drop(unsafe { Box::from_raw(install.graph) });
                return Err("KabelSalat preparation queue ownership changed unexpectedly".into());
            }
        }
        *cache = staged;
        *program_cache = staged_programs;
        Ok(publication)
    }

    pub(super) fn reclaim_assets(&self) {
        let mut boxes = Vec::new();
        {
            // The drop of the device reclaims too, so a lock that a panic
            // poisoned does not panic here.
            let mut cache = locked(&self.fx_reverb_cache);
            let mut kabel = locked(&self.kabel_cache);
            let mut programs = locked(&self.program_cache);
            let mut inserts = locked(&self.insert_cache);
            self.shared.samples.reclaim_with_inserts(
                |returned| {
                    cache.retain(|(_, _, installed)| *installed != returned as usize);
                },
                |returned| {
                    kabel.retain(|entry| entry.pointer != returned as usize);
                },
                |returned| {
                    programs.retain(|entry| entry.pointer != returned as usize);
                },
                |insert| {
                    let returned = &*insert as *const dyn crate::insert::OrbitInsert as *const ();
                    inserts.retain(|(_, _, installed)| *installed != returned as usize);
                    boxes.push(insert);
                },
            );
        }
        // A returned insert drops after the locks: the drop of a Csound
        // insert joins its worker.
        drop(boxes);
    }

    /// Sets the source of the chain names: the output asks the source
    /// before it takes the inserts of a turn, with the counts of the
    /// output, and gives the answer to [`Self::set_named_insert_chains`].
    pub fn set_insert_names(&self, names: Arc<crate::insert::InsertNames>) {
        *locked(&self.insert_names) = Some(names);
    }

    /// Sets the source of the values sliders moved: the output asks the
    /// source at each turn of inserts, after the names, for as many values as
    /// its ring has room for. The callback gives each value to its insert at
    /// the start of its next buffer.
    pub fn set_insert_values(&self, values: Arc<crate::insert::InsertValues>) {
        *locked(&self.insert_values) = Some(values);
    }

    /// Names the insert chains the playing score has, 64 at most. The
    /// callback keeps the inserts of a named chain through each silence,
    /// and gives back the inserts of each other chain after its tail. A
    /// named chain with no free bus takes the bus of a chain with no name,
    /// after a fade of 10 ms. The chain of an orbit keeps its bus. An output
    /// that never gets a list keeps the rule by use: a chain leaves after 30
    /// seconds with no use.
    ///
    /// Name a chain before its inserts go to the output, so the callback
    /// knows the chain when its first insert arrives.
    pub fn set_named_insert_chains(&self, named: &[u64]) {
        let slots = &self.shared.insert_named;
        let name = |at: usize| named.get(at).copied().unwrap_or(u64::MAX);
        let listed = self.shared.insert_named_list.load(Ordering::Relaxed) != 0;
        if listed && (0..slots.len()).all(|at| slots[at].load(Ordering::Relaxed) == name(at)) {
            return;
        }
        // The callback takes a list only with an even number, read before
        // and after its copy. A read of a name from this write then also
        // sees the odd number: so each name is a release and an acquire.
        self.shared.insert_named_list.fetch_add(1, Ordering::AcqRel);
        for (at, slot) in slots.iter().enumerate() {
            slot.store(name(at), Ordering::Release);
        }
        self.shared.insert_named_list.fetch_add(1, Ordering::AcqRel);
    }

    /// Asks the callback to give back each copy of `gone` it holds: a copy
    /// the playing score no longer has. The callback keeps a copy that is
    /// busy or heard, so the release goes again each second while the
    /// callback holds the copy. With a full ring the rest go at the next
    /// turn. The output makes no more tries to ship such a copy.
    fn release_inserts(&self, gone: &[(crate::insert::InsertSlot, crate::insert::InsertKey)]) {
        if gone.is_empty() {
            return;
        }
        let cache = locked(&self.insert_cache);
        let mut waiting = locked(&self.insert_waiting);
        let mut released = locked(&self.insert_released);
        let holds = |slot, key| {
            cache
                .iter()
                .any(|(held, cached, _)| (*held, *cached) == (slot, key))
        };
        // A copy that came back needs no release.
        released.retain(|(slot, key, _)| holds(*slot, *key));
        for &(slot, key) in gone {
            waiting.retain(|(held, wanted, _)| (*held, *wanted) != (slot, key));
            if !holds(slot, key) {
                continue;
            }
            let last = released
                .iter()
                .position(|(held, cached, _)| (*held, *cached) == (slot, key));
            if last.is_some_and(|last| released[last].2.elapsed() < Duration::from_secs(1)) {
                continue;
            }
            let release = crate::assets::InsertRelease { slot, key };
            if self.shared.samples.insert_releases.push(release).is_err() {
                break;
            }
            match last {
                Some(last) => released[last].2 = Instant::now(),
                None => released.push((slot, key, Instant::now())),
            }
        }
    }
}

impl LiveScalarDevice {
    /// Drops each insert of a closed stream on the producer: each copy in the
    /// line to the callback, each copy the backend holds, then each copy the
    /// callback gave back. No box drops while a lock of the device is held.
    /// A close asks the names source nothing.
    ///
    /// Call it after the callback retired and its stream closed.
    pub(super) fn hand_back_inserts(&mut self) {
        for install in self.shared.samples.insert_installs.drain_available() {
            // SAFETY: the old callback joined. This unconsumed box has one owner.
            drop(unsafe { Box::from_raw(install.insert) });
        }
        if let Some(cell) = self.backend.take() {
            // SAFETY: the callback retired, so no render uses the backend
            // again, and this thread holds no other reference to it.
            let backend = unsafe { cell.get() };
            backend.take_inserts().for_each(drop);
        }
        self.reclaim_assets();
    }

    pub(super) fn clear_reverb_caches(&self) {
        // The old callback joined. Its replacement starts with an empty
        // backend, so the producer must prepare its assets again.
        self.reverb_cache
            .lock()
            .expect("producer reverb cache")
            .fill(None);
        self.fx_reverb_cache
            .lock()
            .expect("producer fx reverb cache")
            .clear();
        locked(&self.insert_cache).clear();
        locked(&self.insert_waiting).clear();
        locked(&self.insert_released).clear();
        self.kabel_cache
            .lock()
            .expect("producer KabelSalat cache")
            .clear();
        self.program_cache
            .lock()
            .expect("producer KabelSalat program cache")
            .clear();
        self.shared
            .samples
            .program_resident_bytes
            .store(0, Ordering::Release);
        self.shared
            .samples
            .kabel_resident_bytes
            .store(0, Ordering::Release);
        self.shared
            .samples
            .fx_reverb_resident_bytes
            .store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;

    /// Waits until `ready` gives true. After 10 s the test fails with `what`.
    fn until(what: &str, ready: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !ready() {
            assert!(start.elapsed().as_secs() < 10, "{what}");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn finished_kabel_voices_release_capacity_for_a_larger_patch() {
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        device.silent.take().expect("callback").stop();
        let code = kabel_program();
        let events: Vec<_> = (0..128)
            .map(|id| {
                let mut event = kabel_event(code, 0);
                event.onset_id = id;
                event
            })
            .collect();
        let mut backend = crate::LiveScalarBackend::new(48_000, 128).unwrap();
        let first = device
            .prepare_kabel_events(&events[..64])
            .expect("first graph batch");
        backend.drain_reverb_installs(&device.shared.samples);
        let second = device
            .prepare_kabel_events(&events[64..])
            .expect("second graph batch");
        backend.drain_reverb_installs(&device.shared.samples);
        for event in events {
            assert!(device.shared.ring.push(event));
        }
        drop((first, second));
        let delay = rustel_kabel::Program::from_json(
            r#"{"nodes":[{"type":"n","value":0.25},{"type":"n","value":0.001},{"type":"delay","inputs":[0,1]},{"type":"output","inputs":[2]}]}"#,
        ).unwrap();
        let next = kabel_event(delay, 1);
        assert!(
            device
                .prepare_kabel_events(std::slice::from_ref(&next))
                .is_err()
        );
        let generation = AtomicU64::new(7);
        let zero = AtomicU64::new(0);
        let flip = crate::LiveFlipAtomics {
            generation: &generation,
            takeover_frame: &zero,
            takeover_cut: &zero,
            line_arm: &zero,
        };
        let stopped = AtomicBool::new(false);
        let mut output = [0.0; 1024];
        let mut audible = false;
        let mut returned = 0;
        for block in 0..32 {
            backend.process_block_with(
                &mut output,
                512,
                block * 512,
                &device.shared.ring,
                flip,
                &stopped,
            );
            audible |= output.iter().any(|sample| sample.abs() > 0.01);
            backend.retire_kabel_programs(
                &device.shared.samples,
                &device.shared.ring,
                &device.shared.immediate,
            );
            returned += device.shared.samples.kabel_returns.len();
            device.reclaim_assets();
        }
        assert!(audible, "the first window rendered before retirement");
        assert_eq!(returned, 128);
        let _publication = device
            .prepare_kabel_events(&[next])
            .expect("idle processors were reclaimed");
        assert_eq!(device.kabel_cache.lock().unwrap().len(), 1);
        assert_eq!(device.shared.samples.kabel_installs.len(), 1);
        assert_eq!(device.shared.samples.leaked.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn kabel_window_refusal_publishes_no_partial_assets() {
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        device.silent.take().expect("callback").stop();
        let events: Vec<_> = (1..=513)
            .map(|second| kabel_event(kabel_program(), second))
            .collect();
        let error = device
            .prepare_kabel_events(&events)
            .expect_err("program queue limit");
        assert!(error.contains("program preparation queue"), "{error}");
        assert_eq!(device.shared.samples.program_installs.len(), 0);
        assert_eq!(device.shared.samples.kabel_installs.len(), 0);
        assert!(device.kabel_cache.lock().unwrap().is_empty());
        assert!(device.program_cache.lock().unwrap().is_empty());
        let _publication = device.prepare_kabel_events(&events[..1]).expect("recovery");
        assert_eq!(device.shared.samples.program_installs.len(), 1);
        assert_eq!(device.shared.samples.kabel_installs.len(), 1);
    }

    #[test]
    fn mixed_reverb_batch_keeps_its_kabel_publication_until_drop() {
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        device.silent.take().expect("callback").stop();
        let graph = kabel_event(kabel_program(), 1);
        let mut plain = graph.clone();
        plain.controls.kabel = None;
        let active = &device.shared.samples.active_kabel_publications;
        let mut batch = LiveReverbBatch::default();
        batch.observe(&device, &plain).expect("ordinary event");
        assert_eq!(active.load(Ordering::Acquire), 0);
        batch.observe(&device, &graph).expect("graph event");
        assert_eq!(active.load(Ordering::Acquire), 1);
        batch.observe(&device, &plain).expect("ordinary event");
        batch.observe(&device, &graph).expect("another graph event");
        assert_eq!(active.load(Ordering::Acquire), 1);
        drop(batch);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    /// A batch asks for the inserts of each event it observes: an event
    /// with its worklets prepared by the batch, and an event prepared
    /// with its window. An effect goes to the slot of its stage.
    #[test]
    fn a_batch_ships_the_inserts_of_each_event_it_observes() {
        struct Quiet(crate::insert::InsertKey);
        impl crate::insert::OrbitInsert for Quiet {
            fn key(&self) -> crate::insert::InsertKey {
                self.0
            }
            fn set_param(&mut self, _: crate::insert::InsertParam, _: u32) {}
            fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
        }
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        device.silent.take().expect("callback").stop();
        device.set_insert_provider(Arc::new(|key, _, _| {
            Some(Box::new(Quiet(key)) as Box<dyn crate::insert::OrbitInsert>)
        }));
        let key = |plugin| crate::insert::InsertKey { plugin, preset: 0 };
        // The effect of plugin 1 has stage 1, and the effect of plugin 2
        // stage 2.
        let event = |plugin: u32| {
            let mut event = kabel_event(kabel_program(), 1);
            event.controls.insert_chain = Some(u64::from(plugin));
            let mut effect = crate::insert::InsertControls::new(key(plugin));
            effect.set_stage(plugin as u8);
            event.controls.effects[0] = Some(effect);
            event
        };
        let mut batch = LiveReverbBatch::default();
        batch.observe(&device, &event(1)).expect("graph event");
        batch.observe_prepared(&device, &event(2));
        batch.flush(&device);
        assert_eq!(device.shared.samples.kabel_installs.len(), 1);
        for plugin in [1, 2] {
            let slot = crate::insert::effect_slot(u64::from(plugin), plugin as usize);
            assert!(device.holds_insert(slot, key(plugin)), "plugin {plugin}");
        }
        assert_eq!(device.shared.samples.insert_installs.len(), 2);
    }

    #[test]
    fn kabel_program_lease_extension_keeps_new_cache_entry() {
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        device.silent.take().expect("callback").stop();
        let program = kabel_program();
        let first = kabel_event(program, 1);
        let _first_publication = device
            .prepare_kabel_events(&[first.clone(), first])
            .expect("first lease");
        assert_eq!(device.shared.samples.program_installs.len(), 1);
        let _extended_publication = device
            .prepare_kabel_events(&[kabel_event(program, 2)])
            .expect("extended lease");
        assert_eq!(device.shared.samples.program_installs.len(), 2);
        let old = device
            .shared
            .samples
            .program_installs
            .pop()
            .expect("old lease");
        let new = device
            .shared
            .samples
            .program_installs
            .pop()
            .expect("new lease");
        // The stopped callback leaves both batons exclusively owned here.
        assert!(unsafe { (*new.program).until_frame > (*old.program).until_frame });
        device
            .shared
            .samples
            .program_returns
            .push(crate::assets::ReturnedProgram(old.program))
            .ok()
            .expect("return old lease");
        device.reclaim_assets();
        let cache = device.program_cache.lock().unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].pointer, new.program as usize);
        drop(cache);
        device
            .shared
            .samples
            .program_returns
            .push(crate::assets::ReturnedProgram(new.program))
            .ok()
            .expect("return new lease");
        device.reclaim_assets();
        assert!(device.program_cache.lock().unwrap().is_empty());
    }

    /// An output's reverbs are counted where the producer ships them. The
    /// same room is the reverb already there, a new room on an orbit takes
    /// the old one's place rather than adding to it, and a recycled backend
    /// holds none until asked again. The backend is measured once, before
    /// the callback owns it.
    #[test]
    fn an_outputs_memory_counts_each_orbit_reverb_once() {
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        let opened = device.memory();
        assert!(opened.backend > 0, "{opened:?}");
        assert_eq!((opened.reverbs, opened.input, opened.record), (0, 0, 0));
        let small = crate::reverb::ReverbParams {
            size_secs: 0.025,
            fade_secs: 0.01,
            lp_start_hz: 15_000.0,
            lp_end_hz: 1_000.0,
            ir: None,
        };
        let large = crate::reverb::ReverbParams {
            size_secs: 0.5,
            ..small
        };

        device.ensure_reverb(0, small);
        let one = device.memory().reverbs;
        assert!(one > 0);
        device.ensure_reverb(0, small);
        assert_eq!(device.memory().reverbs, one, "the same room");
        device.ensure_reverb(0, large);
        let larger = device.memory().reverbs;
        assert!(
            larger > one,
            "replaced by the larger room: {larger} against {one}"
        );
        device.ensure_reverb(1, small);
        assert_eq!(device.memory().reverbs, larger + one, "another orbit's own");

        device.recycle_output().expect("recycle silent output");
        let recycled = device.memory();
        assert_eq!(recycled.reverbs, 0, "a fresh backend has none yet");
        assert_eq!(recycled.backend, opened.backend, "the same backend again");
    }

    /// More than eight room settings remain usable across repeated cache cycles.
    #[test]
    fn nine_distinct_stage_reverbs_repeat_after_cache_eviction() {
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        device.silent.take().expect("silent callback").stop();
        let room = |index: usize| crate::reverb::ReverbParams {
            size_secs: 0.025,
            fade_secs: 0.01,
            lp_start_hz: 15_000.0,
            lp_end_hz: 1_000.0 + index as f32,
            ir: None,
        };
        let mut installed = Vec::new();
        for _ in 0..3 {
            for index in 0..9 {
                device.ensure_fx_reverb(room(index));
            }
            assert_eq!(device.shared.samples.fx_reverb_installs.len(), 9);
            for (index, install) in device
                .shared
                .samples
                .fx_reverb_installs
                .drain_available()
                .enumerate()
            {
                // SAFETY: the callback is stopped. This thread owns the popped box.
                let reverb = unsafe { Box::from_raw(install.reverb) };
                assert_eq!(reverb.params(), room(index));
                installed.push(reverb);
            }
        }
        assert_eq!(installed.len(), 27);
        assert_eq!(device.report().asset_queues.fx_reverb_refusals, 0);
    }

    #[test]
    fn returning_an_old_room_does_not_invalidate_its_replacement() {
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        device.silent.take().expect("silent callback").stop();
        let room = |index: usize| crate::reverb::ReverbParams {
            size_secs: 0.001,
            fade_secs: 0.0,
            lp_start_hz: 15_000.0,
            lp_end_hz: 1_000.0 + index as f32,
            ir: None,
        };
        device.ensure_fx_reverb(room(0));
        let old = device
            .shared
            .samples
            .fx_reverb_installs
            .pop()
            .expect("old room");
        for index in 1..=8 {
            device.ensure_fx_reverb(room(index));
        }
        let other_rooms: Vec<_> = device
            .shared
            .samples
            .fx_reverb_installs
            .drain_available()
            .map(|install| {
                // SAFETY: the callback is stopped. This thread owns each popped box.
                unsafe { Box::from_raw(install.reverb) }
            })
            .collect();
        device.ensure_fx_reverb(room(0));
        let replacement = device
            .shared
            .samples
            .fx_reverb_installs
            .pop()
            .expect("replacement room");
        assert_ne!(old.reverb, replacement.reverb);

        assert!(
            device
                .shared
                .samples
                .fx_reverb_returns
                .push(crate::assets::ReturnedFxReverb(old.reverb))
                .is_ok()
        );
        device.reclaim_assets();
        device.ensure_fx_reverb(room(0));
        assert_eq!(
            device.shared.samples.fx_reverb_installs.len(),
            0,
            "the replacement remains cached"
        );

        assert!(
            device
                .shared
                .samples
                .fx_reverb_returns
                .push(crate::assets::ReturnedFxReverb(replacement.reverb))
                .is_ok()
        );
        device.reclaim_assets();
        device.ensure_fx_reverb(room(0));
        assert_eq!(
            device.shared.samples.fx_reverb_installs.len(),
            1,
            "returning the replacement invalidates its cache entry"
        );
        drop(other_rooms);
    }

    #[test]
    fn a_room_above_the_byte_budget_is_refused_once_and_not_generated_again() {
        let mut device = LiveScalarDevice::start_silent(192_000, 7).expect("silent output");
        device.silent.take().expect("silent callback").stop();
        let oversized = crate::reverb::ReverbParams {
            size_secs: 6.0,
            fade_secs: 0.0,
            lp_start_hz: 15_000.0,
            lp_end_hz: 1_000.0,
            ir: None,
        };
        device.ensure_fx_reverb(oversized);
        device.ensure_fx_reverb(oversized);
        assert_eq!(device.shared.samples.fx_reverb_installs.len(), 0);
        assert_eq!(device.report().asset_queues.fx_reverb_refusals, 1);
    }

    #[test]
    fn a_full_stage_install_ring_counts_a_refusal_and_allows_a_retry() {
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        device.silent.take().expect("silent callback").stop();
        let room = |index: usize| crate::reverb::ReverbParams {
            size_secs: 0.001,
            fade_secs: 0.0,
            lp_start_hz: 15_000.0,
            lp_end_hz: 1_000.0 + index as f32,
            ir: None,
        };
        let capacity = device.shared.samples.fx_reverb_installs.capacity();
        for index in 0..=capacity {
            device.ensure_fx_reverb(room(index));
        }
        assert_eq!(device.shared.samples.fx_reverb_installs.len(), capacity);
        assert_eq!(device.report().asset_queues.fx_reverb_refusals, 1);
        let returned = device
            .shared
            .samples
            .fx_reverb_installs
            .pop()
            .expect("queued room");
        assert!(
            device
                .shared
                .samples
                .fx_reverb_returns
                .push(crate::assets::ReturnedFxReverb(returned.reverb))
                .is_ok()
        );
        device.ensure_fx_reverb(room(capacity));
        assert_eq!(
            device.shared.samples.fx_reverb_installs.len(),
            capacity,
            "the refused parameters can be retried"
        );
        assert_eq!(device.report().asset_queues.fx_reverb_refusals, 1);
    }

    /// When the byte budget fills, the callback retires an idle room and admits
    /// the next request without freeing large IR buffers on the audio thread.
    #[test]
    fn a_stage_reverb_replaces_an_idle_room_at_the_byte_budget() {
        let device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        let room = |lp_end_hz| crate::reverb::ReverbParams {
            size_secs: 6.0,
            fade_secs: 0.1,
            lp_start_hz: 15_000.0,
            lp_end_hz,
            ir: None,
        };
        device.ensure_fx_reverb(room(1_000.0));
        let each = device.memory().reverbs;
        let budget = crate::scalar::MAX_FX_REVERB_BYTES;
        assert!(
            each > 0 && 2 * each <= budget && 3 * each > budget,
            "two rooms fit the budget and a third does not: {each} each"
        );
        device.ensure_fx_reverb(room(2_000.0));
        assert_eq!(device.memory().reverbs, 2 * each, "both kept");
        device.ensure_fx_reverb(room(3_000.0));
        assert_eq!(
            device.memory().reverbs,
            2 * each,
            "the idle room is displaced without exceeding the budget"
        );

        // The callback hands an idle old room back, while accepting the third.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while device.report().asset_queues.fx_reverb_returns == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the callback never retired an old room"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(device.report().asset_queues.fx_reverb_refusals, 0);
    }

    /// The producer builds an insert one time for an orbit, the running
    /// callback keeps the instance when its output orbit changes,
    /// and a displaced insert comes back for the producer to free. A copy
    /// the score no longer has comes back too. Each copy drops on the
    /// producer, also the copies of a closed stream.
    #[test]
    fn the_callback_installs_and_returns_the_insert_of_an_orbit() {
        use std::sync::atomic::AtomicUsize;

        #[derive(Default)]
        struct Seen {
            built: AtomicUsize,
            reject: AtomicBool,
            busy: AtomicBool,
            processed: [AtomicUsize; 2],
            parameters: [AtomicUsize; 2],
            /// The thread of each drop.
            dropped: std::sync::Mutex<Vec<std::thread::ThreadId>>,
            /// Each drop panics.
            fault: AtomicBool,
        }
        struct Probe(crate::insert::InsertKey, Arc<Seen>, usize);
        impl crate::insert::OrbitInsert for Probe {
            fn key(&self) -> crate::insert::InsertKey {
                self.0
            }
            fn set_param(&mut self, _param: crate::insert::InsertParam, _frames: u32) {
                self.2 += 1;
                self.1.parameters[self.0.plugin as usize - 1].store(self.2, Ordering::Relaxed);
            }
            fn process(&mut self, _left: &mut [f32], _right: &mut [f32]) {
                self.1.processed[self.0.plugin as usize - 1].fetch_add(1, Ordering::Relaxed);
            }
            fn busy(&self) -> bool {
                self.1.busy.load(Ordering::Relaxed)
            }
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                let thread = std::thread::current().id();
                self.1.dropped.lock().unwrap().push(thread);
                if self.1.fault.load(Ordering::Relaxed) {
                    panic!("a drop fault");
                }
            }
        }

        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        let seen = Arc::new(Seen::default());
        // The count of drops. Each drop ran on this thread.
        let producer = std::thread::current().id();
        let dropped = || {
            let threads = seen.dropped.lock().unwrap();
            assert!(threads.iter().all(|thread| *thread == producer));
            threads.len()
        };
        let key = |plugin| crate::insert::InsertKey { plugin, preset: 0 };
        // A note with no provider plays dry, and a later request tries again.
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(1));
        device.set_insert_provider(Arc::new({
            let seen = Arc::clone(&seen);
            move |wanted, sample_rate, orbit| {
                assert_eq!(
                    (sample_rate, orbit),
                    (48_000, crate::insert::effect_slot(1, 0))
                );
                // A copy that is not ready gives nothing.
                if seen.reject.load(Ordering::Relaxed) {
                    return None;
                }
                seen.built.fetch_add(1, Ordering::Relaxed);
                Some(Box::new(Probe(wanted, Arc::clone(&seen), 0)) as Box<_>)
            }
        }));
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(1));
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(1));
        assert_eq!(seen.built.load(Ordering::Relaxed), 1);
        // An effect runs for a note that asks for the effect.
        let mut controls = crate::OscillatorControls {
            orbit: 1,
            ..Default::default()
        };
        let mut effect = crate::insert::InsertControls::new(key(1));
        assert!(effect.push(crate::insert::InsertParam { id: 0, value: 0.5 }));
        controls.effects[0] = Some(effect);
        let mut event = AudioEvent {
            onset_id: 1,
            generation: device.generation(),
            target_frame: device.clock_frames() + 4_800,
            onset_lead: 0.0,
            freq_hz: 220.0,
            gain: 0.5,
            duration_secs: 0.05,
            ui_visuals: 0,
            controls,
            sample: None,
            synth: None,
            wavetable: None,
            cut: None,
        };
        assert!(device.push(event.clone()));
        until("the callback did not run the insert", || {
            seen.processed[0].load(Ordering::Relaxed) > 0
        });

        // The output orbit moves. The physical insert and its state stay.
        for orbit in [2, 1] {
            event.onset_id += 1;
            event.target_frame = device.clock_frames() + 4_800;
            event.controls.orbit = orbit;
            event.controls.insert_chain = Some(1);
            LiveReverbBatch::install_inserts(&device, std::slice::from_ref(&event));
            assert_eq!(seen.built.load(Ordering::Relaxed), 1);
            assert!(
                device
                    .insert_waiting
                    .lock()
                    .expect("waiting inserts")
                    .is_empty()
            );
            assert!(device.push(event.clone()));
            until("the rerouted note did not reach the same insert", || {
                seen.parameters[0].load(Ordering::Relaxed) == event.onset_id as usize
            });
        }
        assert!(device.holds_insert(crate::insert::effect_slot(1, 0), key(1)));
        assert!(!device.holds_insert(crate::insert::effect_slot(2, 0), key(1)));
        assert_eq!(dropped(), 0);

        device.ensure_insert(crate::insert::effect_slot(1, 0), key(2));
        assert_eq!(seen.built.load(Ordering::Relaxed), 2);
        // The slot has 2 copies now. A note that wants the active copy
        // again builds nothing.
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(1));
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(2));
        assert_eq!(seen.built.load(Ordering::Relaxed), 2);
        let staged_by = device.clock_frames() + 512;
        until("the callback did not stage the replacement", || {
            device.clock_frames() >= staged_by
        });
        assert_eq!(seen.processed[1].load(Ordering::Relaxed), 0);
        assert_eq!(device.shared.samples.insert_returns.len(), 0);
        event.onset_id += 1;
        event.target_frame = device.clock_frames() + 4_800;
        event.controls.effects[0] = Some(crate::insert::InsertControls::new(key(2)));
        // The note ends 60 ms after its onset. Its chain is silent 200 ms
        // after the onset.
        let quiet_by = event.target_frame + 9_600;
        assert!(device.push(event.clone()));
        until("the displaced insert did not come back", || {
            seen.processed[1].load(Ordering::Relaxed) > 0
                && device.shared.samples.insert_returns.len() == 1
        });
        assert_eq!(dropped(), 0);
        // A turn with no request frees the copy that came back.
        LiveReverbBatch::default().flush_inserts(&device);
        assert_eq!(dropped(), 1);
        assert!(device.holds_insert(crate::insert::effect_slot(1, 0), key(2)));
        assert!(!device.holds_insert(crate::insert::effect_slot(1, 0), key(1)));

        // The names source gives a copy the score no longer has. The
        // callback keeps the copy while it is busy, and the device sends the
        // release again 1 s later. The device frees the copy that comes back
        // and forgets its releases.
        let slot = crate::insert::effect_slot(1, 0);
        let gone = Arc::new(std::sync::Mutex::new(vec![(slot, key(2))]));
        device.set_insert_names(Arc::new({
            let gone = Arc::clone(&gone);
            move |_| crate::insert::InsertChains {
                named: vec![1],
                gone: gone.lock().unwrap().clone(),
            }
        }));
        let releases = &device.shared.samples.insert_releases;
        assert_eq!(releases.capacity(), crate::insert::INSERT_SLOTS);
        let sent = || {
            let released = device.insert_released.lock().unwrap();
            released.first().map(|(.., at)| *at)
        };
        until("the note did not end", || device.clock_frames() >= quiet_by);
        seen.busy.store(true, Ordering::Relaxed);
        LiveReverbBatch::default().flush_inserts(&device);
        let first = sent().expect("a release");
        until("the callback did not take the release", || {
            releases.len() == 0
        });
        let taken_by = device.clock_frames() + 512;
        until("the callback did not run", || {
            device.clock_frames() >= taken_by
        });
        assert_eq!(device.shared.samples.insert_returns.len(), 0);
        // A busy machine stalls the waits above at times. The time of the
        // release moves to now, so the next turn comes within 1 s of it.
        let now = Instant::now();
        device.insert_released.lock().unwrap()[0].2 = now;
        LiveReverbBatch::default().flush_inserts(&device);
        assert_eq!(sent(), Some(now), "one release each second");
        seen.busy.store(false, Ordering::Relaxed);
        device.insert_released.lock().unwrap()[0].2 = first - Duration::from_secs(1);
        LiveReverbBatch::default().flush_inserts(&device);
        assert!(sent() > Some(first));
        until("the released insert did not come back", || {
            device.shared.samples.insert_returns.len() == 1
        });
        LiveReverbBatch::default().flush_inserts(&device);
        assert_eq!(dropped(), 2);
        assert!(!device.holds_insert(slot, key(2)));
        assert_eq!(sent(), None);

        // A stopped output makes no sound: its callback gives the copy back
        // at once, also while it is busy.
        *gone.lock().unwrap() = Vec::new();
        device.ensure_insert(slot, key(2));
        let played = seen.processed[1].load(Ordering::Relaxed);
        event.onset_id += 1;
        event.target_frame = device.clock_frames() + 4_800;
        assert!(device.push(event));
        until("the next copy did not play", || {
            seen.processed[1].load(Ordering::Relaxed) > played
        });
        seen.busy.store(true, Ordering::Relaxed);
        assert!(device.stop_and_wait(Duration::from_secs(5)));
        *gone.lock().unwrap() = vec![(slot, key(2))];
        LiveReverbBatch::default().flush_inserts(&device);
        until("the stopped output kept the insert", || {
            device.shared.samples.insert_returns.len() == 1
        });
        LiveReverbBatch::default().flush_inserts(&device);
        assert_eq!(dropped(), 3);
        assert!(!device.holds_insert(slot, key(2)));
        device.resume();
        seen.busy.store(false, Ordering::Relaxed);
        // The callback holds a waiting copy of key 2 for the steps below.
        device.ensure_insert(slot, key(2));
        until("the callback did not take the copy", || {
            device.shared.samples.insert_installs.len() == 0
        });

        // Stop consumption so the next copy stays in the line during the
        // recycle. A copy that is not ready waits for the next turn. The
        // device keeps the backend of the stopped thread and its copy.
        device.silent.as_mut().unwrap().stop();
        seen.reject.store(true, Ordering::Relaxed);
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(1));
        assert_eq!(dropped(), 3);
        assert_eq!(device.shared.samples.insert_installs.len(), 0);
        assert_eq!(
            device
                .insert_waiting
                .lock()
                .unwrap()
                .iter()
                .map(|(slot, key, _)| (*slot, *key))
                .collect::<Vec<_>>(),
            [(crate::insert::effect_slot(1, 0), key(1))]
        );
        assert!(device.holds_insert(crate::insert::effect_slot(1, 0), key(2)));
        // A copy the score no longer has waits no more.
        *gone.lock().unwrap() = vec![(slot, key(1))];
        LiveReverbBatch::default().flush_inserts(&device);
        assert!(device.insert_waiting.lock().unwrap().is_empty());
        device.ensure_insert(slot, key(1));
        // A full line of releases holds the release of the copy of key 2
        // to the next turn.
        *gone.lock().unwrap() = vec![(slot, key(2))];
        let releases = &device.shared.samples.insert_releases;
        let stale = crate::assets::InsertRelease { slot, key: key(3) };
        while releases.push(stale).is_ok() {}
        device.insert_waiting.lock().unwrap()[0].2 = Instant::now() - Duration::from_secs(31);
        let built = seen.built.load(Ordering::Relaxed);
        LiveReverbBatch::default().flush_inserts(&device);
        assert!(device.insert_waiting.lock().unwrap().is_empty());
        assert_eq!(seen.built.load(Ordering::Relaxed), built);
        assert!(device.insert_released.lock().unwrap().is_empty());
        releases.pop();
        LiveReverbBatch::default().flush_inserts(&device);
        assert_eq!(releases.len(), releases.capacity());
        assert_eq!(device.insert_released.lock().unwrap().len(), 1);
        seen.reject.store(false, Ordering::Relaxed);
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(1));
        assert_eq!(device.shared.samples.insert_installs.len(), 1);

        // A copy that came back drops after the locks of the device, so a
        // drop that panics poisons none of them. A turn that asks for a new
        // orbit reverb frees the copy first.
        let faulty = Arc::new(Seen::default());
        faulty.fault.store(true, Ordering::Relaxed);
        let copy = Box::new(Probe(key(1), faulty, 0)) as Box<dyn crate::insert::OrbitInsert>;
        let returned = crate::assets::ReturnedInsert(Box::into_raw(copy));
        assert!(device.shared.samples.insert_returns.push(returned).is_ok());
        let room = crate::reverb::ReverbParams {
            size_secs: 0.025,
            fade_secs: 0.01,
            lp_start_hz: 15_000.0,
            lp_end_hz: 1_000.0,
            ir: None,
        };
        let turn = std::panic::AssertUnwindSafe(|| {
            let mut batch = LiveReverbBatch::default();
            batch.request_orbit(0, room);
            batch.flush(&device);
        });
        assert!(std::panic::catch_unwind(turn).is_err());
        assert!(!device.reverb_cache.is_poisoned() && !device.insert_cache.is_poisoned());
        assert!(!device.fx_reverb_cache.is_poisoned());

        // With a full line the provider keeps its copy, and the slot
        // waits for the next turn.
        let installs = &device.shared.samples.insert_installs;
        let returns = &device.shared.samples.insert_returns;
        assert_eq!(installs.capacity(), crate::insert::INSERT_SLOTS);
        assert_eq!(returns.capacity(), crate::insert::INSERT_SLOTS);
        while installs.len() < installs.capacity() {
            let spare = Box::new(Probe(key(1), Arc::default(), 0));
            let install = crate::assets::InsertInstall {
                slot: crate::insert::effect_slot(1, 0),
                sample_rate: 48_000,
                insert: Box::into_raw(spare),
            };
            assert!(installs.push(install).is_ok());
        }
        let built = seen.built.load(Ordering::Relaxed);
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(3));
        assert_eq!(seen.built.load(Ordering::Relaxed), built);
        assert_eq!(device.insert_waiting.lock().unwrap().len(), 1);
        // The device has room for an active, a prepared and a leaving copy
        // of each slot. Entries with no box fill the room. The device
        // refuses a copy more.
        let fill = |copies| {
            let entry = (crate::insert::effect_slot(2, 0), key(4), 0);
            device.insert_cache.lock().unwrap().resize(copies, entry);
        };
        let refused = device.report().asset_queues.insert_refusals;
        fill(crate::insert::INSERT_SLOTS * 3 - 1);
        device.ensure_insert(slot, key(3));
        assert_eq!(device.report().asset_queues.insert_refusals, refused);
        assert_eq!(device.insert_waiting.lock().unwrap().len(), 1);
        fill(crate::insert::INSERT_SLOTS * 3);
        device.ensure_insert(slot, key(3));
        assert_eq!(device.report().asset_queues.insert_refusals, refused + 1);
        assert!(device.insert_waiting.lock().unwrap().is_empty());
        assert_eq!(
            device.insert_cache.lock().unwrap().capacity(),
            crate::insert::INSERT_SLOTS * 3
        );

        // A host that keeps the callback after its stream closed keeps the
        // backend. The close takes each copy out of it all the same.
        let held = device.backend.clone();
        assert!(
            device
                .recycle_output_to_with(
                    Some(SILENT_OUTPUT_NAME),
                    |_| unreachable!("silent output needs no discovery"),
                    |_, _| Err(DevicePlaybackError::Unavailable(
                        "test preparation failure".into()
                    )),
                )
                .is_err()
        );
        // The close drops the queued copy of key 1 and the copy of key 2
        // the backend held.
        assert_eq!(dropped(), 5);
        drop(held);
        assert_eq!(device.shared.samples.insert_installs.len(), 0);
        assert!(!device.holds_insert(crate::insert::effect_slot(1, 0), key(1)));
        assert!(device.insert_waiting.lock().unwrap().is_empty());
        // The releases of the old callback go, and so does their record.
        assert_eq!(device.shared.samples.insert_releases.len(), 0);
        assert!(device.insert_released.lock().unwrap().is_empty());
        *gone.lock().unwrap() = Vec::new();

        device
            .recycle_output_to_with(
                Some(SILENT_OUTPUT_NAME),
                |_| unreachable!("silent output needs no discovery"),
                |device, _| device.prepare_recycled_output(None),
            )
            .expect("replacement output");
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(1));
        assert!(device.holds_insert(crate::insert::effect_slot(1, 0), key(1)));

        // A provider that panics does not end the output. This step is
        // the last one: the insert locks stay poisoned after it.
        let slot = crate::insert::effect_slot(1, 0);
        device.set_insert_provider(Arc::new(|_, _, _| panic!("a provider fault")));
        let request = std::panic::AssertUnwindSafe(|| device.ensure_insert(slot, key(2)));
        assert!(std::panic::catch_unwind(request).is_err());
        assert!(device.insert_cache.is_poisoned() && device.insert_waiting.is_poisoned());
        device.set_insert_provider(Arc::new({
            let seen = Arc::clone(&seen);
            move |wanted, _, _| Some(Box::new(Probe(wanted, Arc::clone(&seen), 0)) as Box<_>)
        }));
        LiveReverbBatch::default().flush_inserts(&device);
        assert!(device.holds_insert(slot, key(2)));
        // The close reclaims also with a cache lock that a panic poisoned.
        let fault = std::panic::AssertUnwindSafe(|| {
            let _cache = device.fx_reverb_cache.lock();
            panic!("a reverb fault");
        });
        assert!(std::panic::catch_unwind(fault).is_err());
        drop(device);
    }

    /// A moved value reaches the copy of its key after one turn, and a note
    /// of its generation or older takes it at activation. A newer note keeps
    /// its own value. A turn takes the values the ring has room for, and the
    /// rest come at the next turn. The same with plugin workers.
    #[test]
    fn a_slider_value_reaches_the_insert_now_and_each_older_note() {
        use std::sync::atomic::{AtomicU32, AtomicUsize};

        /// The last value of parameter 0, as bits, the count of values and
        /// the count of blocks.
        #[derive(Default)]
        struct Seen {
            value: AtomicU32,
            values: AtomicUsize,
            processed: AtomicUsize,
        }
        struct Probe(crate::insert::InsertKey, Arc<Seen>, bool);
        impl crate::insert::OrbitInsert for Probe {
            fn key(&self) -> crate::insert::InsertKey {
                self.0
            }
            fn set_param(&mut self, param: crate::insert::InsertParam, _frames: u32) {
                if param.id == 0 {
                    self.1.value.store(param.value.to_bits(), Ordering::Relaxed);
                }
                self.1.values.fetch_add(1, Ordering::Release);
            }
            fn process(&mut self, _left: &mut [f32], _right: &mut [f32]) {
                self.1.processed.fetch_add(1, Ordering::Relaxed);
            }
            fn any_thread(&self) -> bool {
                self.2
            }
        }

        let _tripwire_guard = tripwire_assertion_guard();
        for pooled in [false, true] {
            crate::set_plugin_pool_for_test(pooled.then_some(2));
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            let seen = Arc::new(Seen::default());
            let value = || f32::from_bits(seen.value.load(Ordering::Relaxed));
            // Acquire pairs with the Release in `set_param`: a value read
            // after a count is as new as that count.
            let values = || seen.values.load(Ordering::Acquire);
            let key = crate::insert::InsertKey {
                plugin: 1,
                preset: 0,
            };
            let slot = crate::insert::effect_slot(1, 0);
            device.set_insert_provider(Arc::new({
                let seen = Arc::clone(&seen);
                move |key, _, _| Some(Box::new(Probe(key, Arc::clone(&seen), pooled)) as Box<_>)
            }));
            // The source gives the values in its line that the ring has room
            // for.
            let line = Arc::new(std::sync::Mutex::new(Vec::new()));
            device.set_insert_values(Arc::new({
                let line = Arc::clone(&line);
                move |room| {
                    let mut line = line.lock().unwrap();
                    let taken = room.min(line.len());
                    line.drain(..taken).collect()
                }
            }));
            let moved = |id, value| crate::insert::InsertValue {
                slot,
                key,
                param: crate::insert::InsertParam { id, value },
                generation: 7,
            };
            // A note of `generation` with the value 0.5 for parameter 0.
            let note = |onset_id, generation| {
                let mut effect = crate::insert::InsertControls::new(key);
                assert!(effect.push(crate::insert::InsertParam { id: 0, value: 0.5 }));
                let mut controls = crate::OscillatorControls {
                    orbit: 1,
                    ..Default::default()
                };
                controls.effects[0] = Some(effect);
                AudioEvent {
                    onset_id,
                    generation,
                    target_frame: device.clock_frames() + 4_800,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.5,
                    duration_secs: 0.05,
                    ui_visuals: 0,
                    controls,
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }
            };
            device.ensure_insert(slot, key);
            // The turn ships the plugin workers.
            LiveReverbBatch::default().flush_inserts(&device);
            assert!(device.push(note(1, 7)));
            until("the callback did not run the insert", || {
                seen.processed.load(Ordering::Relaxed) > 0 && value() == 0.5
            });
            assert_eq!(device.report().plugin_workers, if pooled { 2 } else { 0 });

            line.lock().unwrap().push(moved(0, 0.25));
            LiveReverbBatch::default().flush_inserts(&device);
            until("the value did not reach the insert", || value() == 0.25);
            let before = values();
            assert!(device.push(note(2, 7)));
            until("the note of the generation did not come", || {
                values() > before
            });
            assert_eq!(value(), 0.25);
            device.set_generation(8, device.clock_frames(), TakeoverCut::None);
            let before = values();
            assert!(device.push(note(3, 8)));
            until("the newer note did not come", || values() > before);
            assert_eq!(value(), 0.5);

            let before = values();
            line.lock()
                .unwrap()
                .extend((1..=70).map(|id| moved(id, 0.5)));
            LiveReverbBatch::default().flush_inserts(&device);
            assert_eq!(line.lock().unwrap().len(), 6);
            until("the first values did not come", || values() == before + 64);
            LiveReverbBatch::default().flush_inserts(&device);
            assert!(line.lock().unwrap().is_empty());
            until("the last values did not come", || values() == before + 70);
            // With no callback to take them, the values past the room of the
            // ring wait in the source.
            device.silent.take().expect("callback").stop();
            line.lock()
                .unwrap()
                .extend((1..=70).map(|id| moved(id, 0.5)));
            for _ in 0..2 {
                LiveReverbBatch::default().flush_inserts(&device);
            }
            let ring = &device.shared.insert_values;
            assert_eq!((ring.len(), line.lock().unwrap().len()), (64, 6));
            drop(device);
            crate::set_plugin_pool_for_test(None);
        }
    }

    /// A copy that runs on any thread starts the plugin workers before its
    /// install. A copy that does not, or an output that asks for no
    /// workers, starts none. Workers with no real-time class stay with the
    /// producer. Each backend takes the workers once, a recycle ships them to
    /// the next backend with its buffer, and the device drop joins them.
    #[test]
    fn a_box_for_any_thread_starts_the_pool_and_the_device_drop_joins_it() {
        struct Probe(crate::insert::InsertKey);
        impl crate::insert::OrbitInsert for Probe {
            fn key(&self) -> crate::insert::InsertKey {
                self.0
            }
            fn set_param(&mut self, _: crate::insert::InsertParam, _: u32) {}
            fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
            // Plugin 2 runs on any thread.
            fn any_thread(&self) -> bool {
                self.0.plugin == 2
            }
        }
        fn probe(
            key: crate::insert::InsertKey,
            _: u32,
            _: crate::insert::InsertSlot,
        ) -> Option<Box<dyn crate::insert::OrbitInsert>> {
            Some(Box::new(Probe(key)))
        }
        let key = |plugin| crate::insert::InsertKey { plugin, preset: 0 };
        let _tripwire_guard = tripwire_assertion_guard();
        // An output that asks for no plugin workers starts none.
        crate::set_plugin_pool_for_test(None);
        let options = LiveOutputOptions::default().with_plugin_threads(Some(0));
        let device =
            LiveScalarDevice::start_silent_with_options(48_000, 7, options).expect("silent output");
        device.set_insert_provider(Arc::new(probe));
        device.ensure_insert(crate::insert::effect_slot(2, 0), key(2));
        assert!(locked(&device.plugin_pool).is_none());
        // Workers with no real-time class stay with the producer, and the
        // callback keeps its priority.
        *locked(&device.plugin_pool) = crate::pool::PluginPool::start_without_class(1);
        LiveReverbBatch::default().flush_inserts(&device);
        assert!(!device.shared.plugin_real_time.load(Ordering::Relaxed));
        drop(device);

        crate::set_plugin_pool_for_test(Some(2));
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        device.silent.take().expect("callback").stop();
        device.set_insert_provider(Arc::new(probe));
        let installs = &device.shared.samples.pool_installs;
        device.ensure_insert(crate::insert::effect_slot(1, 0), key(1));
        assert!(locked(&device.plugin_pool).is_none());
        LiveReverbBatch::default().flush_inserts(&device);
        assert_eq!(installs.len(), 0);
        device.ensure_insert(crate::insert::effect_slot(2, 0), key(2));
        let shared = Arc::clone(
            locked(&device.plugin_pool)
                .as_ref()
                .expect("plugin workers")
                .shared(),
        );
        assert_eq!(device.report().plugin_workers, 2);
        // The workers, the device and this test hold the pool.
        assert_eq!(Arc::strong_count(&shared), 4);

        // The turn ships the pool, and a backend takes it once. The callback
        // then takes the class of the workers.
        LiveReverbBatch::default().flush_inserts(&device);
        assert_eq!(installs.len(), 1);
        assert!(device.shared.plugin_real_time.load(Ordering::Relaxed));
        let mut backend = crate::LiveScalarBackend::new(48_000, 8).unwrap();
        backend.drain_insert_installs(&device.shared.samples);
        assert_eq!((installs.len(), Arc::strong_count(&shared)), (0, 5));
        LiveReverbBatch::default().flush_inserts(&device);
        assert_eq!(installs.len(), 0);
        drop(backend);

        // A failed replacement keeps the workers. The next turn ships them,
        // and the recycle after it drops what no backend took.
        let fail = |device: &mut LiveScalarDevice| {
            device.recycle_output_to_with(
                Some(SILENT_OUTPUT_NAME),
                |_| unreachable!("silent output needs no discovery"),
                |_, _| Err(DevicePlaybackError::Unavailable("no output".into())),
            )
        };
        assert!(fail(&mut device).is_err());
        let installs = &device.shared.samples.pool_installs;
        LiveReverbBatch::default().flush_inserts(&device);
        assert_eq!((installs.len(), Arc::strong_count(&shared)), (1, 5));
        assert!(fail(&mut device).is_err());
        let installs = &device.shared.samples.pool_installs;
        assert_eq!((installs.len(), Arc::strong_count(&shared)), (0, 4));

        // The replacement output takes the same workers, which follow its
        // buffer.
        device.set_buffer_preference(AudioBufferPreference::Frames(257));
        device
            .recycle_output_to_with(
                Some(SILENT_OUTPUT_NAME),
                |_| unreachable!("silent output needs no discovery"),
                |device, _| device.prepare_recycled_output(None),
            )
            .expect("replacement output");
        LiveReverbBatch::default().flush_inserts(&device);
        until("the next backend did not take the pool", || {
            device.shared.samples.pool_installs.len() == 0
        });
        assert_eq!(Arc::strong_count(&shared), 5);
        assert_eq!(device.report().plugin_workers, 2);
        assert_eq!(shared.period(), (257, device.sample_rate()));

        // The drop stops the output, then joins each worker.
        drop(device);
        assert_eq!(Arc::strong_count(&shared), 1);
        crate::set_plugin_pool_for_test(None);
    }

    #[test]
    fn live_reverb_batch_keeps_only_the_final_request_per_orbit() {
        let first = crate::reverb::ReverbParams {
            ir: None,
            size_secs: 0.1,
            fade_secs: 0.1,
            lp_start_hz: 15_000.0,
            lp_end_hz: 1_000.0,
        };
        let final_request = crate::reverb::ReverbParams {
            size_secs: 0.2,
            ..first
        };
        let mut batch = LiveReverbBatch::default();

        batch.request_orbit(0, first);
        batch.request_orbit(0, final_request);
        batch.request_orbit(1, first);

        assert_eq!(batch.requested[0], Some(final_request));
        assert_eq!(batch.requested[1], Some(first));
        assert_eq!(batch.requested.iter().flatten().count(), 2);
    }

    /// The names source hears the counts of the output at each turn. A close
    /// gives the source no turn.
    #[test]
    fn the_names_source_hears_the_counts_of_the_output() {
        let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
        let turns = Arc::new(std::sync::Mutex::new(Vec::new()));
        device.set_insert_names(Arc::new({
            let turns = Arc::clone(&turns);
            move |turn| {
                turns.lock().unwrap().push(turn);
                crate::insert::InsertChains::default()
            }
        }));
        let shared = &device.shared;
        shared
            .samples
            .insert_refusals
            .fetch_add(1, Ordering::Relaxed);
        shared.plugin_late_jobs.fetch_add(2, Ordering::Relaxed);
        shared.plugin_jobs_inline.fetch_add(3, Ordering::Relaxed);
        LiveReverbBatch::default().flush_inserts(&device);
        let turn = crate::insert::InsertTurn {
            refusals: 1,
            late_jobs: 2,
            inline_jobs: 3,
        };
        assert_eq!(*turns.lock().unwrap(), [turn]);
        assert!(
            device
                .recycle_output_to_with(
                    Some(SILENT_OUTPUT_NAME),
                    |_| unreachable!("silent output needs no discovery"),
                    |_, _| Err(DevicePlaybackError::Unavailable("no output".into())),
                )
                .is_err()
        );
        drop(device);
        assert_eq!(*turns.lock().unwrap(), [turn]);
    }
}
