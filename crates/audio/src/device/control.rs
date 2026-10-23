use super::*;

impl LiveStop {
    pub(super) fn new() -> Self {
        Self(AtomicU8::new(OUTPUT_RUNNING))
    }

    pub(super) fn request(&self) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                matches!(state, OUTPUT_RUNNING | OUTPUT_RESUME_PENDING)
                    .then_some(OUTPUT_STOP_REQUESTED)
            });
    }

    pub(super) fn resume(&self) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                OUTPUT_STOP_REQUESTED => Some(OUTPUT_RESUME_PENDING),
                OUTPUT_STOP_ACKNOWLEDGED => Some(OUTPUT_RUNNING),
                _ => None,
            });
    }

    pub(super) fn acknowledged(&self) -> bool {
        self.0.load(Ordering::Acquire) == OUTPUT_STOP_ACKNOWLEDGED
    }

    #[cfg(test)]
    pub(super) fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Acquire) != OUTPUT_RUNNING
    }

    pub(super) fn complete(&self, observed: u8) {
        if !matches!(observed, OUTPUT_STOP_REQUESTED | OUTPUT_RESUME_PENDING) {
            return;
        }
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                OUTPUT_STOP_REQUESTED => Some(OUTPUT_STOP_ACKNOWLEDGED),
                OUTPUT_RESUME_PENDING => Some(OUTPUT_RUNNING),
                _ => None,
            });
    }
}

impl LiveShared {
    pub(super) fn new(initial_generation: u64) -> Self {
        Self {
            ring: Arc::new(Ring::new(LIVE_RING_CAPACITY)),
            immediate: Arc::new(Ring::new(64)),
            live_controls: Arc::new(crate::assets::AssetRing::new()),
            insert_values: Arc::new(crate::assets::AssetRing::new()),
            max_polyphony: Arc::new(AtomicUsize::new(crate::MAX_POLYPHONY)),
            confirmations: crate::confirmation::ConfirmationChannel::new(),
            analysis: Arc::new(LiveAnalysisTap::with_sides()),
            visual_analysis: Arc::new(std::array::from_fn(|_| OnceLock::new())),
            visual_analysis_mask: Arc::new(AtomicU64::new(0)),
            visual_analysis_generation_floor: Arc::new(AtomicU64::new(initial_generation)),
            record: Arc::new(RecordTap::new()),
            orbit_peaks: Arc::new(
                (0..crate::scalar::MAX_ORBITS)
                    .map(|_| std::sync::atomic::AtomicU32::new(0))
                    .collect(),
            ),
            orbit_pairs: Arc::new(
                (0..crate::scalar::MAX_ORBITS)
                    .map(|_| std::sync::atomic::AtomicU8::new(0))
                    .collect(),
            ),
            insert_named: Arc::new(
                (0..crate::insert::MAX_INSERT_BUSES)
                    .map(|_| AtomicU64::new(u64::MAX))
                    .collect(),
            ),
            insert_named_list: Arc::new(AtomicU64::new(0)),
            orbit_gains: Arc::new(
                (0..crate::scalar::MAX_ORBITS)
                    .map(|_| std::sync::atomic::AtomicU32::new(1.0f32.to_bits()))
                    .collect(),
            ),
            input: Arc::new(crate::input::InputRing::new()),
            samples: Arc::new(crate::assets::SampleChannel::new()),
            generation: Arc::new(AtomicU64::new(initial_generation)),
            takeover_frame: Arc::new(AtomicU64::new(0)),
            takeover_cut: Arc::new(AtomicU64::new(0)),
            line_arm: Arc::new(AtomicU64::new(0)),
            cut_sounding: Arc::new(AtomicBool::new(false)),
            frames: Arc::new(AtomicU64::new(0)),
            completed_intake_frame: Arc::new(AtomicU64::new(0)),
            rendering_until_frame: Arc::new(AtomicU64::new(0)),
            playback_origin_nanos: Arc::new(AtomicU64::new(UNSET_STREAM_INSTANT_NANOS)),
            buffer_playback_nanos: Arc::new(AtomicU64::new(0)),
            playback_latency_nanos: Arc::new(AtomicU64::new(0)),
            max_playback_latency_nanos: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(LiveStop::new()),
            stop_ring_end: Arc::new(AtomicUsize::new(0)),
            stop_immediate_end: Arc::new(AtomicUsize::new(0)),
            failed: Arc::new(AtomicBool::new(false)),
            callbacks: Arc::new(AtomicU64::new(0)),
            accepted_events: Arc::new(AtomicU64::new(0)),
            stale_events: Arc::new(AtomicU64::new(0)),
            late_events: Arc::new(AtomicU64::new(0)),
            last_callback_nanos: Arc::new(AtomicU64::new(0)),
            max_callback_gap_nanos: Arc::new(AtomicU64::new(0)),
            max_callback_busy_nanos: Arc::new(AtomicU64::new(0)),
            realtime_load: Arc::new(RealtimeLoadPublisher::new()),
            realtime_pressure: Arc::new(RealtimePressurePublisher::new()),
            clock_base: Instant::now(),
            refused_voices: Arc::new(AtomicU64::new(0)),
            callback_errors: Arc::new(AtomicU64::new(0)),
            callback_xruns: Arc::new(AtomicU64::new(0)),
            callback_scope_misses: Arc::new(AtomicU64::new(0)),
            cutover_race_blocks: Arc::new(AtomicU64::new(0)),
            plugin_jobs_inline: Arc::new(AtomicU64::new(0)),
            plugin_join_wait_nanos: Arc::new(AtomicU64::new(0)),
            plugin_late_jobs: Arc::new(AtomicU64::new(0)),
            plugin_real_time: Arc::new(AtomicBool::new(false)),
            meter: Arc::new(crate::meter::MasterMeterShared::new()),
            score_sources_active: Arc::new(AtomicBool::new(false)),
            score_peak: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    pub(super) fn report(&self, stream_id: u64) -> LiveDeviceReport {
        LiveDeviceReport {
            stream_id,
            submitted_frames: self.clock_frames(),
            playhead_nanos: 0,
            buffer_playback_nanos: self.buffer_playback_nanos.load(Ordering::Acquire),
            playback_latency_nanos: self.playback_latency_nanos.load(Ordering::Acquire),
            max_playback_latency_nanos: self.max_playback_latency_nanos.load(Ordering::Acquire),
            generation: self.generation.load(Ordering::Acquire),
            callbacks: self.callbacks.load(Ordering::Acquire),
            accepted_events: self.accepted_events.load(Ordering::Acquire),
            stale_events_filtered: self.stale_events.load(Ordering::Acquire),
            late_events: self.late_events.load(Ordering::Acquire),
            max_callback_gap_nanos: self.max_callback_gap_nanos.load(Ordering::Acquire),
            max_callback_busy_nanos: self.max_callback_busy_nanos.load(Ordering::Acquire),
            realtime_load: self.realtime_load.try_snapshot().unwrap_or_default(),
            realtime_pressure: self.realtime_pressure.try_snapshot().unwrap_or_default(),
            refused_voices: self.refused_voices.load(Ordering::Acquire),
            callback_errors: self.callback_errors.load(Ordering::Acquire),
            callback_xruns: self.callback_xruns.load(Ordering::Acquire),
            callback_scope_misses: self.callback_scope_misses.load(Ordering::Acquire),
            callback_allocations: 0,
            callback_frees: 0,
            stop_acknowledged: self.stop.acknowledged(),
            ring_refusals: self.ring.refused.load(Ordering::Acquire) as u64,
            ring_role_conflicts: self.ring.role_conflicts.load(Ordering::Acquire) as u64,
            ring_depth: self.ring.len() as u64,
            ring_capacity: self.ring.capacity() as u64,
            ring_peak_depth: self.ring.peak_depth.load(Ordering::Acquire) as u64,
            asset_queues: AssetQueuePressureSnapshot {
                capacity: self.samples.installs.capacity() as u64,
                sample_installs: self.samples.installs.len() as u64,
                sample_returns: self.samples.returns.len() as u64,
                orbit_reverb_installs: self.samples.reverb_installs.len() as u64,
                orbit_reverb_returns: self.samples.reverb_returns.len() as u64,
                fx_reverb_installs: self.samples.fx_reverb_installs.len() as u64,
                fx_reverb_returns: self.samples.fx_reverb_returns.len() as u64,
                fx_reverb_refusals: self.samples.fx_reverb_refusals.load(Ordering::Acquire),
                kabel_installs: self.samples.kabel_installs.len() as u64,
                kabel_returns: self.samples.kabel_returns.len() as u64,
                kabel_refusals: self.samples.kabel_refusals.load(Ordering::Acquire),
                insert_refusals: self.samples.insert_refusals.load(Ordering::Acquire),
                leaked: self.samples.leaked.load(Ordering::Acquire),
            },
            cutover_race_blocks: self.cutover_race_blocks.load(Ordering::Acquire),
            plugin_workers: 0,
            plugin_jobs_inline: self.plugin_jobs_inline.load(Ordering::Acquire),
            plugin_join_wait_nanos: self.plugin_join_wait_nanos.load(Ordering::Acquire),
            plugin_late_jobs: self.plugin_late_jobs.load(Ordering::Acquire),
        }
    }

    pub(super) fn clock_frames(&self) -> u64 {
        self.frames.load(Ordering::Acquire)
    }

    pub(super) fn request_stop(&self) {
        self.stop_ring_end
            .store(self.ring.published_end(), Ordering::Release);
        self.stop_immediate_end
            .store(self.immediate.published_end(), Ordering::Release);
        self.stop.request();
    }

    /// The four handshake words the callback reads, each under its own name.
    pub(super) fn flip_atomics(&self) -> crate::LiveFlipAtomics<'_> {
        crate::LiveFlipAtomics {
            generation: &self.generation,
            takeover_frame: &self.takeover_frame,
            takeover_cut: &self.takeover_cut,
            line_arm: &self.line_arm,
        }
    }

    pub(super) fn render_frontier_frames(&self) -> u64 {
        self.clock_frames()
            .max(self.rendering_until_frame.load(Ordering::Acquire))
    }

    /// Move the shared absolute frame domain to a replacement stream's rate
    /// and clear telemetry whose origin belongs to the discarded CPAL stream.
    /// The old callback is joined and the replacement remains gated.
    pub(super) fn prepare_recycled_stream(&self, old_sample_rate: u32, new_sample_rate: u32) {
        if old_sample_rate != new_sample_rate {
            self.record.close_current();
            while let Some(install) = self.samples.reverb_installs.pop() {
                // SAFETY: neither callback can consume the queue. Popping
                // transfers this pending allocation to the producer.
                drop(unsafe { Box::from_raw(install.reverb) });
            }
            while let Some(install) = self.samples.fx_reverb_installs.pop() {
                // SAFETY: the same exclusive handoff applies to stage IRs.
                drop(unsafe { Box::from_raw(install.reverb) });
            }
        }
        // Every replacement discards queued onsets and resets their producer
        // reservations. Pending K assets must follow those events even at the
        // same rate, or they consume capacity the fresh cache cannot account for.
        while let Some(install) = self.samples.program_installs.pop() {
            // SAFETY: both callbacks are stopped. The queued program is ours.
            drop(unsafe { Box::from_raw(install.program) });
        }
        while let Some(install) = self.samples.kabel_installs.pop() {
            // SAFETY: both callbacks are stopped during stream replacement.
            drop(unsafe { Box::from_raw(install.graph) });
        }
        let frames = rebase_frame_rate(
            self.frames.load(Ordering::Acquire),
            old_sample_rate,
            new_sample_rate,
        );
        let completed_intake_frame = rebase_frame_rate(
            self.completed_intake_frame.load(Ordering::Acquire),
            old_sample_rate,
            new_sample_rate,
        );
        let takeover_frame = rebase_frame_rate(
            self.takeover_frame.load(Ordering::Acquire),
            old_sample_rate,
            new_sample_rate,
        );
        self.takeover_frame.store(takeover_frame, Ordering::Release);
        self.takeover_cut
            .store(TakeoverCut::None as u64, Ordering::Release);
        // A launch's pre-armed line lives in the same frame domain: left in
        // the old rate's frames, a countdown across a rate change would cut
        // early or late by the ratio. Only an armed word carries a frame;
        // zero and the withdrawal marker mean the same at any rate.
        let line_arm = self.line_arm.load(Ordering::Acquire);
        if line_arm & 0b01 != 0 {
            let line = rebase_frame_rate(line_arm >> 2, old_sample_rate, new_sample_rate);
            self.line_arm
                .store((line << 2) | (line_arm & 0b11), Ordering::Release);
        }
        self.frames.store(frames, Ordering::Release);
        self.completed_intake_frame
            .store(completed_intake_frame, Ordering::Release);
        // The old callback has joined: there is no remaining in-flight
        // reservation to carry into the new frame domain.
        self.rendering_until_frame.store(frames, Ordering::Release);
        for ring in [&self.ring, &self.immediate] {
            ring.publish_frontier(frames, frames, new_sample_rate);
        }
        self.playback_origin_nanos
            .store(UNSET_STREAM_INSTANT_NANOS, Ordering::Release);
        self.buffer_playback_nanos.store(0, Ordering::Release);
        self.playback_latency_nanos.store(0, Ordering::Release);
        self.last_callback_nanos.store(0, Ordering::Release);
        self.realtime_load.reset();
        self.realtime_pressure.reset();
    }

    pub(super) fn reset_analysis(&self) {
        self.analysis.reset();
        for tap in self.visual_analysis.iter().filter_map(OnceLock::get) {
            tap.reset();
        }
    }

    pub(super) fn visual_analysis_tap(&self, slot: usize) -> Option<&LiveAnalysisTap> {
        self.visual_analysis.get(slot)?.get().map(Box::as_ref)
    }

    fn ensure_visual_analysis_tap(&self, slot: usize) -> Option<&LiveAnalysisTap> {
        Some(
            self.visual_analysis
                .get(slot)?
                .get_or_init(|| Box::new(LiveAnalysisTap::new()))
                .as_ref(),
        )
    }

    fn set_visual_analysis_mask(&self, mask: u64) {
        let previous = self.visual_analysis_mask.load(Ordering::Acquire);
        let mut removed = previous & !mask;
        while removed != 0 {
            let slot = removed.trailing_zeros() as usize;
            if let Some(tap) = self.visual_analysis_tap(slot) {
                tap.set_enabled(false);
            }
            removed &= removed - 1;
        }
        let mut added = mask & !previous;
        while added != 0 {
            let slot = added.trailing_zeros() as usize;
            if let Some(tap) = self.ensure_visual_analysis_tap(slot) {
                tap.set_enabled(true);
            }
            added &= added - 1;
        }
        self.visual_analysis_mask.store(mask, Ordering::Release);
    }

    fn reset_visual_analysis_mask(&self, mask: u64) {
        self.visual_analysis_mask.store(0, Ordering::Release);
        self.visual_analysis_generation_floor
            .store(self.generation.load(Ordering::Acquire), Ordering::Release);
        for tap in self.visual_analysis.iter().filter_map(OnceLock::get) {
            tap.set_enabled(false);
        }
        let mut slots = mask;
        while slots != 0 {
            let slot = slots.trailing_zeros() as usize;
            if let Some(tap) = self.ensure_visual_analysis_tap(slot) {
                tap.set_enabled(true);
            }
            slots &= slots - 1;
        }
        self.visual_analysis_mask.store(mask, Ordering::Release);
    }

    fn playhead_at(&self, now: StreamInstant) -> u64 {
        let origin = self.playback_origin_nanos.load(Ordering::Acquire);
        if origin == UNSET_STREAM_INSTANT_NANOS {
            return 0;
        }
        let now = u64::try_from(now.as_nanos()).unwrap_or(u64::MAX);
        now.saturating_sub(origin)
    }
}

impl std::fmt::Debug for RecordCapture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordCapture")
            .field("epoch", &self.epoch)
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl RecordCapture {
    fn state(&self) -> Result<u64, RecordCaptureError> {
        let state = self.tap.state.load(Ordering::Acquire);
        if state >> RECORD_EPOCH_SHIFT != self.epoch || state & RECORD_CLAIMED == 0 {
            return Err(RecordCaptureError::StaleCapture);
        }
        Ok(state)
    }

    /// Stop admitting blocks without waiting for an already admitted writer.
    pub fn request_close(&self) -> Result<(), RecordCaptureError> {
        self.state()?;
        // A live handle retains CLAIMED, so its epoch cannot change here.
        self.tap.close_current();
        Ok(())
    }

    /// True only after this epoch stopped admission and its writer retired.
    /// No further callback invocation is needed when no writer was admitted.
    pub fn is_closed(&self) -> bool {
        self.state()
            .is_ok_and(|state| state & (RECORD_OPEN | RECORD_IN_FLIGHT) == 0)
    }

    /// Append the published stereo sample prefix, including after closure.
    /// The return value counts samples, not frames. This does not locate gaps.
    pub fn drain(&mut self, out: &mut Vec<f32>) -> Result<usize, RecordCaptureError> {
        self.state()?;
        Ok(self.tap.drain_owned(out))
    }

    /// Cumulative whole-block losses in stereo frames for this capture.
    pub fn dropped_frames(&self) -> Result<u64, RecordCaptureError> {
        self.state()?;
        Ok(self.tap.dropped_frames())
    }
}

impl Drop for RecordCapture {
    fn drop(&mut self) {
        if self.state().is_ok() {
            // An admitted writer still retains IN_FLIGHT until its guard drops.
            self.tap
                .state
                .fetch_and(!(RECORD_OPEN | RECORD_CLAIMED), Ordering::AcqRel);
        }
    }
}

impl Drop for RecordWriteGuard<'_> {
    fn drop(&mut self) {
        self.tap
            .state
            .fetch_and(!RECORD_IN_FLIGHT, Ordering::Release);
    }
}

impl RecordTap {
    fn new() -> Self {
        Self {
            slots: crate::input::zeroed_atomics(RECORD_RING_CAPACITY),
            state: AtomicU64::new(0),
            written: std::sync::atomic::AtomicUsize::new(0),
            read: std::sync::atomic::AtomicUsize::new(0),
            dropped_frames: AtomicU64::new(0),
        }
    }

    fn open(&self, claimed: bool) -> Result<u64, RecordCaptureError> {
        let previous = self.state.load(Ordering::Acquire);
        if previous & RECORD_FLAGS != 0 {
            return Err(RecordCaptureError::Busy);
        }
        let epoch = (previous >> RECORD_EPOCH_SHIFT)
            .checked_add(1)
            .filter(|epoch| *epoch <= MAX_RECORD_EPOCH)
            .ok_or(RecordCaptureError::EpochExhausted)?;
        let reserved = (epoch << RECORD_EPOCH_SHIFT) | RECORD_CLAIMED;
        self.state
            .compare_exchange(previous, reserved, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RecordCaptureError::Busy)?;
        self.read
            .store(self.written.load(Ordering::Acquire), Ordering::Release);
        self.dropped_frames.store(0, Ordering::Relaxed);
        let owner = if claimed { RECORD_CLAIMED } else { 0 };
        self.state.store(
            (epoch << RECORD_EPOCH_SHIFT) | owner | RECORD_OPEN,
            Ordering::Release,
        );
        Ok(epoch)
    }

    fn capture(self: &Arc<Self>) -> Result<RecordCapture, RecordCaptureError> {
        let epoch = self.open(true)?;
        Ok(RecordCapture {
            tap: Arc::clone(self),
            epoch,
        })
    }

    fn close_current(&self) {
        self.state.fetch_and(!RECORD_OPEN, Ordering::AcqRel);
    }

    /// Legacy control cannot replace a claimed capture or reset an active writer.
    pub(super) fn set_enabled(&self, enabled: bool) -> Result<(), RecordCaptureError> {
        let state = self.state.load(Ordering::Acquire);
        if state & RECORD_CLAIMED != 0 {
            return Err(RecordCaptureError::Busy);
        }
        if enabled {
            if state & RECORD_OPEN == 0 {
                self.open(false)?;
            }
        } else {
            self.state
                .compare_exchange(
                    state,
                    state & !RECORD_OPEN,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .map_err(|_| RecordCaptureError::Busy)?;
        }
        Ok(())
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.state.load(Ordering::Acquire) & RECORD_OPEN != 0
    }

    fn admit(&self, observed: u64) -> Option<RecordWriteGuard<'_>> {
        if observed & RECORD_OPEN == 0 || observed & RECORD_IN_FLIGHT != 0 {
            return None;
        }
        self.state
            .compare_exchange(
                observed,
                observed | RECORD_IN_FLIGHT,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()?;
        Some(RecordWriteGuard { tap: self })
    }

    /// Callback side. Whole blocks or nothing, never a wait.
    pub(super) fn write_stereo(&self, stereo: &[f32]) {
        let observed = self.state.load(Ordering::Relaxed);
        if observed & RECORD_OPEN == 0 {
            return;
        }
        let Some(admission) = self.admit(observed) else {
            return;
        };
        admission.tap.write_admitted(stereo);
    }

    fn write_admitted(&self, stereo: &[f32]) {
        let written = self.written.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        let free = RECORD_RING_CAPACITY - written.wrapping_sub(read);
        if stereo.len() > free {
            self.dropped_frames
                .fetch_add((stereo.len() / 2) as u64, Ordering::Relaxed);
            return;
        }
        for (index, sample) in stereo.iter().enumerate() {
            self.slots[written.wrapping_add(index) % RECORD_RING_CAPACITY]
                .store(sample.to_bits(), Ordering::Relaxed);
        }
        self.written
            .store(written.wrapping_add(stereo.len()), Ordering::Release);
    }

    /// Reader side: append everything written since the last drain.
    fn drain(&self, out: &mut Vec<f32>) -> usize {
        if self.state.load(Ordering::Acquire) & RECORD_CLAIMED != 0 {
            return 0;
        }
        self.drain_owned(out)
    }

    fn drain_owned(&self, out: &mut Vec<f32>) -> usize {
        let read = self.read.load(Ordering::Relaxed);
        let written = self.written.load(Ordering::Acquire);
        let available = written.wrapping_sub(read);
        out.reserve(available);
        for index in 0..available {
            out.push(f32::from_bits(
                self.slots[read.wrapping_add(index) % RECORD_RING_CAPACITY].load(Ordering::Relaxed),
            ));
        }
        self.read
            .store(read.wrapping_add(available), Ordering::Release);
        available
    }

    fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// Bytes of the ring takes have reached: zeroed memory, resident only
    /// once written, and all of it once a take outlasts the ring.
    pub(super) fn touched_bytes(&self) -> usize {
        self.written
            .load(Ordering::Relaxed)
            .min(RECORD_RING_CAPACITY)
            * std::mem::size_of::<std::sync::atomic::AtomicU32>()
    }
}

impl LiveAnalysisTap {
    fn new() -> Self {
        Self {
            slots: [const { AtomicU64::new(0) }; LIVE_ANALYSIS_RING_CAPACITY],
            sides: None,
            enabled: AtomicBool::new(false),
            publication: AtomicU64::new(0),
            written: AtomicU64::new(0),
            valid_from: AtomicU64::new(0),
            end_frame: AtomicU64::new(0),
            epoch: AtomicU64::new(1),
        }
    }

    /// The master's tap, which keeps left and right as well.
    fn with_sides() -> Self {
        let mut tap = Self::new();
        tap.sides = Some(Box::new(
            [const { AtomicU64::new(0) }; LIVE_ANALYSIS_RING_CAPACITY],
        ));
        tap
    }

    fn set_enabled(&self, enabled: bool) {
        if !enabled {
            self.enabled.store(false, Ordering::Release);
            return;
        }
        if self.enabled.load(Ordering::Acquire) {
            return;
        }
        // Begin a fresh capture epoch without clearing slots concurrently
        // with the callback. `valid_from` excludes every pre-enable sample;
        // an in-flight writer from the prior epoch declines to publish.
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.valid_from
            .store(self.written.load(Ordering::Acquire), Ordering::Release);
        self.enabled.store(true, Ordering::Release);
    }

    /// Publish interleaved stereo scratch as mono. Callback-only: fixed
    /// atomics and arithmetic, with no allocation, lock, or syscall.
    pub(super) fn write_stereo(&self, stereo: &[f32], end_frame: u64) {
        self.write_stereo_scaled(stereo, end_frame, 1.0);
    }

    fn write_stereo_scaled(&self, stereo: &[f32], end_frame: u64, gain: f32) {
        // Visual telemetry is explicitly opt-in. Ordinary playback pays one
        // predictable branch per 128-frame DSP block and no analysis stores.
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        let frames = stereo.len() / 2;
        let publication = self.publication.load(Ordering::Relaxed) & !1;
        self.publication
            .store(publication.wrapping_add(1), Ordering::Release);
        let start = self.written.load(Ordering::Relaxed);
        for frame in 0..frames {
            let sequence = start.wrapping_add(frame as u64);
            let slot = sequence as usize % LIVE_ANALYSIS_RING_CAPACITY;
            let lap = sequence / LIVE_ANALYSIS_RING_CAPACITY as u64;
            if let Some(sides) = &self.sides {
                let left = stereo[frame * 2] * gain;
                let right = stereo[frame * 2 + 1] * gain;
                sides[slot].store(
                    (u64::from(left.to_bits()) << 32) | u64::from(right.to_bits()),
                    Ordering::Relaxed,
                );
            }
            let mono = (stereo[frame * 2] + stereo[frame * 2 + 1]) * (0.5 * gain);
            let packed = ((lap.wrapping_add(1) as u32 as u64) << 32) | u64::from(mono.to_bits());
            self.slots[slot].store(packed, Ordering::Release);
        }
        if self.enabled.load(Ordering::Acquire) && self.epoch.load(Ordering::Acquire) == epoch {
            self.written
                .store(start.wrapping_add(frames as u64), Ordering::Relaxed);
            self.end_frame.store(end_frame, Ordering::Relaxed);
        }
        self.publication
            .store(publication.wrapping_add(2), Ordering::Release);
    }

    /// Copy the newest frames as left and right, oldest first, for a
    /// stereo picture. Unlike [`Self::copy_latest`] this reader checks no
    /// laps: a frame the callback overwrites mid-copy is a newer frame in
    /// the same place, which a scope shows as well as the old one. The
    /// prefix is silence before the ring fills. False when the tap keeps
    /// no sides or has nothing yet.
    fn copy_latest_sides(&self, output: &mut [(f32, f32); LIVE_ANALYSIS_SIDES_SAMPLES]) -> bool {
        let Some(sides) = &self.sides else {
            return false;
        };
        if !self.enabled.load(Ordering::Acquire) {
            return false;
        }
        let end = self.written.load(Ordering::Acquire);
        let valid_from = self.valid_from.load(Ordering::Acquire);
        let available = end
            .saturating_sub(valid_from)
            .min(LIVE_ANALYSIS_SIDES_SAMPLES as u64) as usize;
        if available == 0 {
            return false;
        }
        let padding = LIVE_ANALYSIS_SIDES_SAMPLES - available;
        output[..padding].fill((0.0, 0.0));
        let start = end.wrapping_sub(available as u64);
        for (offset, frame) in output[padding..].iter_mut().enumerate() {
            let slot = start.wrapping_add(offset as u64) as usize % LIVE_ANALYSIS_RING_CAPACITY;
            let packed = sides[slot].load(Ordering::Relaxed);
            *frame = (
                f32::from_bits((packed >> 32) as u32),
                f32::from_bits(packed as u32),
            );
        }
        true
    }

    /// Copy the newest complete window in chronological order. Before the
    /// window fills, the missing prefix is silence. A reader makes only a few
    /// attempts if the callback laps a required slot; the callback itself
    /// remains wait-free.
    fn copy_latest(
        &self,
        output: &mut [f32; LIVE_ANALYSIS_WINDOW_SAMPLES],
    ) -> Option<AnalysisTapSnapshot> {
        if !self.enabled.load(Ordering::Acquire) {
            return None;
        }
        // Never let editor telemetry monopolise the scheduling producer if a
        // very small callback buffer repeatedly laps this reader.
        for _ in 0..3 {
            let publication = self.publication.load(Ordering::Acquire);
            if publication & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let end = self.written.load(Ordering::Acquire);
            let valid_from = self.valid_from.load(Ordering::Acquire);
            let end_frame = self.end_frame.load(Ordering::Relaxed);
            let epoch = self.epoch.load(Ordering::Relaxed);
            let available = end
                .saturating_sub(valid_from)
                .min(LIVE_ANALYSIS_WINDOW_SAMPLES as u64) as usize;
            if available == 0 {
                return None;
            }
            let padding = LIVE_ANALYSIS_WINDOW_SAMPLES - available;
            output[..padding].fill(0.0);
            let start = end.wrapping_sub(available as u64);
            let mut complete = true;
            for offset in 0..available {
                let sequence = start.wrapping_add(offset as u64);
                let slot = sequence as usize % LIVE_ANALYSIS_RING_CAPACITY;
                let expected_lap =
                    (sequence / LIVE_ANALYSIS_RING_CAPACITY as u64).wrapping_add(1) as u32;
                let packed = self.slots[slot].load(Ordering::Acquire);
                if (packed >> 32) as u32 != expected_lap {
                    complete = false;
                    break;
                }
                output[padding + offset] = f32::from_bits(packed as u32);
            }
            if complete && self.publication.load(Ordering::Acquire) == publication {
                return Some(AnalysisTapSnapshot { epoch, end_frame });
            }
            std::hint::spin_loop();
        }
        None
    }

    fn reset(&self) {
        for slot in &self.slots {
            slot.store(0, Ordering::Relaxed);
        }
        self.written.store(0, Ordering::Release);
        self.valid_from.store(0, Ordering::Release);
        self.end_frame.store(0, Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

impl LiveScalarDevice {
    /// Post-fader master level, read and reset. See [`crate::meter`].
    pub fn take_levels(&self) -> crate::meter::MasterLevels {
        self.shared.meter.take()
    }

    /// Remaining score sources and largest orbit peak since the last take.
    /// Excludes the independent computer piano bus, including its held voices.
    pub fn take_score_activity(&self) -> (bool, f32) {
        (
            self.shared.score_sources_active.load(Ordering::Acquire),
            f32::from_bits(self.shared.score_peak.swap(0, Ordering::AcqRel)),
        )
    }

    /// The safety limiter's ceiling and character, or `None` for off. Off
    /// still refuses to hand the device a sample past full scale.
    pub fn set_limiter(&self, settings: Option<crate::meter::LimiterSettings>) {
        self.shared.meter.set_limiter(settings);
    }

    /// Whether the limiter's ceiling is brought back up to full scale.
    pub fn set_limiter_makeup(&self, on: bool) {
        self.shared.meter.set_limiter_makeup(on);
    }

    /// What the callback will read on its next block. The producer side
    /// needs this to tell "nobody set it" from "it was set and did
    /// nothing".
    pub fn limiter(&self) -> Option<crate::meter::LimiterSettings> {
        self.shared.meter.limiter_settings()
    }

    /// Set the linear master gain applied to the final mix.
    ///
    /// This is an engine control, not a score control: it multiplies the mix
    /// on its way to the device and never changes what the score evaluates to.
    pub fn set_master_gain(&self, gain: f32) {
        self.shared.meter.set_gain(gain);
    }

    /// Submit one exact continuous-control target. The sole device owner
    /// produces these POD messages; the callback drains them without locks.
    /// A full ring leaves the newest value with the engine for its next tick.
    pub fn try_set_live_control(&mut self, update: crate::live_control::LiveControlUpdate) -> bool {
        self.shared.live_controls.push(update).is_ok()
    }

    /// Requested voice limit, adopted by the callback at its next block edge.
    pub fn max_polyphony(&self) -> usize {
        self.shared.max_polyphony.load(Ordering::Relaxed)
    }

    /// Raising the limit permits more simultaneous sounds and more CPU work.
    /// Lowering it fades excess voices without restarting the stream.
    pub fn set_max_polyphony(&self, limit: usize) {
        self.shared.max_polyphony.store(
            limit.clamp(1, crate::MAX_CONFIGURABLE_POLYPHONY),
            Ordering::Relaxed,
        );
    }

    pub fn master_gain(&self) -> f32 {
        self.shared.meter.gain()
    }

    /// One orbit's fader, linear; 1 is unity. An engine control like the
    /// master: it scales the orbit on its way to the sum and never changes
    /// what the score evaluates to.
    pub fn set_orbit_gain(&self, orbit: usize, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 4.0)
        } else {
            1.0
        };
        if let Some(slot) = self.shared.orbit_gains.get(orbit) {
            slot.store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn orbit_gain(&self, orbit: usize) -> f32 {
        self.shared
            .orbit_gains
            .get(orbit)
            .map_or(1.0, |slot| f32::from_bits(slot.load(Ordering::Relaxed)))
    }

    /// The audio input's fader, applied as the frames land.
    pub fn set_input_gain(&self, gain: f32) {
        self.shared.input.set_gain(gain);
    }

    pub fn input_gain(&self) -> f32 {
        self.shared.input.gain()
    }

    /// The lag a reader of the input is placed at, in frames: a fixed few
    /// milliseconds, or twice the largest delivery so far when the driver
    /// writes in bursts. This is the number a readout owes the player.
    pub fn input_read_lag_frames(&self) -> u64 {
        self.shared.input.read_lag()
    }

    /// Enable or disable the mix telemetry tap before master gain and limiting.
    /// It is disabled by default so ordinary CLI playback performs no per-sample
    /// atomic stores.
    pub fn set_analysis_enabled(&self, enabled: bool) {
        self.shared.analysis.set_enabled(enabled);
    }

    pub fn set_visual_analysis_mask(&self, mask: u64) {
        self.shared.set_visual_analysis_mask(mask);
    }

    pub fn reset_visual_analysis_mask(&self, mask: u64) {
        self.shared.reset_visual_analysis_mask(mask);
    }

    /// Copy the newest mono samples before master gain and limiting, oldest to
    /// newest. A newly started stream is left-padded with silence until its first full window
    /// has been written. A bounded collision with the callback returns `None`
    /// so UI work can simply try again on its next frame.
    ///
    /// This is producer-side telemetry: it performs no callback work and
    /// allocates no storage because the caller owns the fixed output array.
    pub fn copy_analysis_window(
        &self,
        output: &mut [f32; LIVE_ANALYSIS_WINDOW_SAMPLES],
    ) -> Option<LiveAnalysisSnapshot> {
        let snapshot = self.shared.analysis.copy_latest(output)?;
        Some(LiveAnalysisSnapshot {
            stream_id: self.stream_id,
            epoch: snapshot.epoch,
            end_frame: snapshot.end_frame,
            sample_rate: self.sample_rate,
        })
    }

    /// The newest frames of the mix as left and right, oldest first, for
    /// a stereo picture; false with nothing to show.
    pub fn copy_analysis_sides(
        &self,
        output: &mut [(f32, f32); LIVE_ANALYSIS_SIDES_SAMPLES],
    ) -> bool {
        self.shared.analysis.copy_latest_sides(output)
    }

    pub fn copy_visual_analysis_window(
        &self,
        slot: u8,
        output: &mut [f32; LIVE_ANALYSIS_WINDOW_SAMPLES],
    ) -> Option<LiveAnalysisSnapshot> {
        let tap = self.shared.visual_analysis_tap(usize::from(slot))?;
        let snapshot = tap.copy_latest(output)?;
        Some(LiveAnalysisSnapshot {
            stream_id: self.stream_id,
            epoch: snapshot.epoch,
            end_frame: snapshot.end_frame,
            sample_rate: self.sample_rate,
        })
    }

    /// The loudest sample the input has delivered since the last take, for
    /// a meter; 0 while none is open or nothing has come in.
    pub fn take_input_peak(&self) -> f32 {
        self.shared.input.take_peak()
    }

    /// Each orbit's highest absolute sample, post-fader, since the last
    /// take: the orbit meters.
    pub fn take_orbit_levels(&self) -> [f32; crate::scalar::MAX_ORBITS] {
        let mut levels = [0.0f32; crate::scalar::MAX_ORBITS];
        for (level, slot) in levels.iter_mut().zip(self.shared.orbit_peaks.iter()) {
            *level = f32::from_bits(slot.swap(0, Ordering::AcqRel));
        }
        levels
    }

    /// Send an orbit to an output pair (0 is the main pair). Takes effect
    /// at the next block; a pair the device does not have is the main pair.
    pub fn set_orbit_output(&self, orbit: usize, pair: u8) {
        let orbit = orbit.min(crate::scalar::MAX_ORBITS - 1);
        self.shared.orbit_pairs[orbit].store(pair, Ordering::Relaxed);
    }

    pub fn orbit_outputs(&self) -> [u8; crate::scalar::MAX_ORBITS] {
        let mut pairs = [0u8; crate::scalar::MAX_ORBITS];
        for (slot, pair) in pairs.iter_mut().zip(self.shared.orbit_pairs.iter()) {
            *slot = pair.load(Ordering::Relaxed);
        }
        pairs
    }

    /// Stereo pairs the output has: 1 for a stereo device.
    pub fn output_pairs(&self) -> u16 {
        (self.channels / 2).max(1)
    }

    /// Claim a new recording epoch. A closed capture remains busy until its
    /// handle is dropped, leaving its final PCM available for that reader.
    pub fn start_recording_capture(&self) -> Result<RecordCapture, RecordCaptureError> {
        self.shared.record.capture()
    }

    /// Whether this output uses the tap retained by the capture. This does
    /// not claim the capture is open or that the sample rate is unchanged.
    pub fn owns_record_capture(&self, capture: &RecordCapture) -> bool {
        Arc::ptr_eq(&self.shared.record, &capture.tap)
    }

    /// Legacy recording control. Starting discards the prior unclaimed epoch;
    /// a new epoch returns Busy while a capture or retiring writer owns it.
    /// Repeated enabling is idempotent. Successful disabling does not wait for
    /// an admitted writer; use RecordCapture for a complete final drain.
    ///
    /// Legacy control and draining belong to one producer/reader and must not
    /// race each other or `start_recording_capture`.
    pub fn set_recording_enabled(&self, enabled: bool) -> Result<(), RecordCaptureError> {
        self.shared.record.set_enabled(enabled)
    }

    pub fn recording_enabled(&self) -> bool {
        self.shared.record.is_enabled()
    }

    /// Take everything the callback has queued since the last drain:
    /// interleaved stereo, post-fader, at [`Self::sample_rate`]. Producer
    /// side; never blocks the callback. Returns the number of f32 SAMPLES
    /// appended (two per stereo frame), not frames. Returns zero while a
    /// RecordCapture owns the tap; that handle is then its only reader.
    /// The legacy producer must serialize this with recording control/start.
    pub fn drain_recording(&self, out: &mut Vec<f32>) -> usize {
        self.shared.record.drain(out)
    }

    /// Stereo FRAMES the callback had to drop because the recorder fell
    /// behind (contrast the sample count from `drain_recording`).
    pub fn recording_dropped_frames(&self) -> u64 {
        self.shared.record.dropped_frames()
    }

    pub fn clock_seconds(&self) -> f64 {
        self.clock_nanos() as f64 / 1_000_000_000.0
    }

    /// Callbacks completed so far. Bumped as each one exits, after it has
    /// drained the install ring at its entry and rendered its block - so
    /// two more than the count read after a push proves one whole block
    /// began after the push, took the baton at its top, and rendered with
    /// the slot empty.
    pub fn callbacks(&self) -> u64 {
        self.shared.callbacks.load(Ordering::Acquire)
    }

    /// Exact render-frontier frame used by callback generation cutovers.
    ///
    /// Unlike converting [`Self::clock_nanos`] back into frames, this cannot
    /// lose one frame to two integer divisions at non-divisible sample rates.
    /// Zero before the first callback is deliberately conservative.
    pub fn clock_frames(&self) -> u64 {
        self.shared.clock_frames()
    }

    /// End of the latest completed DSP chunk that admitted scheduled events.
    /// Holds during Stop and its release ramp, even as the output clock runs.
    /// This tracks event intake, not host copies or audible playback.
    pub fn completed_intake_frames(&self) -> u64 {
        self.shared.completed_intake_frame.load(Ordering::Acquire)
    }

    /// Render frontier including the DSP chunk currently being written.
    ///
    /// Use for replacement freshness checks, not playback/confirmation
    /// reporting: the announced chunk has not necessarily reached its host
    /// slice yet. The callback checks generation again for each subsequent
    /// chunk, including within an oversized host callback. This observation
    /// does not make generation publication atomic with rendering; the
    /// existing single-chunk cutover race remains bounded as before. Zero
    /// before the first callback, rather than a future playback timestamp.
    pub fn render_frontier_seconds(&self) -> f64 {
        self.render_frontier_frames() as f64 / f64::from(self.sample_rate.max(1))
    }

    /// [`Self::render_frontier_seconds`] in frames: the end of the block
    /// the callback is rendering, or the clock when none is.
    pub fn render_frontier_frames(&self) -> u64 {
        self.shared.render_frontier_frames()
    }

    /// Render-frontier playhead: frames we have actually handed the device.
    ///
    /// WSLg's RDP sink reports jumpy CPAL playback timestamps (stall, then a
    /// leap). Using those as the live DSP/event clock can cluster onsets into
    /// the current callback after an underrun. Submitted frames are monotonic
    /// in the callback, so a kick
    /// grid stays even in the PCM we emit even when the sink's clock is not.
    /// Before the first callback, fall back to the stream timestamp so start
    /// prefetch still has a playhead.
    pub fn clock_nanos(&self) -> u64 {
        let frames = self.shared.frames.load(Ordering::Acquire);
        if frames == 0 {
            if let Some(stream) = &self.stream {
                return self.shared.playhead_at(stream.now());
            }
            return 0;
        }
        let sample_rate = u64::from(self.sample_rate.max(1));
        (u128::from(frames) * 1_000_000_000 / u128::from(sample_rate)) as u64
    }

    /// Pipeline depth used for startup schedule lead.
    ///
    /// The live clock is submitted frames (the render frontier), not Pulse's
    /// audible timestamp. Extra Pulse delay is already-rendered PCM sitting in
    /// the host and must not widen the query window, so the depth is capped at
    /// a few buffers.
    fn pipeline_latency_seconds(&self) -> f64 {
        let buffer_secs =
            f64::from(self.requested_buffer_frames) / f64::from(self.sample_rate.max(1));
        let latency_nanos = self.shared.playback_latency_nanos.load(Ordering::Acquire);
        let raw = if latency_nanos == 0 {
            3.0 * buffer_secs
        } else {
            latency_nanos as f64 / 1e9
        };
        let cap = (4.0 * buffer_secs).max(0.20);
        raw.min(cap)
    }

    /// How far ahead of the render-frontier clock an event must be targeted.
    /// Before the first callback, a conservative three-buffer estimate
    /// stands in. (Telemetry: events inside this window count as
    /// `late_events` - the signature of every start/reload tick.)
    pub fn schedule_lead_seconds(&self) -> f64 {
        let buffer_secs =
            f64::from(self.requested_buffer_frames) / f64::from(self.sample_rate.max(1));
        let lead = self.pipeline_latency_seconds() + 2.0 * buffer_secs;
        // WSLg's RDP sink can deepen its buffer during the first seconds of a
        // stream, so a lead taken at startup may undershoot the growing render
        // frontier. Floor the lead there.
        if std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::env::var_os("WSL_INTEROP").is_some()
        {
            lead.max(0.35)
        } else {
            lead
        }
    }

    /// Replacement reserve relative to the completed-frame clock: in-flight
    /// rendering plus one requested buffer. Downstream playback latency is
    /// already-rendered PCM and must not be added to this boundary again.
    ///
    /// Startup still uses [`Self::schedule_lead_seconds`]. Candidate
    /// preparation headroom belongs to the producer; it must also recheck
    /// [`Self::render_frontier_seconds`] before publishing a replacement.
    /// Existing ringing/pre-onset voice semantics are unchanged here.
    pub fn continuity_margin_seconds(&self) -> f64 {
        let completed = self.shared.clock_frames();
        let rendering_until = self.shared.rendering_until_frame.load(Ordering::Acquire);
        let reserve_frames = rendering_until
            .saturating_sub(completed)
            .saturating_add(u64::from(self.requested_buffer_frames));
        reserve_frames as f64 / f64::from(self.sample_rate.max(1))
    }

    /// Silence everything sounding, over the choke ramp.
    ///
    /// For swapping an audition: the snippet being replaced has to stop
    /// rather than ring on under the one that was asked for. The score's
    /// own reloads leave voices alone, as they must.
    pub fn cut_sounding(&self) {
        self.shared.cut_sounding.store(true, Ordering::Release);
    }

    /// Publish a replacement generation with its takeover frame (where its
    /// re-query cursor starts) and its cut intent.
    ///
    /// `cut` selects where the consumer silences the voices that sound
    /// under the new generation. [`TakeoverCut::AtFlip`] cuts at the
    /// consumer's flip (an immediate rewind). [`TakeoverCut::AtTakeover`]
    /// cuts at the takeover frame (a quantised rewind): the old score plays
    /// its countdown to the line, then the restarted loop sounds alone. An
    /// ordinary reload passes [`TakeoverCut::None`]: the old voices ring
    /// out, so a live edit does not click.
    pub fn set_generation(&self, generation: u64, takeover_frame: u64, cut: TakeoverCut) {
        self.shared
            .takeover_frame
            .store(takeover_frame, Ordering::Release);
        self.shared
            .takeover_cut
            .store(cut as u64, Ordering::Release);
        // A published flip supersedes any line arm: from here the flip's
        // own cut intent and takeover frame carry the handoff, and a stale
        // arm from a cancelled launch must not cut a later generation's
        // line that never asked for one.
        self.shared.line_arm.store(0, Ordering::Release);
        self.shared.generation.store(generation, Ordering::Release);
    }

    /// Pre-arm a cut at `frame`, the line of a quantised launch, before the
    /// generation that takes over there exists. The consumer silences the
    /// outgoing rendition at the frame, whenever the incoming generation is
    /// published. An evaluation that outlasts the countdown then cannot let
    /// the old score play through the line and sound its first beat twice.
    /// With `drop_from` set the consumer also refuses outgoing ring events
    /// at or after the frame (the restart's ghost window), so late
    /// countdown onsets never sound under the new loop.
    ///
    /// Cleared by the next [`Self::set_generation`] flip (whose own cut
    /// intent supersedes) or [`Self::clear_line_arm`]. An armed frame in
    /// the past still cuts: a launch whose evaluation overran the line
    /// asked for exactly that.
    pub fn arm_line_cut(&self, frame: u64, drop_from: bool) {
        let word = (frame << 2) | 0b01 | (u64::from(drop_from) << 1);
        self.shared.line_arm.store(word, Ordering::Release);
    }

    /// Withdraw a pre-armed line cut: the launch was cancelled or its
    /// evaluation failed, and the outgoing rendition keeps playing whole.
    /// Leaves [`crate::LINE_ARM_WITHDRAWN`] rather than zero so a consumer
    /// whose arm already fired knows no flip is coming and lets it go.
    pub fn clear_line_arm(&self) {
        self.shared
            .line_arm
            .store(crate::LINE_ARM_WITHDRAWN, Ordering::Release);
    }

    /// The raw armed line word: a test and telemetry probe of exactly what
    /// [`Self::arm_line_cut`] stored, [`Self::clear_line_arm`] leaves and a
    /// published flip clears. Bit 0 is the arm; see [`Self::line_cut_armed`].
    pub fn armed_line_word(&self) -> u64 {
        self.shared.line_arm.load(Ordering::Acquire)
    }

    /// Whether a line cut is armed on the consumer.
    pub fn line_cut_armed(&self) -> bool {
        self.armed_line_frame().is_some()
    }

    /// The frame a line cut is armed at, while one is. The consumer fires it
    /// in the first block that ends past that frame.
    pub fn armed_line_frame(&self) -> Option<u64> {
        let word = self.armed_line_word();
        (word & 0b01 != 0).then_some(word >> 2)
    }

    /// Generation currently published to the audio consumer.
    ///
    /// This is cheaper than building a complete device report and is the
    /// authoritative boundary for clients that must describe what can
    /// actually be heard rather than a replacement still being prefetched by
    /// the producer.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Play a helper note at the next callback boundary, independently of
    /// the score producer. Its target frame is intentionally ignored.
    pub fn push_immediate(&self, event: crate::AudioEvent) -> bool {
        self.shared.immediate.push(event)
    }

    /// False is back-pressure: the producer must retain and retry this event.
    pub fn push(&self, event: impl Into<crate::QueuedAudioEvent>) -> bool {
        self.shared.ring.push_queued(event.into())
    }

    /// Producer endpoint for finite prefill-window host-copy confirmations.
    pub fn confirmations(&self) -> crate::confirmation::ConfirmationChannel {
        self.shared.confirmations.clone()
    }

    pub fn stop(&self) {
        self.shared.request_stop();
    }

    /// Whether a requested stop is still waiting for its callback fade,
    /// including a resume that must finish that fade first.
    pub fn stop_pending(&self) -> bool {
        matches!(
            self.shared.stop.0.load(Ordering::Acquire),
            OUTPUT_STOP_REQUESTED | OUTPUT_RESUME_PENDING
        )
    }

    /// Request silence and wait until a data callback has rendered the whole
    /// stop ramp.
    ///
    /// This is an off-callback wait used by the product's graceful Stop path;
    /// it does not infer when already-submitted host buffers reach speakers.
    pub fn stop_and_wait(&self, timeout: Duration) -> bool {
        self.stop();
        let deadline = Instant::now() + timeout;
        while !self.shared.stop.acknowledged() {
            if Instant::now() >= deadline || self.shared.failed.load(Ordering::Acquire) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // DRAIN before teardown: acknowledgement only means the declick fade
        // was RENDERED - the sink still holds up to a playback-latency of
        // queued audio, and closing the stream then discards it mid-sample,
        // producing a stop click. Let the faded tail actually
        // reach the speaker, bounded so a stuck sink cannot hang a stop.
        let latency = self.shared.playback_latency_nanos.load(Ordering::Acquire);
        let buffer_nanos = u64::from(self.requested_buffer_frames) * 1_000_000_000
            / u64::from(self.sample_rate.max(1));
        let drain = Duration::from_nanos((latency + 2 * buffer_nanos).min(1_500_000_000));
        std::thread::sleep(drain);
        true
    }

    /// Resume this stream after its stop ramp completes. An already-running
    /// output keeps its voices. Events queued after resume wait for the ramp.
    /// A stopped output keeps the inserts of its chains, and each chain is
    /// silent until its next note.
    pub fn resume(&self) {
        self.shared.stop.resume();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;
    mod analysis {
        use super::*;

        #[test]
        fn master_fader_moves_smoothly_on_every_output_pair() {
            let _tripwire_guard = tripwire_assertion_guard();
            for sample_rate in [44_100, 96_000] {
                for channels in [2, 4] {
                    let render = |chunk: usize| {
                        let shared = LiveShared::new(1);
                        shared.meter.set_gain(0.5);
                        if channels == 4 {
                            shared.orbit_pairs[1].store(1, Ordering::Relaxed);
                        }
                        for orbit in [0, 1] {
                            assert!(shared.ring.push(AudioEvent {
                                onset_id: orbit as u64 + 1,
                                generation: 1,
                                target_frame: 0,
                                onset_lead: 0.0,
                                freq_hz: 0.0,
                                gain: 0.1,
                                duration_secs: 2.0,
                                ui_visuals: 0,
                                controls: crate::OscillatorControls {
                                    orbit,
                                    envelope: crate::Envelope {
                                        attack_secs: 0.001,
                                        decay_secs: 0.0,
                                        sustain: 1.0,
                                        release_secs: 0.01,
                                    },
                                    ..Default::default()
                                },
                                sample: None,
                                synth: Some(crate::SynthSource::One),
                                wavetable: None,
                                cut: None,
                            }));
                        }
                        let mut backend = LiveScalarBackend::with_dispatch(
                            sample_rate,
                            4,
                            DspDispatch::portable(),
                        )
                        .expect("portable backend");
                        let mut meters =
                            LiveCallbackMeters::new(sample_rate, &shared.realtime_load);
                        let mut output = [0.0f32; 1024 * 4];
                        let mut render = |frames: usize, output: &mut [f32]| {
                            let before = tripwire::Violations::capture();
                            tripwire::audio_scope(|| {
                                write_live_output(
                                    &mut backend,
                                    &mut meters,
                                    &mut output[..frames * channels],
                                    channels,
                                    &shared,
                                );
                            });
                            assert!(tripwire::Violations::capture().since(before).clean());
                        };
                        render(1024, &mut output);
                        let mut previous = output[1023 * channels];
                        assert!(previous > 0.01);
                        let unity = previous * 2.0;
                        let ramp_frames = (sample_rate as f32 * 0.01).round() as usize;
                        let mut rendered = Vec::new();
                        for (target, frames) in [
                            (0.0, ramp_frames * 2),
                            (4.0, ramp_frames * 2),
                            (0.2, ramp_frames / 3),
                            (3.0, ramp_frames / 4),
                            (1.0, ramp_frames * 2),
                        ] {
                            shared.meter.set_gain(target);
                            let mut remaining = frames;
                            while remaining != 0 {
                                let count = remaining.min(chunk);
                                render(count, &mut output);
                                for frame in output[..count * channels].chunks_exact(channels) {
                                    assert!(frame[0].is_finite());
                                    assert!((frame[0] - previous).abs() < 0.01);
                                    assert!(frame.iter().all(|sample| *sample == frame[0]));
                                    previous = frame[0];
                                    rendered.push(previous);
                                }
                                remaining -= count;
                            }
                            if frames >= ramp_frames {
                                assert!((previous - unity * target).abs() < 1e-6);
                            }
                        }
                        assert_eq!(shared.callback_scope_misses.load(Ordering::Relaxed), 0);
                        rendered
                    };
                    let reference = render(1);
                    for chunk in [73, 1024] {
                        assert_eq!(render(chunk), reference);
                    }
                }
            }
        }

        #[test]
        fn master_volume_changes_output_without_changing_visuals() {
            let _tripwire_guard = tripwire_assertion_guard();
            let render = |gain| {
                let shared = LiveShared::new(1);
                shared.analysis.set_enabled(true);
                shared.set_visual_analysis_mask(1);
                shared.meter.set_gain(gain);
                assert!(shared.ring.push(AudioEvent {
                    onset_id: 1,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 440.0,
                    gain: 0.8,
                    duration_secs: 0.1,
                    ui_visuals: 1,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
                let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
                let mut output = [0.0f32; 128 * 2];
                let mut meters = test_meter();
                let before = tripwire::Violations::capture();
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert!(tripwire::Violations::capture().since(before).clean());
                let mut master = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
                let mut visual = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
                let mut sides = [(0.0, 0.0); LIVE_ANALYSIS_SIDES_SAMPLES];
                shared.analysis.copy_latest(&mut master).expect("master");
                assert!(shared.analysis.copy_latest_sides(&mut sides));
                shared
                    .visual_analysis_tap(0)
                    .expect("visual tap")
                    .copy_latest(&mut visual)
                    .expect("visual");
                (output, master, visual, sides)
            };

            let (reference, master, visual, sides) = render(1.0);
            assert!(reference.iter().any(|sample| sample.abs() > 1e-5));
            assert!(master.iter().any(|sample| sample.abs() > 1e-5));
            assert!(visual.iter().any(|sample| sample.abs() > 1e-5));
            for gain in [0.0, 0.25, 4.0] {
                let (output, actual_master, actual_visual, actual_sides) = render(gain);
                assert_eq!(actual_master, master, "master visual at gain {gain}");
                assert_eq!(actual_visual, visual, "receiver visual at gain {gain}");
                assert_eq!(actual_sides, sides, "stereo visual at gain {gain}");
                for (actual, expected) in output.iter().zip(reference) {
                    assert_eq!(*actual, (expected * gain).clamp(-1.0, 1.0));
                }
            }
        }

        #[test]
        fn live_analysis_window_is_zero_padded_post_mix_mono() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            shared.analysis.set_enabled(true);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.8,
                duration_secs: 0.1,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            assert!(tripwire::allocator_is_armed());
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            let violations = tripwire::Violations::capture().since(before);
            assert!(
                violations.clean(),
                "analysis tap violated callback allocation contract: {violations:?}"
            );

            let mut analysis = [f32::NAN; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let snapshot = shared
                .analysis
                .copy_latest(&mut analysis)
                .expect("published analysis");
            assert_eq!(snapshot.end_frame, 128);
            assert!(
                analysis[..LIVE_ANALYSIS_WINDOW_SAMPLES - 128]
                    .iter()
                    .all(|sample| sample.to_bits() == 0.0f32.to_bits())
            );
            for frame in 0..128 {
                let expected = (output[frame * 2] + output[frame * 2 + 1]) * 0.5;
                assert_eq!(
                    analysis[LIVE_ANALYSIS_WINDOW_SAMPLES - 128 + frame].to_bits(),
                    expected.to_bits(),
                    "analysis diverged from final stereo mix at frame {frame}"
                );
            }
        }

        #[test]
        fn live_visual_analysis_keeps_sibling_receivers_out_of_each_other() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            shared.analysis.set_enabled(true);
            shared.set_visual_analysis_mask(0b11);
            for (onset_id, frequency, ui_visuals) in [(1, 220.0, 0b01), (2, 440.0, 0b10)] {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: frequency,
                    gain: 0.4,
                    duration_secs: 0.1,
                    ui_visuals,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
            }
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            assert!(
                tripwire::Violations::capture().since(before).clean(),
                "per-visual capture allocated in the callback"
            );

            let mut master = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let mut first = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let mut second = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            shared.analysis.copy_latest(&mut master).expect("master");
            shared
                .visual_analysis_tap(0)
                .expect("first tap")
                .copy_latest(&mut first)
                .expect("first visual");
            shared
                .visual_analysis_tap(1)
                .expect("second tap")
                .copy_latest(&mut second)
                .expect("second visual");
            assert!(first.iter().any(|sample| sample.abs() > 1e-5));
            assert!(second.iter().any(|sample| sample.abs() > 1e-5));
            for ((master, first), second) in master.iter().zip(first).zip(second) {
                assert!((master - first - second).abs() < 1e-6);
            }
        }

        #[test]
        fn visual_analysis_taps_are_allocated_only_for_requested_slots() {
            let shared = LiveShared::new(1);
            assert!(shared.visual_analysis.iter().all(|tap| tap.get().is_none()));

            shared.set_visual_analysis_mask(1_u64 << 7);
            assert_eq!(
                shared
                    .visual_analysis
                    .iter()
                    .filter(|tap| tap.get().is_some())
                    .count(),
                1
            );
            assert!(shared.visual_analysis_tap(7).is_some());
        }

        #[test]
        fn reload_does_not_publish_a_ringing_old_voice_into_the_new_visual() {
            let shared = LiveShared::new(1);
            shared.analysis.set_enabled(true);
            shared.set_visual_analysis_mask(1);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 1.0,
                ui_visuals: 1,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            assert!(output.iter().any(|sample| sample.abs() > 1e-5));

            shared.takeover_frame.store(128, Ordering::Release);
            shared.generation.store(2, Ordering::Release);
            shared.reset_visual_analysis_mask(1);
            output.fill(0.0);
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            assert!(
                output.iter().any(|sample| sample.abs() > 1e-5),
                "the old voice should still ring in the master mix"
            );

            let mut visual = [f32::NAN; LIVE_ANALYSIS_WINDOW_SAMPLES];
            shared
                .visual_analysis_tap(0)
                .expect("visual tap")
                .copy_latest(&mut visual)
                .expect("new-generation visual frame");
            assert!(
                visual.iter().all(|sample| *sample == 0.0),
                "the old generation leaked into the new receiver"
            );
        }

        #[test]
        fn unchanged_visual_revision_keeps_its_ringing_voice_across_generation() {
            let shared = LiveShared::new(1);
            shared.set_visual_analysis_mask(1);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 1.0,
                ui_visuals: 1,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });

            shared.takeover_frame.store(128, Ordering::Release);
            shared.generation.store(2, Ordering::Release);
            output.fill(0.0);
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });

            let mut visual = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            shared
                .visual_analysis_tap(0)
                .expect("visual tap")
                .copy_latest(&mut visual)
                .expect("same-revision visual frame");
            assert!(
                visual[LIVE_ANALYSIS_WINDOW_SAMPLES - 128..]
                    .iter()
                    .any(|sample| sample.abs() > 1e-5),
                "a generation-only cutover discarded the same receiver's tail"
            );
        }

        #[test]
        fn live_analysis_window_survives_ring_wrap_and_keeps_latest_frames() {
            let shared = LiveShared::new(1);
            shared.analysis.set_enabled(true);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.7,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut expected = [0.0f32; LIVE_ANALYSIS_WINDOW_SAMPLES];
            for _ in 0..(LIVE_ANALYSIS_RING_CAPACITY / 128 + 4) {
                let mut output = [0.0f32; 128 * 2];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
                });
                expected.rotate_left(128);
                for frame in 0..128 {
                    expected[LIVE_ANALYSIS_WINDOW_SAMPLES - 128 + frame] =
                        (output[frame * 2] + output[frame * 2 + 1]) * 0.5;
                }
            }

            let mut analysis = [0.0f32; LIVE_ANALYSIS_WINDOW_SAMPLES];
            shared
                .analysis
                .copy_latest(&mut analysis)
                .expect("published analysis");
            for (index, (actual, expected)) in analysis.iter().zip(expected).enumerate() {
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "wrapped analysis diverged at sample {index}"
                );
            }
        }

        #[test]
        fn the_master_tap_keeps_left_and_right_for_a_stereo_picture() {
            let tap = LiveAnalysisTap::with_sides();
            tap.set_enabled(true);
            let mut stereo = [0.0f32; 128 * 2];
            for frame in stereo.as_chunks_mut::<2>().0 {
                frame[0] = 0.5;
                frame[1] = -0.25;
            }
            tap.write_stereo(&stereo, 128);
            let mut sides = [(f32::NAN, f32::NAN); LIVE_ANALYSIS_SIDES_SAMPLES];
            assert!(tap.copy_latest_sides(&mut sides));
            assert!(
                sides[..LIVE_ANALYSIS_SIDES_SAMPLES - 128]
                    .iter()
                    .all(|&(left, right)| left == 0.0 && right == 0.0),
                "silence before the ring fills"
            );
            assert!(
                sides[LIVE_ANALYSIS_SIDES_SAMPLES - 128..]
                    .iter()
                    .all(|&(left, right)| left == 0.5 && right == -0.25)
            );
            let mut mono = [f32::NAN; LIVE_ANALYSIS_WINDOW_SAMPLES];
            tap.copy_latest(&mut mono)
                .expect("the mono window is kept as before");
            assert_eq!(mono[LIVE_ANALYSIS_WINDOW_SAMPLES - 1], 0.125);

            let plain = LiveAnalysisTap::new();
            plain.set_enabled(true);
            plain.write_stereo(&stereo, 128);
            assert!(
                !plain.copy_latest_sides(&mut sides),
                "a visual tap keeps only the mono window"
            );
        }

        #[test]
        fn live_analysis_is_opt_in_and_reenable_starts_a_clean_epoch() {
            let tap = LiveAnalysisTap::new();
            let stereo = [1.0f32; 128 * 2];
            let mut analysis = [f32::NAN; LIVE_ANALYSIS_WINDOW_SAMPLES];

            tap.write_stereo(&stereo, 128);
            assert_eq!(tap.written.load(Ordering::Acquire), 0);
            assert_eq!(tap.copy_latest(&mut analysis), None);

            tap.set_enabled(true);
            tap.write_stereo(&stereo, 256);
            let first = tap.copy_latest(&mut analysis).expect("first epoch");
            assert_eq!(first.end_frame, 256);
            assert!(
                analysis[LIVE_ANALYSIS_WINDOW_SAMPLES - 128..]
                    .iter()
                    .all(|sample| *sample == 1.0)
            );

            tap.set_enabled(false);
            tap.write_stereo(&[0.25; 128 * 2], 384);
            tap.set_enabled(true);
            tap.write_stereo(&[0.5; 128 * 2], 512);
            let second = tap.copy_latest(&mut analysis).expect("second epoch");
            assert!(second.epoch > first.epoch);
            assert_eq!(second.end_frame, 512);
            assert!(
                analysis[..LIVE_ANALYSIS_WINDOW_SAMPLES - 128]
                    .iter()
                    .all(|sample| *sample == 0.0)
            );
            assert!(
                analysis[LIVE_ANALYSIS_WINDOW_SAMPLES - 128..]
                    .iter()
                    .all(|sample| *sample == 0.5)
            );
        }

        #[test]
        fn live_analysis_reader_gives_up_on_an_in_progress_publication() {
            let tap = LiveAnalysisTap::new();
            tap.set_enabled(true);
            tap.publication.store(1, Ordering::Release);
            let mut analysis = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            assert_eq!(tap.copy_latest(&mut analysis), None);
        }

        #[test]
        fn live_analysis_reset_changes_epoch_and_accepts_absolute_frontier() {
            let tap = LiveAnalysisTap::new();
            tap.set_enabled(true);
            tap.write_stereo(&[0.25; 128 * 2], 10_128);
            let mut analysis = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let before = tap.copy_latest(&mut analysis).expect("before reset");

            tap.reset();
            tap.write_stereo(&[0.5; 128 * 2], 10_256);
            let after = tap.copy_latest(&mut analysis).expect("after reset");
            assert!(after.epoch > before.epoch);
            assert!(after.end_frame > before.end_frame);
            assert!(
                analysis[..LIVE_ANALYSIS_WINDOW_SAMPLES - 128]
                    .iter()
                    .all(|sample| *sample == 0.0)
            );
        }

        #[test]
        fn live_analysis_concurrent_snapshots_do_not_mix_publications() {
            let tap = Arc::new(LiveAnalysisTap::new());
            tap.set_enabled(true);
            let writer_tap = Arc::clone(&tap);
            let gate = Arc::new(std::sync::Barrier::new(2));
            let writer_gate = Arc::clone(&gate);
            let writer = std::thread::spawn(move || {
                let mut frontier = 0u64;
                for block in 0..256usize {
                    let frames = if block % 2 == 0 { 64 } else { 128 };
                    let mut stereo = [0.0f32; 128 * 2];
                    for frame in 0..frames {
                        let value = (frontier + frame as u64) as f32;
                        stereo[frame * 2] = value;
                        stereo[frame * 2 + 1] = value;
                    }
                    frontier += frames as u64;
                    writer_tap.write_stereo(&stereo[..frames * 2], frontier);
                    if block == 0 {
                        writer_gate.wait();
                    }
                    std::thread::yield_now();
                }
            });

            let mut analysis = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let mut snapshots = 0usize;
            gate.wait();
            // Bounded by time, not by tries. `copy_latest` declines while a
            // write is in flight, so a loaded machine can lose many tries in
            // a row. The claim is that a reader eventually gets a clean window.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while snapshots < 512 && std::time::Instant::now() < deadline {
                let Some(snapshot) = tap.copy_latest(&mut analysis) else {
                    std::thread::yield_now();
                    continue;
                };
                snapshots += 1;
                let available =
                    snapshot.end_frame.min(LIVE_ANALYSIS_WINDOW_SAMPLES as u64) as usize;
                let padding = LIVE_ANALYSIS_WINDOW_SAMPLES - available;
                assert!(analysis[..padding].iter().all(|sample| *sample == 0.0));
                for (offset, sample) in analysis[padding..].iter().enumerate() {
                    let expected = snapshot.end_frame - available as u64 + offset as u64;
                    assert_eq!(*sample, expected as f32);
                }
            }
            writer.join().expect("analysis writer");
            assert!(snapshots > 0, "concurrent reader never captured a snapshot");
        }
    }
    mod record_capture {
        use super::*;

        #[test]
        fn the_record_tap_hands_over_whole_blocks_and_drops_rather_than_waits() {
            let tap = RecordTap::new();
            tap.write_stereo(&[0.5, -0.5]);
            let mut out = Vec::new();
            assert_eq!(tap.drain(&mut out), 0, "disabled: nothing queued");
            tap.set_enabled(true).expect("legacy recording");
            tap.write_stereo(&[0.25, -0.25, 1.0, -1.0]);
            assert_eq!(tap.drain(&mut out), 4);
            assert_eq!(out, [0.25, -0.25, 1.0, -1.0]);
            // Fill it to the brim, then one more block is dropped whole.
            let block = vec![0.1f32; 4096];
            let mut written = 0;
            while written + block.len() <= RECORD_RING_CAPACITY {
                tap.write_stereo(&block);
                written += block.len();
            }
            tap.write_stereo(&block);
            assert_eq!(tap.dropped_frames(), (block.len() / 2) as u64);
            out.clear();
            assert_eq!(tap.drain(&mut out), written);
            // Re-enabling starts from now and forgets the count.
            tap.set_enabled(false).expect("close legacy recording");
            tap.write_stereo(&block);
            tap.set_enabled(true).expect("restart legacy recording");
            assert_eq!(tap.dropped_frames(), 0);
            out.clear();
            assert_eq!(tap.drain(&mut out), 0);
        }

        #[test]
        fn record_capture_close_waits_for_admitted_pcm() {
            let tap = Arc::new(RecordTap::new());
            let mut capture = tap.capture().expect("capture");
            let admission = tap.admit(tap.state.load(Ordering::Relaxed)).unwrap();
            capture.request_close().unwrap();
            assert!(!tap.is_enabled());
            assert!(!capture.is_closed(), "admitted PCM has not published yet");
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            admission.tap.write_admitted(&[0.25, -0.5]);
            drop(admission);
            assert!(capture.is_closed());
            let mut samples = Vec::new();
            assert_eq!(capture.drain(&mut samples), Ok(2));
            assert_eq!(samples, [0.25, -0.5]);
            assert_eq!(capture.drain(&mut samples), Ok(0));
            assert_eq!(capture.dropped_frames(), Ok(0));
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            drop(capture);

            // Dropping the reader also cannot reset an admitted writer's epoch.
            let capture = tap.capture().unwrap();
            let admission = tap.admit(tap.state.load(Ordering::Relaxed)).unwrap();
            drop(capture);
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            drop(admission);
            let mut next = tap.capture().unwrap();
            assert_eq!(next.drain(&mut samples), Ok(0));
        }

        #[test]
        fn record_capture_close_waits_for_admitted_loss() {
            let tap = Arc::new(RecordTap::new());
            let mut capture = tap.capture().unwrap();
            let full = vec![0.25; RECORD_RING_CAPACITY];
            tap.write_stereo(&full);
            let admission = tap.admit(tap.state.load(Ordering::Relaxed)).unwrap();
            capture.request_close().unwrap();
            assert!(!capture.is_closed(), "admitted loss has not published yet");
            admission.tap.write_admitted(&[0.5, -0.5]);
            drop(admission);
            assert!(capture.is_closed());
            assert_eq!(capture.dropped_frames(), Ok(1));
            let mut samples = Vec::new();
            assert_eq!(capture.drain(&mut samples), Ok(RECORD_RING_CAPACITY));
            assert_eq!(samples, full);
            tap.write_stereo(&[0.75, -0.75]);
            assert_eq!(capture.drain(&mut samples), Ok(0));
            assert_eq!(capture.dropped_frames(), Ok(1));
        }

        #[test]
        fn record_capture_rejects_stale_admission_and_handle_operations() {
            let tap = Arc::new(RecordTap::new());
            let capture = tap.capture().unwrap();
            let observed = tap.state.load(Ordering::Relaxed);
            let epoch = capture.epoch;
            capture.request_close().unwrap();
            assert!(capture.is_closed());
            drop(capture);
            let mut next = tap.capture().unwrap();
            assert_ne!(next.epoch, epoch);
            assert!(tap.admit(observed).is_none());
            tap.write_stereo(&[0.75, -0.75]);

            let mut stale = RecordCapture {
                tap: Arc::clone(&tap),
                epoch,
            };
            let mut samples = Vec::new();
            assert_eq!(stale.request_close(), Err(RecordCaptureError::StaleCapture));
            assert!(!stale.is_closed());
            assert_eq!(
                stale.drain(&mut samples),
                Err(RecordCaptureError::StaleCapture)
            );
            assert_eq!(
                stale.dropped_frames(),
                Err(RecordCaptureError::StaleCapture)
            );
            assert!(samples.is_empty());
            drop(stale);
            assert!(tap.is_enabled());
            assert_eq!(next.drain(&mut samples), Ok(2));
            assert_eq!(samples, [0.75, -0.75]);
        }

        #[test]
        fn record_capture_rejects_busy_and_exhausted_epochs_without_resetting_pcm() {
            let tap = Arc::new(RecordTap::new());
            tap.set_enabled(true).unwrap();
            tap.write_stereo(&[0.25, -0.25]);
            let legacy_epoch = tap.state.load(Ordering::Acquire);
            tap.set_enabled(true).unwrap();
            assert_eq!(tap.state.load(Ordering::Acquire), legacy_epoch);
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            let mut samples = Vec::new();
            assert_eq!(tap.drain(&mut samples), 2);
            assert_eq!(samples, [0.25, -0.25]);
            tap.write_stereo(&[0.5, -0.5]);
            tap.set_enabled(false).unwrap();
            tap.state
                .store(MAX_RECORD_EPOCH << RECORD_EPOCH_SHIFT, Ordering::Release);
            let read = tap.read.load(Ordering::Acquire);
            let written = tap.written.load(Ordering::Acquire);
            assert_eq!(
                tap.capture().unwrap_err(),
                RecordCaptureError::EpochExhausted
            );
            assert_eq!(
                tap.set_enabled(true),
                Err(RecordCaptureError::EpochExhausted)
            );
            assert_eq!(tap.read.load(Ordering::Acquire), read);
            assert_eq!(tap.written.load(Ordering::Acquire), written);
            samples.clear();
            assert_eq!(tap.drain(&mut samples), 2);
            assert_eq!(samples, [0.5, -0.5]);
        }

        #[test]
        fn record_capture_legacy_controls_cannot_steal_claimed_pcm() {
            let tap = Arc::new(RecordTap::new());
            let mut capture = tap.capture().unwrap();
            tap.write_stereo(&[0.25, -0.25]);
            assert_eq!(tap.set_enabled(true), Err(RecordCaptureError::Busy));
            assert_eq!(tap.set_enabled(false), Err(RecordCaptureError::Busy));
            let mut samples = Vec::new();
            assert_eq!(tap.drain(&mut samples), 0);
            assert!(samples.is_empty());
            capture.request_close().unwrap();
            assert!(capture.is_closed());
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            assert_eq!(capture.drain(&mut samples), Ok(2));
            assert_eq!(samples, [0.25, -0.25]);
        }

        #[test]
        fn record_capture_rate_change_closes_without_resetting_tail() {
            let shared = LiveShared::new(7);
            let mut capture = shared.record.capture().unwrap();
            shared.record.write_stereo(&[0.25, -0.25]);
            shared.prepare_recycled_stream(48_000, 48_000);
            assert!(!capture.is_closed());
            shared.record.write_stereo(&[0.5, -0.5]);
            shared.prepare_recycled_stream(48_000, 44_100);
            assert!(capture.is_closed());
            shared.record.write_stereo(&[0.75, -0.75]);
            drop(shared);
            let mut samples = Vec::new();
            assert_eq!(capture.drain(&mut samples), Ok(4));
            assert_eq!(samples, [0.25, -0.25, 0.5, -0.5]);
            assert_eq!(capture.dropped_frames(), Ok(0));
            assert_eq!(capture.drain(&mut samples), Ok(0));
        }

        #[test]
        fn record_capture_callback_accept_drop_and_closed_paths_do_not_allocate() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            let shared = LiveShared::new(7);
            let mut capture = shared.record.capture().unwrap();
            let mut backend = LiveScalarBackend::new(48_000, 4).unwrap();
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            let owners = Arc::strong_count(&shared.record);
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            let rest = vec![0.0; RECORD_RING_CAPACITY - output.len()];
            shared.record.write_stereo(&rest);
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            assert_eq!(capture.dropped_frames(), Ok(128));
            capture.request_close().unwrap();
            assert!(capture.is_closed());
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            assert_eq!(Arc::strong_count(&shared.record), owners);
            assert_eq!(shared.report(1).callback_scope_misses, 0);
            assert_eq!(shared.clock_frames(), 384);
            let mut samples = Vec::new();
            assert_eq!(capture.drain(&mut samples), Ok(RECORD_RING_CAPACITY));
            assert!(samples.iter().all(|sample| *sample == 0.0));
            assert_eq!(capture.dropped_frames(), Ok(128));
        }
    }
    mod reload_and_clock {
        use super::*;

        /// The reload takeover contract: a save's new generation re-queries
        /// from the published takeover frame, so old-generation events BEFORE it
        /// (the already-scheduled horizon) still play, and only events at/after
        /// it are filtered. Dropping the earlier ones cuts the music by one
        /// continuity margin on every save.
        /// A reload lets voices ring; an audition swap must not.
        ///
        /// Previewing a snippet installs a source the way a save does, and a
        /// save deliberately leaves sounding voices alone - cutting them would
        /// click on every edit. For an audition that is the bug: a four-second
        /// break played on under the snippet that replaced it. `cut_sounding`
        /// is the verb for that case, and this is what it buys.
        #[test]
        fn cut_sounding_silences_what_a_reload_would_have_let_ring() {
            let long_note = AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 4.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            let render = |cut_at: Option<u64>| -> f32 {
                let shared = LiveShared::new(1);
                assert!(shared.ring.push(long_note.clone()));
                let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
                let mut output = [0.0f32; 128 * 2];
                let mut start = 0u64;
                let mut tail = 0.0f32;
                while start < 24_000 {
                    if Some(start) == cut_at {
                        backend.cut_sounding_voices(start);
                    }
                    output.fill(0.0);
                    backend.process_block_with(
                        &mut output,
                        128,
                        start,
                        &shared.ring,
                        shared.flip_atomics(),
                        &AtomicBool::new(shared.stop.is_stopped()),
                    );
                    // A tenth of a second past the cut, well beyond its ramp.
                    if start >= 16_800 {
                        tail = tail.max(
                            output
                                .iter()
                                .fold(0.0f32, |peak, sample| peak.max(sample.abs())),
                        );
                    }
                    start += 128;
                }
                tail
            };
            let ringing = render(None);
            let cut = render(Some(12_032));
            assert!(ringing > 1e-3, "the note never sounded: {ringing}");
            assert!(
                cut < 1e-5,
                "the note was still sounding a tenth of a second after the cut: {cut}"
            );
        }

        #[test]
        fn live_reload_preserves_an_already_active_sbd_graph() {
            let render = |reload: bool, resend: bool| {
                let shared = LiveShared::new(1);
                let hit = |onset_id: u64, generation: u64| AudioEvent {
                    onset_id,
                    generation,
                    target_frame: 4_800,
                    onset_lead: 0.0,
                    freq_hz: 55.0,
                    gain: 0.5,
                    duration_secs: 0.3,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: Some(crate::SynthSource::Sbd {
                        decay_secs: 0.2,
                        pdecay_secs: 0.3,
                        penv_semitones: 36.0,
                        stop_secs: 0.21,
                    }),
                    wavetable: None,
                    cut: None,
                };
                assert!(shared.ring.push(hit(1, 1)));
                let mut backend =
                    LiveScalarBackend::with_dispatch(48_000, 4, DspDispatch::portable())
                        .expect("live scalar");
                let mut meters = test_meter();
                let mut pcm = vec![0.0f32; 12_288 * 2];
                for (index, output) in pcm.as_chunks_mut::<{ 128 * 2 }>().0.iter_mut().enumerate() {
                    if reload && index == 1 {
                        // Existing low-latency edits can take over in 80 ms,
                        // inside SBD's 100 ms graph lead. An already activated
                        // graph, like an already sounding ordinary voice, is
                        // not pending work for retire_pending_from to discard.
                        shared.takeover_frame.store(128 + 3_840, Ordering::Release);
                        shared.generation.store(2, Ordering::Release);
                        // The new generation queries from the takeover and
                        // sends the same onset again. The copy is dropped.
                        if resend {
                            assert!(shared.ring.push(hit(2, 2)));
                        }
                    }
                    tripwire::audio_scope(|| {
                        write_live_output(&mut backend, &mut meters, output, 2, &shared)
                    });
                }
                assert_eq!(shared.refused_voices.load(Ordering::Acquire), 0);
                assert_eq!(shared.late_events.load(Ordering::Acquire), 0);
                assert!(pcm[4_800 * 2..].iter().any(|sample| sample.abs() > 1e-3));
                pcm
            };
            assert_eq!(render(false, false), render(true, false));
            assert_eq!(
                render(false, false),
                render(true, true),
                "the onset sounds once"
            );
        }

        #[test]
        fn reload_keeps_old_generation_events_before_the_takeover_frame() {
            let event = |onset_id: u64, generation: u64, target_frame: u64| AudioEvent {
                onset_id,
                generation,
                target_frame,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 0.01,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            let shared = LiveShared::new(1);
            // Old-generation horizon: one onset soon, one past the takeover.
            assert!(shared.ring.push(event(1, 1, 1_000)));
            assert!(shared.ring.push(event(2, 1, 60_000)));
            // The reload: takeover is published before the generation flip, then
            // the replacement generation supplies onsets from the takeover on.
            shared.takeover_frame.store(50_000, Ordering::Release);
            shared.generation.store(2, Ordering::Release);
            assert!(shared.ring.push(event(3, 2, 60_100)));

            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            let mut kept_audible = false;
            let mut accepted = 0usize;
            let mut stale = 0usize;
            let mut start = 0u64;
            while start < 61_000 {
                output.fill(0.0);
                let report = backend.process_block_with(
                    &mut output,
                    128,
                    start,
                    &shared.ring,
                    shared.flip_atomics(),
                    &AtomicBool::new(shared.stop.is_stopped()),
                );
                accepted += report.accepted;
                stale += report.stale;
                if (960..1_100).contains(&start) && output.iter().any(|sample| sample.abs() > 1e-6)
                {
                    kept_audible = true;
                }
                start += 128;
            }
            assert_eq!(
                accepted, 2,
                "pre-takeover old onset and the replacement onset must both play"
            );
            assert_eq!(stale, 1, "only the post-takeover old onset is replaced");
            assert!(kept_audible, "the kept pre-takeover onset never sounded");
        }

        /// The callback reads each handshake word from its own atomic. The line
        /// arm and the cut intent are both `AtomicU64`s published by the same
        /// thread; wired into each other's place, a line arm reads as no cut and
        /// a cut reads as a line armed at frame 0 with no drop horizon. Driven
        /// through `write_live_output`, the production wiring: a line armed
        /// with its drop bit refuses an outgoing event past it, and an
        /// immediate rewind's cut refuses an outgoing event its takeover frame
        /// alone would have let through.
        #[test]
        fn the_callback_reads_the_line_arm_and_the_cut_from_their_own_words() {
            let event = |onset_id: u64, generation: u64, target_frame: u64| AudioEvent {
                onset_id,
                generation,
                target_frame,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 0.01,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut meters = test_meter();
            let mut output = [0.0f32; 128 * 2];
            let mut callback = |backend: &mut LiveScalarBackend| {
                tripwire::audio_scope(|| {
                    write_live_output(backend, &mut meters, &mut output, 2, &shared)
                });
            };
            let stale = || shared.stale_events.load(Ordering::Acquire);

            // The line at 256, armed with its drop bit (arm_line_cut's word).
            shared.line_arm.store((256 << 2) | 0b11, Ordering::Release);
            callback(&mut backend); // [0, 128)
            callback(&mut backend); // [128, 256)
            assert!(shared.ring.push(event(1, 1, 4_000)));
            callback(&mut backend); // [256, 384): fires, refuses the event
            assert_eq!(
                stale(),
                1,
                "the armed line refuses the outgoing event past it"
            );

            // An immediate rewind (set_generation's order), its takeover past
            // the outgoing event: only the cut's own horizon refuses it.
            shared.takeover_frame.store(8_192, Ordering::Release);
            shared
                .takeover_cut
                .store(TakeoverCut::AtFlip as u64, Ordering::Release);
            shared.line_arm.store(0, Ordering::Release);
            shared.generation.store(2, Ordering::Release);
            assert!(shared.ring.push(event(2, 1, 6_000)));
            callback(&mut backend); // [384, 512)
            assert_eq!(stale(), 2, "the rewind's cut refuses the outgoing event");
        }

        /// A published flip supersedes a pre-armed line: `set_generation`
        /// leaves no arm behind, even an edit's flip that asks for no cut of its
        /// own, so a launch's stale line can never cut a later generation that
        /// never asked for one. It leaves zero, not the withdrawn marker: the
        /// consumer tells "the flip took over" from "the launch was withdrawn"
        /// by exactly that word.
        #[test]
        fn a_published_flip_leaves_no_line_arm() {
            let device = LiveScalarDevice::start_silent(48_000, 6).expect("silent output");
            device.arm_line_cut(1_000, true);
            assert!(device.line_cut_armed());
            assert_eq!(device.armed_line_word(), (1_000 << 2) | 0b11);
            assert_eq!(device.armed_line_frame(), Some(1_000));
            device.set_generation(7, 2_000, TakeoverCut::None);
            assert!(!device.line_cut_armed(), "the flip supersedes the arm");
            assert_eq!(device.armed_line_word(), 0, "and leaves no word behind");
            assert_eq!(device.armed_line_frame(), None);

            device.arm_line_cut(3_000, false);
            device.clear_line_arm();
            assert!(!device.line_cut_armed());
            assert_eq!(device.armed_line_word(), crate::LINE_ARM_WITHDRAWN);
            assert_eq!(device.armed_line_frame(), None);
            device.set_generation(8, 4_000, TakeoverCut::None);
            assert_eq!(
                device.armed_line_word(),
                0,
                "a flip after a withdrawal is a flip, not a withdrawal"
            );
        }

        #[test]
        fn live_reload_margin_does_not_recount_downstream_playback_latency() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            // Join the callback before injecting the two observations. Both
            // describe exactly the same completed render frontier; only PCM
            // already handed downstream takes longer to reach the listener.
            device.silent.take().expect("silent callback").stop();
            device.shared.frames.store(48_000, Ordering::Release);

            for requested_frames in [256, 2048] {
                device.requested_buffer_frames = requested_frames;
                device
                    .shared
                    .playback_latency_nanos
                    .store(75_000_000, Ordering::Release);
                let shallow_margin = device.continuity_margin_seconds();
                let shallow_clock = device.clock_nanos();
                device
                    .shared
                    .playback_latency_nanos
                    .store(800_000_000, Ordering::Release);
                let deep_margin = device.continuity_margin_seconds();

                assert_eq!(device.clock_nanos(), shallow_clock);
                assert_eq!(
                    deep_margin, shallow_margin,
                    "downstream delay moved the edit boundary without any additional rendering"
                );
                assert_eq!(deep_margin, f64::from(requested_frames) / 48_000.0);
            }
        }

        #[test]
        fn live_reload_margin_tracks_in_flight_work_without_certifying_it() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.silent.take().expect("silent callback").stop();
            device.requested_buffer_frames = 256;
            for sample_rate in [44_100, 48_000, 96_000] {
                device.sample_rate = sample_rate;
                let startup_lead = device.schedule_lead_seconds();
                let completed = u64::from(sample_rate);
                device.shared.frames.store(completed, Ordering::Release);
                device
                    .shared
                    .rendering_until_frame
                    .store(completed + 1_024, Ordering::Release);

                assert_eq!(device.clock_nanos(), 1_000_000_000);
                assert_eq!(device.report().submitted_frames, completed);
                assert_eq!(
                    device.render_frontier_seconds(),
                    (completed + 1_024) as f64 / f64::from(sample_rate)
                );
                assert_eq!(
                    device.continuity_margin_seconds(),
                    1_280.0 / f64::from(sample_rate)
                );

                // Once copied, the reservation is no longer extra work ahead
                // of the completed clock. Do not charge for it a second time.
                device
                    .shared
                    .frames
                    .store(completed + 1_024, Ordering::Release);
                assert_eq!(
                    device.continuity_margin_seconds(),
                    256.0 / f64::from(sample_rate)
                );
                assert_eq!(device.schedule_lead_seconds(), startup_lead);
            }
        }

        #[test]
        fn live_render_frontier_advances_per_chunk_inside_an_oversized_callback() {
            // Observe the real writer at its existing sample-conversion seam;
            // no production-only test hooks, clocks, or callback allocations.
            thread_local! {
                static OBSERVED: std::cell::RefCell<Option<LiveShared>> = const {
                    std::cell::RefCell::new(None)
                };
            }
            #[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
            struct ObservedSample {
                value: f32,
                completed: u64,
                frontier: u64,
            }
            impl Sample for ObservedSample {
                type Signed = f32;
                type Float = f32;
                const EQUILIBRIUM: Self = Self {
                    value: 0.0,
                    completed: 0,
                    frontier: 0,
                };
            }
            impl FromSample<f32> for ObservedSample {
                fn from_sample_(value: f32) -> Self {
                    OBSERVED.with(|slot| {
                        let shared = slot.borrow();
                        let shared = shared.as_ref().expect("writer under observation");
                        Self {
                            value,
                            completed: shared.clock_frames(),
                            frontier: shared.render_frontier_frames(),
                        }
                    })
                }
            }
            impl FromSample<ObservedSample> for f32 {
                fn from_sample_(value: ObservedSample) -> Self {
                    value.value
                }
            }

            let _guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
            let mut meters = test_meter();
            OBSERVED.with(|slot| *slot.borrow_mut() = Some(shared.clone()));
            for frames in [1_025, 2_048] {
                let start = shared.clock_frames();
                let mut output = vec![ObservedSample::EQUILIBRIUM; frames * 2];
                let before = tripwire::Violations::capture();
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert!(tripwire::Violations::capture().since(before).clean());
                for (frame, pair) in output.as_chunks::<2>().0.iter().enumerate() {
                    let expected_frontier = start + ((frame / 128 + 1) * 128).min(frames) as u64;
                    for sample in pair {
                        assert_eq!(sample.completed, start, "partial copy was certified");
                        assert_eq!(sample.frontier, expected_frontier);
                    }
                }
                assert_eq!(shared.clock_frames(), start + frames as u64);
                assert_eq!(shared.render_frontier_frames(), shared.clock_frames());
            }
            OBSERVED.with(|slot| *slot.borrow_mut() = None);
        }

        #[test]
        fn live_dsp_advances_by_submitted_frames_not_playback_timestamps() {
            let shared = LiveShared::new(1);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 128,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 0.01,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut first = [0.0f32; 128 * 2];
            let mut second = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut first, 2, &shared)
            });
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut second, 2, &shared)
            });
            assert_eq!(shared.report(1).submitted_frames, 256);
            let first_peak = first
                .iter()
                .fold(0.0f32, |acc, sample| acc.max(sample.abs()));
            let second_peak = second
                .iter()
                .fold(0.0f32, |acc, sample| acc.max(sample.abs()));
            assert!(
                first_peak < 1e-6,
                "onset at frame 128 leaked into the first callback: {first_peak}"
            );
            assert!(
                second_peak > 1e-4,
                "monotonic start frames never reached the onset: {second_peak}"
            );
        }

        #[test]
        fn scheduler_clock_uses_current_playhead_not_future_buffer_position() {
            let origin = 4_000_000_000;
            let shared = LiveShared::new(1);
            shared
                .playback_origin_nanos
                .store(origin, Ordering::Release);
            shared
                .buffer_playback_nanos
                .store(12_000_000_000, Ordering::Release);
            let now = StreamInstant::from_nanos(origin + 8_000_000_000);

            assert_eq!(shared.playhead_at(now), 8_000_000_000);
            assert_ne!(
                shared.playhead_at(now),
                shared.buffer_playback_nanos.load(Ordering::Acquire)
            );
            assert_eq!(shared.playhead_at(StreamInstant::from_nanos(origin - 1)), 0);
        }
    }
}
