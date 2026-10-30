use super::*;
use crate::pool::{JobKind, JoinGuard, PoolHandle};
use crate::sample::{SampleBank, SampleResamplingMode};

/// On x86_64, flush subnormal inputs and results to zero so quiet filter and
/// reverb tails avoid denormal arithmetic costs. These flags are per-thread;
/// set them at every `process_block` because init may run on another thread.
/// A plugin worker sets them at each job.
/// Miri has no inline assembly, so a Miri run keeps the default flags.
pub(crate) fn enable_denormal_flush() {
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    {
        const FTZ: u32 = 1 << 15;
        const DAZ: u32 = 1 << 6;
        let mut csr = 0u32;
        // SAFETY: `stmxcsr`/`ldmxcsr` only read and write this thread's MXCSR.
        unsafe {
            std::arch::asm!(
                "stmxcsr [{csr}]",
                csr = in(reg) &mut csr,
                options(nostack, preserves_flags)
            );
            let flushed = csr | FTZ | DAZ;
            if csr != flushed {
                csr = flushed;
                std::arch::asm!(
                    "ldmxcsr [{csr}]",
                    csr = in(reg) &csr,
                    options(nostack, preserves_flags)
                );
            }
        }
    }
}

/// True when the voice runs no optional ladder at all: a stereo sample
/// with no modulators, no FM route, no live sliders, no fades, no filters,
/// no FX of any kind, no sends, no insert routing and no visual capture.
///
/// Every frame of such a voice is the source, the envelope, three gains,
/// the panner and the mix - which is exactly what
/// [`render_plain_stereo_sample`] computes, operation for operation as the
/// general body does with every gate closed. The classification is
/// recomputed each render because steals and polyphony fades change a
/// voice's ladders mid-life.
fn plain_sample_voice(v: &Voice) -> bool {
    matches!(v.source, VoiceSource::Sample { .. })
        && v.filters_right.is_some()
        && !v.has_filters
        && !v.has_pre_distort_fx
        && !v.has_param_modulators
        && v.pitch_quantum_skip == 0
        && v.fm_state.is_none()
        && v.controls.fm.is_none()
        && v.live_fm_index.binding == 0
        && v.live_cutoff.binding == 0
        && v.live_resonance.binding == 0
        && v.live_gain.binding == 0
        && v.cut_fade_frame.is_none()
        && v.polyphony_fade_frame.is_none()
        && !v.through_insert
        && v.controls.vibrato.is_none()
        && v.controls.pitch_env.is_none()
        && v.controls.tremolo.is_none()
        && v.controls.phaser.is_none()
        && v.controls.vowel.is_none()
        && v.controls.distort.is_none()
        && v.compressor.is_none()
        && v.controls.compressor.is_none()
        && v.limiter.is_none()
        && v.fx_stage_state.is_none()
        && v.kabel.is_none()
        && v.stretch.is_none()
        && v.controls.stretch.is_none()
        && v.transient.is_none()
        && v.delay_wet == 0.0
        && v.reverb_wet == 0.0
        && v.controls.bus.is_none()
        && v.ui_visuals == 0
}

/// The tight path a plain sample voice takes, one frame at a time.
///
/// Kept out of line so it earns its own registers: the general body in
/// [`ScalarBackend::process_segment`] is so wide that a plain voice paid
/// its stack spills every frame, plain or not. The arithmetic here is the
/// general body's with every gate closed, in the same order: skips are
/// only identities (a multiply by one, an add the full path also makes),
/// so the frames are bit for bit what the general body produces.
#[inline(never)]
fn render_plain_stereo_sample(
    v: &mut Voice,
    bank: &SampleBank,
    resampling: SampleResamplingMode,
    i: usize,
    t: f32,
    blocks: &mut [[[f32; crate::reverb::REVERB_BLOCK]; 2]],
    piano_mix: &mut [[f32; crate::reverb::REVERB_BLOCK]; 2],
) {
    let env = ScalarBackend::envelope(t, v.duration_secs, v.controls.envelope, v.gate_value);
    let VoiceSource::Sample {
        sample,
        position,
        increment,
        reversed,
        delay_frames,
        loop_frames,
    } = &mut v.source
    else {
        return;
    };
    // The general body reads its playback step through the per-quantum
    // pitch hold; a plain voice holds no pitch shaper, so its step is the
    // increment itself (multiplying by the held one is an exact identity).
    let (wave_l, wave_r) = if bank.get(*sample).is_none() {
        // The slot was emptied under the voice - an uninstall landed. The
        // voice ends with the block, as the general body ends it.
        v.source_gone = true;
        (0.0, 0.0)
    } else if *delay_frames > 0 {
        *delay_frames -= 1;
        (0.0, 0.0)
    } else if let Some((loop_start, loop_end)) = *loop_frames
        && loop_end > loop_start
        && bank
            .get(*sample)
            .is_some_and(|decoded| *increment > loop_end.min(decoded.frames() as f64) - loop_start)
    {
        (0.0, 0.0)
    } else {
        let wave = bank
            .get(*sample)
            .and_then(|decoded| {
                if *reversed {
                    let last = (decoded.frames().max(1) - 1) as f64;
                    let read = last - *position;
                    if read < 0.0 {
                        return None;
                    }
                    decoded.stereo_at_reversed_with_mode(read, resampling)
                } else {
                    decoded.stereo_at_with_mode(*position, resampling)
                }
            })
            .unwrap_or((0.0, 0.0));
        *position += *increment;
        if let Some((loop_start, loop_end)) = loop_frames
            && *position >= *loop_end
            && *loop_end > *loop_start
        {
            *position -= *loop_end - *loop_start;
        }
        wave
    };
    let scale = v.amplitude_scale * env;
    let (left, right) = (wave_l * scale, wave_r * scale);
    let (left, right) = stereo_pan(left, right, v.pan_x);
    let post_gain = v.controls.postgain;
    let (left, right) = (left * post_gain, right * post_gain);
    let dry = v.dry_gain + 0.0;
    let (left, right) = (left * dry, right * dry);
    let mix = if v.piano {
        piano_mix
    } else {
        &mut blocks[v.orbit]
    };
    mix[0][i] += left;
    mix[1][i] += right;
}

impl ScalarPressureState {
    fn pool_miss(&mut self, pool: RealtimePool) {
        self.pool_misses[pool.index()] = self.pool_misses[pool.index()].saturating_add(1);
    }

    fn activate(&mut self, voice: &Voice) {
        let orbit = voice.orbit.min(MAX_ORBITS - 1);
        self.active_orbit_voices[orbit] = self.active_orbit_voices[orbit].saturating_add(1);

        let mut lease = |pool: RealtimePool, active: bool| {
            if active {
                self.active_pool_leases[pool.index()] =
                    self.active_pool_leases[pool.index()].saturating_add(1);
            }
        };
        lease(RealtimePool::Compressor, voice.compressor.is_some());
        lease(RealtimePool::Stretch, voice.stretch.is_some());
        lease(
            RealtimePool::ZzfxDelay,
            matches!(voice.source, VoiceSource::ZzFx { ring: Some(_), .. }),
        );
        for stage in voice
            .fx_stage_state
            .iter()
            .flat_map(|stages| stages.iter().flatten())
        {
            lease(RealtimePool::Compressor, stage.compressor.is_some());
            lease(RealtimePool::FxDelay, stage.delay.is_some());
            lease(RealtimePool::Stretch, stage.stretch.is_some());
            lease(RealtimePool::FxReverb, stage.room.is_some());
        }

        if voice.controls.compressor.is_some() && voice.compressor.is_none() {
            self.pool_miss(RealtimePool::Compressor);
        }
        if voice.controls.stretch.is_some() && voice.stretch.is_none() {
            self.pool_miss(RealtimePool::Stretch);
        }
        if let Some(stages) = &voice.fx_stage_state {
            for state in stages.iter().flatten() {
                let controls = &state.controls;
                if controls.compressor.is_some() && state.compressor.is_none() {
                    self.pool_miss(RealtimePool::Compressor);
                }
                if controls.delay.is_some() && state.delay.is_none() {
                    self.pool_miss(RealtimePool::FxDelay);
                }
                if controls.stretch.is_some() && state.stretch.is_none() {
                    self.pool_miss(RealtimePool::Stretch);
                }
                if controls.room.is_some() && state.room.is_none() {
                    self.pool_miss(RealtimePool::FxReverb);
                }
            }
        }
    }

    pub(super) fn retire(&mut self, voice: &Voice) {
        let orbit = voice.orbit.min(MAX_ORBITS - 1);
        self.active_orbit_voices[orbit] = self.active_orbit_voices[orbit].saturating_sub(1);

        let mut release = |pool: RealtimePool, active: bool| {
            if active {
                self.active_pool_leases[pool.index()] =
                    self.active_pool_leases[pool.index()].saturating_sub(1);
            }
        };
        release(RealtimePool::Compressor, voice.compressor.is_some());
        release(RealtimePool::Stretch, voice.stretch.is_some());
        release(
            RealtimePool::ZzfxDelay,
            matches!(voice.source, VoiceSource::ZzFx { ring: Some(_), .. }),
        );
        for stage in voice
            .fx_stage_state
            .iter()
            .flat_map(|stages| stages.iter().flatten())
        {
            release(RealtimePool::Compressor, stage.compressor.is_some());
            release(RealtimePool::FxDelay, stage.delay.is_some());
            release(RealtimePool::Stretch, stage.stretch.is_some());
            release(RealtimePool::FxReverb, stage.room.is_some());
        }
    }

    pub(super) fn clear_active(&mut self) {
        self.active_orbit_voices = [0; MAX_ORBITS];
        self.active_pool_leases = [0; REALTIME_POOL_COUNT];
    }
}

impl AudioBackend for ScalarBackend {
    fn init(&mut self, sample_rate: u32) -> Result<(), String> {
        if sample_rate == 0 {
            return Err("sample_rate must be > 0".into());
        }
        self.sample_rate = sample_rate;
        reserve_fx_reverb_returns(&mut self.fx_reverb_pool, self.fx_reverb_inline_count);
        self.tables = Some(prepared_tables(sample_rate));
        self.sbd_saturation_curve = Some(sbd_saturation_curve(sample_rate));
        match self.bank.as_mut() {
            None => self.bank = Some(SampleBank::with_bundled_bd()?),
            Some(bank) => {
                // Re-init (or preloaded-before-init): keep installed samples,
                // just guarantee the bundled slot.
                if bank.get(crate::sample::BUNDLED_BD_SAMPLE_ID).is_none() {
                    let _ = bank.install(
                        crate::sample::BUNDLED_BD_SAMPLE_ID,
                        Box::new(bundled_sample(crate::sample::BundledSample::Bd)?),
                    );
                }
            }
        }
        // Fixed 1-second stereo line per orbit.
        self.orbit_ducks = vec![OrbitDuck::default(); MAX_ORBITS];
        self.orbit_djf = vec![DjfState::default(); MAX_ORBITS];
        self.orbit_channels = vec![None; MAX_ORBITS];
        self.limiter_pool = (0..LIMITER_POOL)
            .map(|_| {
                Box::new(crate::limiter::InlineLimiter::in_line(
                    self.sample_rate,
                    crate::limiter::DEFAULT_THRESHOLD_DB,
                    crate::limiter::Character::default(),
                ))
            })
            .collect();
        self.compressor_pool = (0..16 * (1 + crate::backend::MAX_FX_STAGES))
            .map(|_| Box::new(CompressorState::cold()))
            .collect();
        // Smaller than the compressor pool because each line is far larger:
        // 384 KiB at 48 kHz against 10 KiB. Sixteen covers sixteen voices
        // delaying in one stage, or five using all three.
        if self.kabel_pool.capacity() < MAX_KABEL_GRAPHS {
            self.kabel_pool
                .reserve(MAX_KABEL_GRAPHS - self.kabel_pool.len());
        }
        self.kabel_program_protected
            .resize(crate::assets::MAX_KABEL_PROGRAMS, false);
        if self.kabel_programs.capacity() < crate::assets::MAX_KABEL_PROGRAMS {
            self.kabel_programs
                .reserve(crate::assets::MAX_KABEL_PROGRAMS - self.kabel_programs.len());
        }
        self.fx_delay_pool = (0..16).map(|_| FxDelayLine::new(sample_rate)).collect();
        self.zzfx_ring_pool = (0..8)
            .map(|_| vec![0.0f32; (sample_rate as usize) * 2].into_boxed_slice())
            .collect();
        // Shared FFT plans, buffers allocated once: leasing from this pool is
        // pointer work. Building Stretch::new inside process_block was the
        // live-path xrun for `.stretch()`.
        {
            let mut planner = rustfft::FftPlanner::new();
            let prototype = crate::stretch::Stretch::new(&mut planner);
            self.stretch_pool = (0..16 * (1 + crate::backend::MAX_FX_STAGES))
                .map(|_| Box::new([prototype.clone(), prototype.clone()]))
                .collect();
        }
        if self.fx_reverb_wanted.capacity() < 8 {
            self.fx_reverb_wanted = Vec::with_capacity(8);
        }
        if self.pending_ducks.capacity() == 0 {
            self.pending_ducks = Vec::with_capacity(4096);
        }
        if self.orbit_reverbs.is_empty() {
            self.orbit_reverbs = (0..MAX_ORBITS).map(|_| None).collect();
        }
        self.init_insert_pool();
        self.orbit_blocks = vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; MAX_ORBITS];
        self.insert_blocks =
            vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; crate::insert::MAX_INSERT_BUSES];
        self.insert_dry =
            vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; crate::insert::MAX_INSERT_BUSES];
        self.insert_leaving_blocks =
            vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; crate::insert::MAX_INSERT_BUSES];
        self.instrument_blocks =
            vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; crate::insert::MAX_INSERT_BUSES];
        self.instrument_reverb_sends =
            vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; crate::insert::MAX_INSERT_BUSES];
        self.instrument_delay_sends =
            vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; crate::insert::MAX_INSERT_BUSES];
        self.insert_postgains = vec![InsertPostgain::default(); crate::insert::MAX_INSERT_BUSES];
        self.insert_postgain_blocks =
            vec![[1.0; crate::reverb::REVERB_BLOCK]; crate::insert::MAX_INSERT_BUSES];
        self.insert_outputs = (0..crate::insert::MAX_INSERT_BUSES)
            .map(|bus| bus.min(MAX_ORBITS - 1))
            .collect();
        self.insert_idle_frames = vec![0; crate::insert::INSERT_SLOTS];
        self.effect_order = vec![
            [crate::insert::NO_STAGE; crate::insert::EFFECT_CHAIN];
            crate::insert::MAX_INSERT_BUSES
        ];
        self.insert_mixes = vec![InsertMix::default(); crate::insert::INSERT_SLOTS];
        self.insert_generations = vec![0; crate::insert::MAX_INSERT_BUSES];
        self.instrument_to_effect = vec![false; crate::insert::MAX_INSERT_BUSES];
        self.instrument_gains = vec![InstrumentGain::default(); crate::insert::MAX_INSERT_BUSES];
        self.instrument_filters =
            vec![[FilterChain::default(); 2]; crate::insert::MAX_INSERT_BUSES];
        self.instrument_gain_changes = Vec::with_capacity(MAX_ACTIVE_VOICES);
        self.reverb_sends = vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; MAX_ORBITS];
        self.orbit_mix = vec![0.0; MAX_ORBITS * ORBIT_MIX_FRAMES * 2];
        self.ui_visual_mix = vec![[0.0; UI_VISUAL_MIX_FRAMES * 2]; MAX_UI_AUDIO_VISUALS];
        self.ui_visual_capture_mask = 0;
        self.ui_visual_capture_generation_floor = 0;
        self.orbit_peaks = [0.0; MAX_ORBITS];
        self.live_control_targets = Vec::with_capacity(MAX_ACTIVE_VOICES * 2);
        self.insert_values = Vec::with_capacity(MAX_INSERT_VALUES);
        self.allow_inline_reverb = true;
        self.orbit_delays = (0..MAX_ORBITS)
            .map(|_| OrbitDelay {
                left: vec![0.0; sample_rate as usize],
                right: vec![0.0; sample_rate as usize],
                ..OrbitDelay::default()
            })
            .collect();
        self.reset();
        self.ensure_fm_state_pool(self.voices.capacity());
        Ok(())
    }

    fn reset(&mut self) {
        self.reset_at(0);
    }

    fn prepare_note(&mut self, event: &OnsetEvent) -> Result<(), String> {
        self.prepare_kabel_event(event)
    }

    fn note(&mut self, event: OnsetEvent) {
        if event.controls.choke_only {
            let _ = self.queue_choke(event);
            return;
        }
        // Muted (speed 0) events STAY: the sidechain is still armed
        // and updates the orbit chain before the sampler bails; only the
        // source is skipped (activation handles that).
        if self.pending.len() >= MAX_PENDING_EVENTS {
            self.pending_ceiling_drops += 1;
            self.pressure.pending_ceiling_drops =
                self.pressure.pending_ceiling_drops.saturating_add(1);
            return;
        }
        if self.prepare_kabel_event(&event).is_err() {
            self.missing_kabel_events += 1;
            return;
        }
        self.arm_duck(&event);
        // A real-time consumer activates pending events by moving them into
        // `voices`. Reserve that final size HERE, on the producer thread, so
        // the later push cannot allocate in the audio callback.
        let required = self
            .voices
            .len()
            .saturating_add(self.pending.len())
            .saturating_add(1);
        if self.voices.capacity() < required {
            self.voices.reserve(required - self.voices.len());
        }
        self.ensure_fx_stage_state_pool(self.voices.capacity());
        self.ensure_fm_state_pool(self.voices.capacity());
        self.leasing_waves |= event.controls.partials.is_some();
        self.pending
            .push(event.map_kabel(CompactKabelControls::from));
    }

    fn stems(&self) -> Option<&Stems> {
        self.stems.as_ref()
    }

    fn process_block(&mut self, out: &mut [f32], frames: usize) {
        assert!(
            out.len() >= frames * 2,
            "stereo interleaved buffer too small"
        );
        self.lease_due_waves(frames);
        let visual_frames = frames.min(UI_VISUAL_MIX_FRAMES) * 2;
        let mut visual_slots = self.ui_visual_capture_mask;
        while visual_slots != 0 {
            let slot = visual_slots.trailing_zeros() as usize;
            if let Some(block) = self.ui_visual_mix.get_mut(slot) {
                block[..visual_frames].fill(0.0);
            }
            visual_slots &= visual_slots - 1;
        }
        if self.routing_active() {
            self.clear_orbit_mix(frames);
        }
        if let Some(stems) = self.stems.as_mut() {
            stems.start_block();
        }
        // Reserve input for the whole callback before any instrument changes
        // split it into shorter spans.
        let input_base = self.input.as_ref().map(|ring| {
            let ring_rate = ring.sample_rate();
            let ratio = if ring_rate == 0 || self.sample_rate == 0 {
                1.0
            } else {
                f64::from(ring_rate) / f64::from(self.sample_rate)
            };
            // An input voice under the FM matrix reads ahead of the shared
            // cursor by the integrated bend, and a rate-locked cursor keeps
            // only the callback's own frames of room - the keep rule never
            // widens it to the fixed lag. Reserve the bend's room with the
            // callback's, so the read never crosses the writer.
            let input_fm = self.voices.iter().any(|voice| {
                voice.controls.fm.is_some() && matches!(voice.source, VoiceSource::Input { .. })
            });
            let wanted =
                (frames as f64 * ratio).ceil() as u64 + u64::from(input_fm) * INPUT_FM_HEADROOM;
            let placed = ring.reader_frame(
                self.input_cursor.map(|cursor| cursor.floor() as u64),
                wanted,
            );
            // The fraction survives while the cursor was not moved.
            let base = match self.input_cursor {
                Some(cursor) if cursor.floor() as u64 == placed => cursor,
                _ => placed as f64,
            };
            self.input_cursor = Some(base + frames as f64 * ratio);
            (base, ratio)
        });
        let mut offset = 0;
        while offset < frames {
            let count = self.instrument_revision_frames(frames - offset);
            self.process_segment(
                &mut out[offset * 2..(offset + count) * 2],
                count,
                offset,
                input_base.map(|(base, ratio)| (base + offset as f64 * ratio, ratio)),
            );
            offset += count;
        }
    }

    fn name(&self) -> &'static str {
        "scalar"
    }
}

impl ScalarBackend {
    /// Lease the custom waves of notes given to [`AudioBackend::note`] that
    /// start before this block ends, and let go of those whose notes ended.
    /// Notes from the ring arrive leased, so a live callback has nothing to
    /// do here.
    fn lease_due_waves(&mut self, frames: usize) {
        if !self.leasing_waves {
            return;
        }
        let block_end = self.frame.saturating_add(frames as u64);
        let mut waiting = false;
        for &slot in &self.pending.order {
            let Some(event) = self.pending.slots[slot].event.as_mut() else {
                continue;
            };
            if event
                .controls
                .partials
                .is_none_or(|partials| partials.wave.is_some())
            {
                continue;
            }
            if event.onset_frame >= block_end {
                waiting = true;
                continue;
            }
            let hold = crate::partials::hold_frames(
                event.duration_secs,
                event.controls.envelope.release_secs,
                event.onset_frame,
                kabel_controls(&event.controls)
                    .map(|kabel| kabel.end_secs)
                    .reduce(f64::max),
                self.sample_rate,
            );
            let start = event.onset_frame.max(self.frame);
            if let Some(wave) = event
                .controls
                .partials
                .as_mut()
                .and_then(crate::partials::lease)
            {
                self.wave_leases.hold(wave, start.saturating_add(hold));
            }
        }
        self.wave_leases.release_before(self.frame);
        self.leasing_waves = waiting || !self.wave_leases.is_empty();
    }

    fn instrument_revision_frames(&self, maximum: usize) -> usize {
        if self.pending.order.is_empty() {
            return maximum;
        }
        let mut chains = [None; crate::insert::MAX_INSERT_BUSES];
        let mut keys = [None; crate::insert::MAX_INSERT_BUSES];
        let mut first_onsets = [u64::MAX; crate::insert::MAX_INSERT_BUSES];
        for (bus, chain) in self.insert_chains.iter().enumerate() {
            chains[bus] = *chain;
            if let Some(chain) = chain {
                keys[bus] = self.orbit_inserts
                    [crate::insert::instrument_slot(*chain).physical(bus)]
                .as_ref()
                .map(|insert| insert.key());
                if keys[bus].is_some() {
                    first_onsets[bus] = 0;
                }
            }
        }
        // A due replacement may change the key before this segment renders.
        for &slot in &self.pending.order {
            let event = self.pending.slots[slot]
                .event
                .as_ref()
                .expect("pending event");
            if event.onset_frame <= self.frame
                && let Some(instrument) = &event.controls.instrument
                && let Some(bus) = claim_insert_bus(&mut chains, event.controls.insert_chain())
            {
                keys[bus] = Some(instrument.key);
                first_onsets[bus] = 0;
            }
        }
        let mut end = self.frame.saturating_add(maximum as u64);
        // A new chain needs no boundary until its first instrument changes.
        for &slot in &self.pending.order {
            let event = self.pending.slots[slot]
                .event
                .as_ref()
                .expect("pending event");
            if event.onset_frame <= self.frame || event.onset_frame >= end {
                continue;
            }
            if let Some(instrument) = &event.controls.instrument
                && let Some(bus) = claim_insert_bus(&mut chains, event.controls.insert_chain())
                && event.onset_frame < first_onsets[bus]
            {
                keys[bus] = Some(instrument.key);
                first_onsets[bus] = event.onset_frame;
            }
        }
        for &slot in &self.pending.order {
            let event = self.pending.slots[slot]
                .event
                .as_ref()
                .expect("pending event");
            if event.onset_frame <= self.frame || event.onset_frame >= end {
                continue;
            }
            let Some(instrument) = &event.controls.instrument else {
                continue;
            };
            let key = chains
                .iter()
                .position(|chain| *chain == Some(event.controls.insert_chain()))
                .and_then(|bus| keys[bus]);
            if key != Some(instrument.key) {
                // Keep the previous instrument until the replacement onset.
                end = event.onset_frame;
            }
        }
        (end - self.frame) as usize
    }

    fn process_segment(
        &mut self,
        out: &mut [f32],
        frames: usize,
        block_offset: usize,
        input_base: Option<(f64, f64)>,
    ) {
        enable_denormal_flush();
        // A takeover cut expires once its block has passed the cut frame:
        // the block that contains it is the last one whose activation pass
        // can see an outgoing-generation onset, and a later flip re-arms or
        // clears the field. Checked per block - the arm's whole life is the
        // few blocks the cut needs.
        if let Some((cut_frame, _)) = self.armed_takeover_cut
            && self.frame > cut_frame
        {
            self.armed_takeover_cut = None;
        }
        let routing = self.routing_active();
        let sr = self.sample_rate.max(1) as f64;
        let tables = self
            .tables
            .as_ref()
            .expect("ScalarBackend::init must prepare oscillator tables");
        let sbd_saturation_curve = self
            .sbd_saturation_curve
            .as_deref()
            .expect("ScalarBackend::init must prepare the SBD WaveShaper curve");
        let bank = self
            .bank
            .as_ref()
            .expect("ScalarBackend::init must preload bundled samples");
        let missing_sample_events = &mut self.missing_sample_events;
        let start = self.frame;
        let end = start + frames as u64;
        let sample_rate = self.sample_rate;
        let ducks = &mut self.orbit_ducks;
        let pending_ducks = &mut self.pending_ducks;
        let reverbs = &mut self.orbit_reverbs;
        let insert_chains = &mut self.insert_chains;
        let insert_last_used = &mut self.insert_last_used;
        let insert_fade_from = &mut self.insert_fade_from;
        let insert_refusals = &mut self.insert_refusals;
        let insert_bus_wanted = &mut self.insert_bus_wanted;
        let inserts = &mut self.orbit_inserts;
        let prepared_inserts = &mut self.prepared_inserts;
        let leaving_inserts = &mut self.leaving_inserts;
        let retired_inserts = &mut self.retired_inserts;
        let insert_blocks = &mut self.insert_blocks;
        let insert_dry = &mut self.insert_dry;
        let insert_leaving_blocks = &mut self.insert_leaving_blocks;
        let instrument_blocks = &mut self.instrument_blocks;
        let instrument_reverb_sends = &mut self.instrument_reverb_sends;
        let instrument_delay_sends = &mut self.instrument_delay_sends;
        let insert_postgains = &mut self.insert_postgains;
        let insert_postgain_blocks = &mut self.insert_postgain_blocks;
        let insert_outputs = &mut self.insert_outputs;
        let insert_idle_frames = &mut self.insert_idle_frames;
        let effect_order = &mut self.effect_order;
        let insert_mixes = &mut self.insert_mixes;
        let insert_generations = &mut self.insert_generations;
        let insert_fading = &mut self.insert_fading;
        let insert_values = &self.insert_values;
        let plugin_pool = &mut self.plugin_pool;
        let instrument_to_effect = &mut self.instrument_to_effect;
        let instrument_gains = &mut self.instrument_gains;
        let instrument_filters = &mut self.instrument_filters;
        let instrument_gain_changes = &mut self.instrument_gain_changes;
        instrument_gain_changes.clear();
        let insert_provider = self.insert_provider.as_deref();
        let missing_insert_events = &mut self.missing_insert_events;
        let allow_inline_reverb = self.allow_inline_reverb;
        let offline = self.offline;
        let missing_reverb_events = &mut self.missing_reverb_events;
        let voice_ceiling_drops = &mut self.voice_ceiling_drops;
        let pressure = &mut self.pressure;
        let missing_kabel_events = &mut self.missing_kabel_events;
        let dispatch = self.dispatch;
        let supersaw_kernel = dispatch.supersaw();
        let wavetable_kernel = dispatch.wavetable();
        let voice_count = self.voices.len();
        let mut activated = 0usize;
        let live_control_targets = &self.live_control_targets;

        // Activate due graphs. Most voices activate at source onset. SBD gets
        // one scheduler lead so its connected WaveShaper can establish the
        // non-zero state it produces from zero input (see `sbd_saturation`).
        #[cfg(feature = "device-audio")]
        let confirmations = &mut self.confirmations;
        self.pending.retain_with_confirmation(|e, confirmation, expected_sample_identity| {
            #[cfg(not(feature = "device-audio"))]
            let _ = (confirmation, expected_sample_identity);
            #[cfg(feature = "device-audio")]
            let mut confirmation_sample_matches = true;
            let graph_start_frame =
                if matches!(e.synth, Some(crate::backend::SynthSource::Sbd { .. })) {
                    e.onset_frame
                        .saturating_sub(sbd_graph_lead_frames(sample_rate))
                } else {
                    e.onset_frame
                };
            if graph_start_frame < end {
                #[cfg(feature = "device-audio")]
                let missing_reverbs_before = *missing_reverb_events;
                if voice_count + activated >= MAX_ACTIVE_VOICES {
                    // Ceiling: drop this onset rather than let the mix cost
                    // grow without bound. Retiring voices free slots, so a
                    // dense-but-sane score never reaches this.
                    *voice_ceiling_drops += 1;
                    pressure.voice_ceiling_drops = pressure.voice_ceiling_drops.saturating_add(1);
                    #[cfg(feature = "device-audio")]
                    if let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                        consumer.activation(tag, crate::confirmation::ActivationOutcome::VoiceRefused);
                    }
                    return false;
                }
                if !kabel_available(
                    &self.kabel_pool,
                    &self.kabel_programs,
                    &e.controls,
                    f64::from(self.sample_rate),
                ) {
                    // Asset installs and onsets use independent SPSC queues. A
                    // producer may publish this onset after this block's asset
                    // snapshot. Keep it pending until the next install pass.
                    // Its program identity remains protected from retirement.
                    let deadline = kabel_controls(&e.controls)
                        .map(|controls| (controls.end_secs.max(0.0) * sr).ceil() as u64)
                        .max()
                        .unwrap_or(e.onset_frame)
                        .saturating_add(256);
                    if start < deadline {
                        return true;
                    }
                    *missing_kabel_events += 1;
                    #[cfg(feature = "device-audio")]
                    if let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                        consumer.activation(tag, crate::confirmation::ActivationOutcome::VoiceRefused);
                    }
                    return false;
                }
                let mut event = *e;
                if event.controls.live_controls != [0; 4] {
                    for target in live_control_targets {
                        if event.controls.live_controls[0] == target.binding {
                            event.gain = target.value;
                        }
                        if let Some(filter) = event.controls.filters.lowpass.as_mut() {
                            if event.controls.live_controls[1] == target.binding {
                                filter.frequency_hz = target.value;
                            }
                            if event.controls.live_controls[2] == target.binding {
                                filter.q = target.value;
                            }
                        }
                        if let Some(fm) = event.controls.fm.as_mut() {
                            for route in fm.routes.iter_mut().flatten() {
                                if route.mod_slot == Some(0)
                                    && event.controls.live_controls[3] == target.binding
                                {
                                    route.amount = target.value;
                                }
                            }
                        }
                    }
                }
                let mut live_gain = crate::live_control::Ramp::new(event.controls.live_controls[0], event.gain);
                let instrument_gain = live_gain;
                let live_cutoff = crate::live_control::Ramp::new(
                    event.controls.live_controls[1],
                    event.controls.filters.lowpass.map_or(0.0, |filter| filter.frequency_hz),
                );
                let live_resonance = crate::live_control::Ramp::new(
                    event.controls.live_controls[2],
                    event.controls.filters.lowpass.map_or(0.0, |filter| filter.q),
                );
                // The binding rides the direct route (`fm`, `fmi`): operator 1
                // into the carrier. A value of zero carries no route, and the
                // ramp stays inert until a retrigger brings one.
                let live_fm_index = crate::live_control::Ramp::new(
                    event.controls.live_controls[3],
                    event
                        .controls
                        .fm
                        .and_then(|fm| {
                            fm.routes
                                .iter()
                                .flatten()
                                .find(|route| route.mod_slot == Some(0))
                                .map(|route| route.amount)
                        })
                        .unwrap_or(0.0),
                );
                // Keep the source's unit-gain amplitude even when the slider
                // begins at zero, so a sustained silent note can fade in.
                // Dynamic gain is applied at the same point in the chain.
                if live_gain.binding != 0 {
                    event.gain = 1.0;
                } else {
                    live_gain = crate::live_control::Ramp::new(0, 1.0);
                }
                if event.controls.instrument.is_some() {
                    // The plugin supplies the sound, including while its
                    // copy is loading. Do not add a native oscillator.
                    event.gain = 0.0;
                }
                let e = &event;
                let (left_gain, right_gain) = Self::channel_gains(e.controls.pan);
                let mut filters = FilterChain::new(e.controls.filters, self.sample_rate);
                filters.set_block_phase(start);
                let has_filters = filters.is_active();
                // Built-in oscillators stop 10 ms after the
                // amplitude release. Chromium can then render a downstream
                // biquad tail until the graph teardown crosses render-quantum
                // boundaries, so retain two 128-frame quanta of zero-input
                // filter state. The unfiltered path still ends at release.
                let (source, duration_secs, amplitude_scale, natural_stop_secs) =
                    if let Some(synth) = e.synth {
                        match synth {
                            crate::backend::SynthSource::Supersaw {
                                voices,
                                freqspread,
                                panspread,
                            } => {
                                let plan = PreparedSupersaw::new(voices, freqspread, panspread);
                                let mut phases = [0.0f32; MAX_UNISON];
                                for (n, phase) in phases.iter_mut().enumerate().take(plan.count) {
                                    // Seeded, like the wavetable phases, so
                                    // renders are reproducible.
                                    *phase = seeded_phase(e.onset_frame, n);
                                }
                                let amplitude_scale =
                                    e.gain * e.controls.velocity * 0.3 / plan.voices.sqrt();
                                (
                                    VoiceSource::Supersaw { plan, phases },
                                    e.duration_secs,
                                    // env peak 0.3 × 1/√voices.
                                    amplitude_scale,
                                    None,
                                )
                            }
                            crate::backend::SynthSource::Pulse {
                                pulsewidth,
                                width_lfo,
                            } => (
                                VoiceSource::Pulse {
                                    pulsewidth,
                                    width_lfo,
                                    width_lfo_phase: None,
                                    // The pulse pair starts half a turn
                                    // out (phi = -PI), not at zero.
                                    phi: -std::f64::consts::PI,
                                    y0: 0.0,
                                    y1: 0.0,
                                    dphif: 0.0,
                                    envf: 0.0,
                                    env: 1.0,
                                },
                                e.duration_secs,
                                // The 0.15 factor is inside the algorithm.
                                // Unlike the table oscillators, pulse runs
                                // its envelope to 1 rather than 0.3, which
                                // is the whole of the level difference.
                                e.gain * e.controls.velocity,
                                None,
                            ),
                            crate::backend::SynthSource::ByteBeat {
                                expression,
                                program,
                                start_offset,
                            } => (
                                VoiceSource::ByteBeat {
                                    expression,
                                    program,
                                    start_offset: start_offset.unwrap_or(0.0),
                                    // byteBeatStartTime, when present,
                                    // resets `t`; otherwise the worklet seeds
                                    // it with `params.begin[0] * sampleRate`,
                                    // which is the note's begin as the
                                    // AudioParam holds it - f32, and NOT
                                    // rounded to a frame. An onset that falls
                                    // between two samples seeds a fraction,
                                    // and the counter carries that fraction
                                    // through every byte it evaluates.
                                    t: if start_offset.is_some() {
                                        0.0
                                    } else {
                                        f64::from(e.controls.worklet_begin_secs)
                                            * f64::from(self.sample_rate)
                                    },
                                    gate: first_open_quantum(
                                        e.controls.worklet_begin_secs,
                                        self.sample_rate as f64,
                                    ),
                                },
                                e.duration_secs,
                                // The bytebeat byte clamp is the only
                                // shaping; an ordinary synth ADSR rides on
                                // top.
                                e.gain * e.controls.velocity,
                                None,
                            ),
                            crate::backend::SynthSource::ZzFx { params } => {
                                let mut params = params;
                                // The random draw is seeded from the onset
                                // like every other stochastic source, so
                                // zrand(0) stays bit-comparable.
                                params.random_draw = f64::from(seeded_phase(e.onset_frame, 0));
                                let voice = crate::zzfx::ZzfxVoice::new(
                                    &params,
                                    f64::from(self.sample_rate),
                                );
                                let natural =
                                    (voice.frames() as f32 / self.sample_rate as f32).max(0.0);
                                let ring = (params.delay > 0.0)
                                    .then(|| self.zzfx_ring_pool.pop())
                                    .flatten();
                                if params.delay > 0.0 && ring.is_none() {
                                    pressure.pool_miss(RealtimePool::ZzfxDelay);
                                }
                                (
                                    VoiceSource::ZzFx { voice, ring },
                                    // The buffer runs to its own end whatever
                                    // the hap lasts: hold the (flat) envelope
                                    // for the whole of it.
                                    natural,
                                    // volume 0.25 is baked into the params;
                                    // the chain gain rides on top, no 0.3.
                                    e.gain * e.controls.velocity,
                                    Some(natural),
                                )
                            }
                            crate::backend::SynthSource::Bus { bus } => (
                                VoiceSource::Bus { bus },
                                e.duration_secs,
                                // `gainNode(0)` under an ADSR peaking at 1 -
                                // no 0.3 scale, unlike the synth sources.
                                e.gain * e.controls.velocity,
                                None,
                            ),
                            crate::backend::SynthSource::Input { channel } => (
                                VoiceSource::Input {
                                    channel,
                                    fm_phase: 0.0,
                                },
                                e.duration_secs,
                                // A live signal at its own level, under the
                                // ordinary envelope. postgain is NOT folded
                                // in here: like every other source it rides
                                // the chain tap after the stages, and folding
                                // it would apply it twice (and move the
                                // level the FX stages see).
                                e.gain * e.controls.velocity,
                                None,
                            ),
                            crate::backend::SynthSource::Noise { kind, density } => (
                                VoiceSource::Noise {
                                    kind,
                                    density,
                                    state: NoiseGen::at_onset(self.sample_rate, e.onset_lead),
                                },
                                e.duration_secs,
                                // gainNode(0.3) then an ADSR peaking at 1,
                                // the ordinary synth shape.
                                e.gain * e.controls.velocity * 0.3,
                                None,
                            ),
                            crate::backend::SynthSource::One => (
                                VoiceSource::One,
                                e.duration_secs,
                                // The ordinary synth shape, as for noise.
                                e.gain * e.controls.velocity * 0.3,
                                None,
                            ),
                            crate::backend::SynthSource::Sbd {
                                decay_secs,
                                pdecay_secs,
                                penv_semitones,
                                stop_secs,
                            } => (
                                VoiceSource::Sbd {
                                    controls: SbdState {
                                        decay_secs,
                                        pdecay_secs,
                                        penv_semitones,
                                        stop_secs,
                                    },
                                    phase: 0.0,
                                    // Every SBD voice replays the session's
                                    // cached brown-noise buffer from the
                                    // beginning. `NoiseGen::at_onset` restarts
                                    // the prepared stream at each hit.
                                    noise: NoiseGen::at_onset(self.sample_rate, e.onset_lead),
                                },
                                // SBD runs its own envelopes; the standard
                                // ADSR must stay at unity until the stop.
                                stop_secs,
                                e.gain * e.controls.velocity,
                                Some(stop_secs),
                            ),
                        }
                    } else if let Some(wavetable) = e.wavetable {
                        #[cfg(feature = "device-audio")]
                        if let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                            confirmation_sample_matches = bank.get(wavetable.table)
                                .is_some_and(|decoded| expected_sample_identity == Some(decoded.identity()));
                            if !confirmation_sample_matches {
                                consumer.activation(tag, crate::confirmation::ActivationOutcome::MissingSample);
                            }
                        }
                        // The wavetable synth follows the OSCILLATOR gain
                        // path (ADSR to 0.3) and the hap gate; phases seed
                        // deterministically when phaserand is set.
                        let mut phases = [0.0f64; MAX_UNISON];
                        if wavetable.phaserand > 0.0 {
                            for (n, phase) in phases
                                .iter_mut()
                                .enumerate()
                                .take((wavetable.voices.max(1.0).ceil() as usize).min(MAX_UNISON))
                            {
                                *phase = f64::from(
                                    seeded_phase(e.onset_frame, n)
                                        * wavetable.phaserand.clamp(0.0, 1.0),
                                );
                            }
                        }
                        (
                            VoiceSource::Wavetable {
                                controls: wavetable,
                                phases,
                                lfo_phase: None,
                                warp_lfo_phase: None,
                            },
                            e.duration_secs,
                            e.gain * e.controls.velocity * 0.3,
                            None,
                        )
                    } else if let Some(sample) = e.sample {
                        let Some(decoded) = bank.get(sample.sample) else {
                            // Not installed yet: skip only this onset, as the
                            // reference does when a sound misses its start
                            // time. Later triggers find it loaded.
                            *missing_sample_events += 1;
                            #[cfg(feature = "device-audio")]
                            if let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                                consumer.activation(tag, crate::confirmation::ActivationOutcome::MissingSample);
                            }
                            return false;
                        };
                        // A previous payload in the same bank slot must not
                        // certify this candidate's required body. Keep the
                        // existing playback policy even when receipt evidence
                        // is missing or mismatched.
                        #[cfg(feature = "device-audio")]
                        if let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                            confirmation_sample_matches = expected_sample_identity == Some(decoded.identity());
                            if !confirmation_sample_matches {
                                consumer.activation(tag, crate::confirmation::ActivationOutcome::MissingSample);
                            }
                        }
                        let source_frames = decoded.frames() as f64;
                        let playback_rate = f64::from(sample.playback_rate);
                        let begin = begin_frame(sample.begin, source_frames, playback_rate);
                        let (duration_secs, natural_stop_secs) =
                            sample.duration_and_natural_stop(decoded, e.duration_secs);
                        let loop_frames = sample.loop_secs.map(|(start, end)| {
                            (
                                f64::from(start) * f64::from(decoded.sample_rate()),
                                f64::from(end) * f64::from(decoded.sample_rate()),
                            )
                        });
                        let increment = playback_rate * f64::from(decoded.sample_rate()) / sr;
                        let (delay, lead) = nudged_start(sample.nudge_secs, e.onset_lead, sr);
                        (
                            VoiceSource::Sample {
                                sample: sample.sample,
                                position: begin + lead * increment,
                                increment,
                                reversed: sample.reversed,
                                delay_frames: delay,
                                loop_frames,
                            },
                            duration_secs,
                            e.gain * e.controls.velocity * sample.envelope_peak,
                            natural_stop_secs,
                        )
                    } else {
                        (
                            VoiceSource::Oscillator {
                                table_selection: match e
                                    .controls
                                    .partials
                                    .and_then(|partials| partials.wave)
                                {
                                    Some(wave) => wave.wave().selection(e.freq_hz, sr as f32),
                                    None => tables.selection(e.freq_hz),
                                },
                                phase: f64::from(e.onset_lead * e.freq_hz) / sr,
                                partial_limit: 0,
                            },
                            e.duration_secs,
                            e.gain * e.controls.velocity * 0.3,
                            None,
                        )
                    };
                let mut source_stop_secs = duration_secs + e.controls.envelope.release_secs;
                if let Some(natural_stop_secs) = natural_stop_secs {
                    source_stop_secs = source_stop_secs.min(natural_stop_secs);
                }
                if let Some(end) = kabel_controls(&e.controls).map(|kabel| kabel.end_secs).reduce(f64::max) {
                    // Worklets silence their outputs after their final quantum,
                    // even if the underlying sample would have played longer.
                    let end_frame = (end * sr / 128.0).ceil() * 128.0;
                    source_stop_secs = ((end_frame - e.onset_frame as f64) / sr) as f32;
                }
                let stop_secs = stop_secs_for(
                    source_stop_secs,
                    has_filters || e.controls.vowel.is_some(),
                    self.sample_rate as f32,
                );
                let reverb_wet = match e.controls.reverb {
                    Some(reverb) if reverb.wet > 0.0 => {
                        let orbit = (e.controls.orbit as usize).min(MAX_ORBITS - 1);
                        // A custom IR whose sample is not decoded yet
                        // falls back to the generated IR for THIS trigger
                        // (never-die: no awaiting a fetch mid-render). The
                        // LIVE path's producer installs generated-IR reverbs
                        // only, so live strips the ir instead of playing dry.
                        let ir = if allow_inline_reverb {
                            reverb.ir.filter(|ir| bank.get(ir.sample).is_some())
                        } else {
                            None
                        };
                        let wanted = crate::reverb::ReverbParams {
                            size_secs: reverb.size_secs,
                            fade_secs: reverb.fade_secs,
                            lp_start_hz: reverb.lp_start_hz,
                            lp_end_hz: reverb.lp_end_hz,
                            ir,
                        };
                        let installed = reverbs.get(orbit).and_then(|slot| slot.as_ref());
                        if installed.is_some_and(|existing| {
                            !allow_inline_reverb || existing.params() == wanted
                        }) {
                            reverb.wet
                        } else if allow_inline_reverb {
                            // Offline: synthesise at activation, on the
                            // producer side of the pipeline.
                            reverbs[orbit] = Some(Box::new(match ir {
                                Some(params_ir) => match bank.get(params_ir.sample) {
                                    Some(decoded) => crate::reverb::OrbitReverb::generate_custom_with_dispatch(
                                        sample_rate,
                                        wanted,
                                        decoded,
                                        dispatch,
                                    ),
                                    None => crate::reverb::OrbitReverb::generate_with_dispatch(
                                        sample_rate,
                                        crate::reverb::ReverbParams { ir: None, ..wanted },
                                        dispatch,
                                    ),
                                },
                                None => crate::reverb::OrbitReverb::generate_with_dispatch(sample_rate, wanted, dispatch),
                            }));
                            reverb.wet
                        } else {
                            // Live and not installed yet: this onset plays
                            // dry; the producer's install lands for later
                            // triggers. Never fatal.
                            *missing_reverb_events += 1;
                            pressure.orbit_reverb_misses =
                                pressure.orbit_reverb_misses.saturating_add(1);
                            0.0
                        }
                    }
                    _ => 0.0,
                };
                // Gives the insert of a slot what the note asks for. False
                // means the slot does not hold the insert, and the note
                // plays with no insert.
                let chain = e.controls.insert_chain();
                let wants_insert = e.controls.instrument.is_some() || e.controls.effects.iter().any(Option::is_some);
                let insert_bus = wants_insert.then(|| claim_insert_bus(insert_chains, chain)).flatten();
                if let Some(bus) = insert_bus {
                    insert_last_used[bus] = start;
                    insert_fade_from[bus] = u64::MAX;
                }
                else if wants_insert {
                    *insert_refusals += 1;
                    // A chain the host names waits for the bus of a chain
                    // it does not name: see `reclaim_insert_chains`.
                    *insert_bus_wanted |= self.insert_named.contains(&chain);
                }
                // The chain of an orbit has a number below MAX_ORBITS.
                let own_chain = chain >= MAX_ORBITS as u64;
                // A note older than the note that set the copies and the
                // order of the chain changes neither, and goes only through
                // the stages the chain runs. The chain is heard while its
                // effects sound or take the sound of a voice, or while its
                // instrument sounds.
                let older = insert_bus.is_some_and(|bus| e.generation < insert_generations[bus]);
                let current = insert_bus.map_or([crate::insert::NO_STAGE; crate::insert::EFFECT_CHAIN], |bus| effect_order[bus]);
                let heard = insert_bus.is_some_and(|bus| {
                    let level = &insert_postgains[bus];
                    let gain = &instrument_gains[bus];
                    let slot = crate::insert::instrument_slot(chain).physical(bus);
                    (current[0] != crate::insert::NO_STAGE && level.quiet.min(level.unfed) < INSERT_QUIET_FRAMES)
                        || (inserts[slot].as_ref().is_some_and(|held| held.key() == gain.key)
                            && gain.quiet < INSERT_QUIET_FRAMES)
                });
                let mut serve = |wanted: &crate::insert::InsertControls, logical: crate::insert::InsertSlot| {
                    let Some(bus) = insert_bus else { *missing_insert_events += 1; return false; };
                    let slot = logical.physical(bus);
                    let Some(held) = inserts.get_mut(slot) else {
                        return false;
                    };
                    // An effect stage the chain does not run makes no sound.
                    let effect = usize::from(logical.stage) < crate::insert::EFFECT_CHAIN;
                    let runs = !effect || current.contains(&logical.stage);
                    let heard = heard && runs;
                    // A newer note takes the copy of its key. The copy it
                    // displaces fades out when heard, else it goes back now.
                    let swaps = !older && held.as_ref().is_none_or(|held| held.key() != wanted.key);
                    let mut swapped = false;
                    if swaps
                        && prepared_inserts[slot]
                            .as_ref()
                            .is_some_and(|prepared| prepared.key() == wanted.key)
                        && displace_insert(held, &mut leaving_inserts[slot], &mut insert_mixes[slot], retired_inserts, heard, sample_rate)
                    {
                        *held = prepared_inserts[slot].take();
                        swapped = true;
                    }
                    // Offline: build at activation, as the inline reverb
                    // does. Live waits for the producer's install.
                    if allow_inline_reverb
                        && swaps
                        && held.as_ref().is_none_or(|held| held.key() != wanted.key)
                        && let Some(built) =
                            insert_provider.and_then(|build| build(wanted.key, sample_rate, logical))
                        && displace_insert(held, &mut leaving_inserts[slot], &mut insert_mixes[slot], retired_inserts, heard, sample_rate)
                    {
                        *held = Some(built);
                        swapped = true;
                    }
                    if swapped && heard {
                        // A new effect fades in. A new instrument starts
                        // silent, so it starts at its level.
                        if effect {
                            let active = &mut insert_mixes[slot].active;
                            *active = crate::live_control::Ramp::new(0, 1.0);
                            active.declick_from(crate::live_control::Ramp::new(0, 0.0), sample_rate);
                        }
                        *insert_fading |= 1u64 << bus;
                    }
                    let Some(held) = held.as_mut().filter(|held| held.key() == wanted.key && (runs || !older)) else {
                        *missing_insert_events += 1;
                        return false;
                    };
                    // The frames from the block start to the note.
                    let frames = e.onset_frame.saturating_sub(start);
                    let frames = frames.min(u64::from(u32::MAX)) as u32;
                    deliver(held.as_mut(), logical, wanted, e.generation, insert_values, frames);
                    insert_idle_frames[slot] = 0;
                    true
                };
                // The stages of the chain that hold the effect the note
                // asks for, in the order of its calls and each one time.
                // The note goes through these and no other.
                let mut order = [crate::insert::NO_STAGE; crate::insert::EFFECT_CHAIN];
                let mut served = 0;
                for (place, wanted) in e.controls.effects.iter().enumerate() {
                    let Some(wanted) = wanted else { continue };
                    let stage = wanted.stage(place);
                    if !order[..served].contains(&(stage as u8))
                        && serve(wanted, crate::insert::effect_slot(chain, stage))
                    {
                        order[served] = stage as u8;
                        served += 1;
                    }
                }
                let through_insert = order[0] != crate::insert::NO_STAGE;
                let instrument = e.controls.instrument.filter(|wanted| serve(wanted, crate::insert::instrument_slot(chain)));
                // A newer note sets the order. Only a chain of its own takes
                // the empty order of an instrument note with no effect.
                if let Some(bus) = insert_bus.filter(|_| !older) {
                    insert_generations[bus] = e.generation;
                    if through_insert || (own_chain && e.controls.instrument.is_some()) {
                        order_insert_stages(&mut effect_order[bus], insert_mixes, insert_fading, bus, &order, heard, sample_rate);
                    }
                }
                if through_insert && let Some(bus) = insert_bus {
                    insert_outputs[bus] = usize::from(e.controls.orbit).min(MAX_ORBITS - 1);
                }
                let muted = e.sample.is_some_and(|sample| sample.muted);
                let (delay_wet, orbit) = match e.controls.delay {
                    Some(delay) => {
                        let orbit = (e.controls.orbit as usize).min(MAX_ORBITS - 1);
                        let bus = &mut self.orbit_delays[orbit];
                        // Round, never truncate. The resolved time is f32,
                        // so 0.08 arrives as 3839.9999 frames and 0.15 as
                        // 7200.0003. A truncation leaves the first one
                        // sample short. Chromium is exactly nominal for
                        // both (impulse probe).
                        bus.time_frames = ((f64::from(delay.time_secs.clamp(0.0, 1.0))
                            * f64::from(self.sample_rate))
                        .round() as usize)
                            // A 1 Hz rate leaves no room above one frame,
                            // and the clamp stays a range.
                            .clamp(1, (self.sample_rate as usize).max(2) - 1);
                        bus.feedback = delay.feedback.clamp(0.0, 0.98);
                        bus.active = true;
                        (delay.wet.abs(), orbit)
                    }
                    // The voice's orbit routes its output mix (duck stage)
                    // as well as the delay send, so it is always the real
                    // orbit, also when the voice has no delay.
                    None => (0.0, (e.controls.orbit as usize).min(MAX_ORBITS - 1)),
                };
                if let Some(wanted) = instrument
                    && let Some(bus) = insert_bus
                {
                    // The instrument also goes through the stages that fade
                    // out on a chain of its own.
                    instrument_to_effect[bus] = through_insert
                        || (own_chain && effect_order[bus][0] != crate::insert::NO_STAGE);
                    insert_outputs[bus] = usize::from(e.controls.orbit).min(MAX_ORBITS - 1);
                    let frame = e.onset_frame.saturating_sub(start) as usize;
                    let index = instrument_gain_changes.partition_point(|change| {
                        (change.bus, change.frame) <= (bus, frame)
                    });
                    instrument_gain_changes.insert(index, InstrumentGainChange {
                        bus,
                        frame,
                        value: InstrumentGain {
                            key: wanted.key,
                            gain: instrument_gain,
                            postgain: crate::live_control::Ramp::new(0, e.controls.postgain),
                            reverb_wet: crate::live_control::Ramp::new(0, reverb_wet),
                            delay_wet: crate::live_control::Ramp::new(0, delay_wet),
                            dry: crate::live_control::Ramp::new(0, e.controls.dry.unwrap_or(1.0)),
                            filters: e.controls.filters,
                            cutoff: live_cutoff,
                            resonance: live_resonance,
                            pan: e.controls.pan,
                            pan_at: crate::live_control::Ramp::new(0, e.controls.pan.unwrap_or(0.5)),
                            onset: e.onset_frame,
                            duration_secs: e.duration_secs,
                            quiet: 0,
                        },
                    });
                }
                if muted {
                    // Chain side effects applied (duck armed at intake,
                    // delay params and reverb above); the source itself is
                    // the speed-zero no-op.
                    let _ = (source, duration_secs, amplitude_scale, natural_stop_secs);
                    let _ = (delay_wet, orbit);
                    #[cfg(feature = "device-audio")]
                    if confirmation_sample_matches && let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                        consumer.activation(tag, crate::confirmation::ActivationOutcome::Muted);
                    }
                    return false;
                }
                let source_stereo = match &source {
                    VoiceSource::Wavetable { .. }
                    | VoiceSource::Supersaw { .. }
                    | VoiceSource::ByteBeat { .. }
                    // A bus mixes in stereo, so its taps are stereo even
                    // when everything feeding it was mono.
                    | VoiceSource::Bus { .. }
                    | VoiceSource::Pulse { .. } => true,
                    VoiceSource::Sample { sample, .. } => bank
                        .get(*sample)
                        .is_some_and(|decoded| decoded.channels() == 2),
                    VoiceSource::Oscillator { .. }
                    | VoiceSource::Sbd { .. }
                    | VoiceSource::Input { .. }
                    // A ZzFX note is ONE channel by construction.
                    | VoiceSource::ZzFx { .. }
                    | VoiceSource::Noise { .. }
                    | VoiceSource::One => false,
                };
                let stereo_source = source_stereo || e.controls.kabel.is_some()
                // A StereoPanner UPMIXES: a mono voice that reaches one comes
                // out stereo, and everything after it is two channels. The
                // mono chain carries a single sample and applies pan only at
                // the very end from `left_gain`/`right_gain`, so a per-stage
                // pan on a mono voice would be computed and then thrown away.
                // Naming pan in any stage therefore makes the voice stereo
                // from the start.
                //
                // A stage REVERB needs it for the same reason: its impulse
                // response is two decorrelated channels, so it turns one
                // channel into two that differ. Left mono, the right half is
                // computed and then dropped by the mono chain, which both
                // silences the decorrelation and leaves the wrong channel's
                // wet signal in place.
                || e.controls.fx_stages.iter().flatten().any(|stage| {
                    stage.kabel.is_some() || stage.pan_x.is_some() || stage.room.is_some()
                });
                let mut voice_controls = e.controls;
                if matches!(source, VoiceSource::Sbd { .. }) {
                    // sbd runs its own amplitude envelope; the standard
                    // ADSR is never wired for it.
                    voice_controls.envelope = crate::backend::Envelope {
                        attack_secs: 0.0,
                        decay_secs: 0.0,
                        sustain: 1.0,
                        release_secs: 0.01,
                    };
                }
                if let Some(djf) = e.controls.djf {
                    // Create-once; each trigger just sets the value.
                    let state = &mut self.orbit_djf[orbit];
                    state.active = true;
                    state.value = djf;
                }
                Self::choke_previous(&mut self.voices, e, start, sample_rate);
                // Stages that wanted a reverb and found none play dry; counted
                // through the same channel as an orbit reverb that was not
                // ready, so one number covers both.
                let mut missed_stage_reverb = 0u64;
                // The first score voice into an orbit keys its routing for good;
                // `channels` on any later voice does nothing. Ordering
                // matters and is the trigger order.
                if !e.controls.piano && self.orbit_channels[orbit].is_none() {
                    self.orbit_channels[orbit] = Some(e.controls.channels.unwrap_or([1, 2]));
                }
                let faded =
                    Self::cull_for_polyphony(&mut self.voices, e.onset_frame, self.sample_rate, self.max_polyphony.0);
                pressure.semantic_polyphony_fades = pressure
                    .semantic_polyphony_fades
                    .saturating_add(u64::try_from(faded).unwrap_or(u64::MAX));
                // Lease before the Voice literal so the FX-stage block can
                // borrow the same pool without overlapping the main-chain pop.
                let kabel = e.controls.kabel.map(|controls| lease_kabel(&mut self.kabel_pool, &self.kabel_programs, controls, f64::from(self.sample_rate)));
                let stretch = e.controls.stretch.and_then(|_| self.stretch_pool.pop());
                let fx_stage_state = if voice_controls.fx_stages.iter().all(Option::is_none) {
                    None
                } else {
                    let mut stages = self
                        .fx_stage_state_pool
                        .pop()
                        .expect("prepared FX-stage state pool exhausted");
                    let sample_rate = self.sample_rate;
                    let onset_frame = e.onset_frame;
                    let pool = &mut self.compressor_pool;
                    let delay_pool = &mut self.fx_delay_pool;
                    let reverb_pool = &mut self.fx_reverb_pool;
                    let reverb_inline_count = &mut self.fx_reverb_inline_count;
                    let reverb_wanted = &mut self.fx_reverb_wanted;
                    let stretch_pool = &mut self.stretch_pool;
                    let missed = &mut missed_stage_reverb;

                    let mut source_is_mono = !source_stereo;
                    for (slot, stage) in stages.iter_mut().zip(voice_controls.fx_stages) {
                        *slot = stage.map(|stage| {
                            if stage.kabel.is_some() {
                                source_is_mono = false;
                            }
                            let stage_source_is_mono = source_is_mono;
                            if stage.pan_x.is_some() || stage.room.is_some() {
                                source_is_mono = false;
                            }
                            FxStageState {
                            kabel: stage.kabel.map(|controls| lease_kabel(&mut self.kabel_pool, &self.kabel_programs, controls, f64::from(self.sample_rate))),
                            controls: stage,
                            filters: {
                                let mut chain = FilterChain::new(stage.filters, sample_rate);
                                chain.set_block_phase(start);
                                chain
                            },
                            filters_right: stereo_source.then(|| {
                                let mut chain = FilterChain::new(stage.filters, sample_rate);
                                chain.set_block_phase(start);
                                chain
                            }),
                            coarse_hold: [0.0; 2],
                            vowel_coefs: match stage.vowel {
                                Some(vw) => std::array::from_fn(|k| {
                                    bandpass_coefs(vw.freqs[k], vw.qs[k], sample_rate as f32)
                                }),
                                None => [NotchCoefs::default(); 5],
                            },
                            vowel_filters: [[NotchState::default(); 5]; 2],
                            tremolo_phase: None,
                            phaser_phase: None,
                            phaser_filters: [NotchState::default(); 2],
                            // Drawn from the same pool as the main chain's.
                            // An exhausted pool means the stage plays
                            // uncompressed rather than not at all.
                            compressor: stage.compressor.and_then(|c| {
                                pool.pop().map(|mut state| {
                                    *state =
                                        CompressorState::new(sample_rate as f32, &c, onset_frame);
                                    state
                                })
                            }),
                            source_is_mono: stage_source_is_mono,
                            stretch: stage.stretch.and_then(|_| stretch_pool.pop()),
                            transient: stage
                                .transient
                                .map(|c| TransientState::new(c, sample_rate as f32)),
                            // An exhausted pool means the stage plays dry
                            // rather than not at all.
                            delay: stage.delay.and_then(|_| {
                                delay_pool.pop().map(|mut line| {
                                    line.reset();
                                    line
                                })
                            }),
                            room: stage.room.and_then(|room| {
                                let params = crate::reverb::ReverbParams {
                                    ir: room.ir,
                                    size_secs: room.size_secs,
                                    fade_secs: room.fade_secs,
                                    lp_start_hz: room.lp_start_hz,
                                    lp_end_hz: room.lp_end_hz,
                                };
                                match reverb_pool.iter().position(|r| r.params() == params) {
                                    Some(index) => {
                                        let mut reverb = reverb_pool.swap_remove(index);
                                        reverb.reset();
                                        Some(reverb)
                                    }
                                    // Offline: synthesise at activation,
                                    // exactly as the orbit reverbs above do.
                                    None if allow_inline_reverb => {
                                        // Streaming: a stage is fed a sample
                                        // at a time, so its head is convolved
                                        // in the time domain.
                                        let reverb = Box::new(
                                            crate::reverb::OrbitReverb::generate_streaming_with_dispatch(
                                                sample_rate,
                                                crate::reverb::ReverbParams { ir: None, ..params },
                                                dispatch,
                                            ),
                                        );
                                        *reverb_inline_count = reverb_inline_count
                                            .checked_add(1)
                                            .expect("FX reverb inline count overflow");
                                        reserve_fx_reverb_returns(
                                            reverb_pool,
                                            *reverb_inline_count,
                                        );
                                        Some(reverb)
                                    }
                                    None => {
                                        // Live: play dry and ask for one. The
                                        // next hap with these params gets it.
                                        if reverb_wanted.len() < 8
                                            && !reverb_wanted.contains(&params)
                                        {
                                            reverb_wanted.push(params);
                                        }
                                        *missed += 1;
                                        None
                                    }
                                }
                            }),
                        }});
                    }
                    Some(stages)
                };
                let fm_state = voice_controls.fm.map(|fm| {
                    let mut state = self
                        .fm_state_pool
                        .pop()
                        .expect("prepared FM state pool exhausted");
                    state.restart(e.onset_frame, self.sample_rate, &fm, e.freq_hz);
                    state
                });
                self.next_voice_id += 1;
                let mut voice = Voice {
                    live_gain,
                    live_cutoff,
                    live_resonance,
                    live_fm_index,
                    graph_start_frame,
                    start_frame: e.onset_frame,
                    generation: e.generation,
                    replayed_by: None,
                    replayed_on_next_frame: false,
                    ui_visuals: e.ui_visuals,
                    preview_epoch: e.controls.preview_epoch,
                    piano: e.controls.piano,
                    plain: false,
                    onset_lead: e.onset_lead,
                    distort_curve: e.controls.distort.map(|d| d.curve(d.shape())),
                    delay_wet,
                    reverb_wet,
                    through_insert,
                    insert_bus,
                    dry_gain: e.controls.dry.unwrap_or(1.0),
                    kabel,
                    // Leased from the pool built at init. An exhausted pool
                    // means the voice plays without the vocoder rather than
                    // allocating FFT state under the callback deadline.
                    stretch,
                    orbit,
                    freq_hz: e.freq_hz,
                    duration_secs,
                    hap_duration_secs: e.duration_secs,
                    controls: VoiceRenderControls::from(voice_controls),
                    has_param_modulators: voice_controls.lfos.iter().any(Option::is_some)
                        || voice_controls.envs.iter().any(Option::is_some)
                        || voice_controls.bus_mods.iter().any(Option::is_some),
                    source,
                    amplitude_scale,
                    // The main stage's own gain belongs AFTER the `.FX()`
                    // stages - it lives inside the last FX entry, which is
                    // the hap's own params. Folding it into
                    // `amplitude_scale` up front would hand every stage a
                    // signal 0.8x too quiet, which linear effects forgive
                    // and crush, shape and distort do not.
                    fx_post_gain: e.gain * e.controls.velocity,
                    gate_value: Self::gate_value(duration_secs, voice_controls.envelope),
                    left_gain,
                    right_gain,
                    pan_x: e.controls.pan.map(|pan| 2.0 * pan - 1.0).unwrap_or(0.0),
                    filters_right: stereo_source.then(|| {
                        let mut chain = FilterChain::new(voice_controls.filters, self.sample_rate);
                        chain.set_block_phase(start);
                        chain
                    }),
                    filters,
                    has_filters,
                    has_pre_distort_fx: voice_controls.vowel.is_some()
                        || voice_controls.coarse.is_some()
                        || voice_controls.crush.is_some()
                        || voice_controls.shape.is_some(),
                    stop_secs,
                    mod_lfo_phases: {
                        let mut phases = [0.0f64; crate::backend::MAX_VOICE_MODS];
                        for (slot, phase) in voice_controls.lfos.iter().zip(phases.iter_mut()) {
                            if let Some(lfo) = slot {
                                *phase = f64::from(lfo.phase0);
                            }
                        }
                        phases
                    },
                    sample_pitch_hold: None,
                    phaser_phase: None,
                    phaser_notch: [NotchState::default(); 2],
                    tremolo_phase: None,
                    mod_param_hold: ModParamAdds::default(),
                    transient: e
                        .controls
                        .transient
                        .map(|c| TransientState::new(c, self.sample_rate as f32)),
                    fx_stage_state,
                    vibrato_phase: 0.0,
                    // The noise-mix pink source starts at the note's own
                    // time, so it reads the buffer between samples; the FM
                    // modulators below start frame-aligned and do not.
                    source_noise: NoiseGen::at_onset(self.sample_rate, e.onset_lead),
                    // A modulator feeds ONLY the carrier's frequency param,
                    // and a node feeding a param alone is not pulled until
                    // the owning node evaluates it - which an oscillator
                    // skips entirely while it is still before its start
                    // time. So a modulator first runs in the quantum where
                    // its carrier starts, at the phase it reached from the
                    // render start, and notes a whole modulator cycle apart
                    // are bit-identical because of it.
                    fm_state,
                    pitch_quantum_skip: {
                        // The onset's offset within its 128-frame quantum:
                        // the pitch envelope runs that many frames late
                        // until the next boundary (see the apply site). A
                        // voice activated after that boundary has nothing
                        // to shift; live admits events already past their
                        // onset, so this is reachable.
                        let boundary = (e.onset_frame / 128 + 1) * 128;
                        if start >= boundary {
                            0
                        } else {
                            (e.onset_frame % 128) as u8
                        }
                    },
                    cut_group: e.cut.or_else(|| e.sample.and_then(|sample| sample.cut)).map(f32::to_bits),
                    id: self.next_voice_id,
                    cut_fade_frame: None,
                    polyphony_fade_frame: None,
                    compressor: if let Some(c) = e.controls.compressor {
                        self.compressor_pool.pop().map(|mut state| {
                            *state =
                                CompressorState::new(self.sample_rate as f32, &c, e.onset_frame);
                            state
                        })
                    } else {
                        None
                    },
                    limiter: if let Some(settings) = e.controls.limit {
                        // In place: the struct is twenty-five kilobytes
                        // of ring, and assigning a fresh one over it would
                        // build that on the callback's stack and copy it,
                        // once per note.
                        self.limiter_pool.pop().map(|mut limiter| {
                            limiter.reconfigure(
                                self.sample_rate,
                                settings.threshold_db,
                                settings.character,
                            );
                            limiter
                        })
                    } else {
                        None
                    },
                    pitch_gate: match voice_controls.pitch_env {
                        Some(pe) => Self::gate_value(
                            duration_secs,
                            crate::backend::Envelope {
                                attack_secs: pe.adsr.attack_secs,
                                decay_secs: pe.adsr.decay_secs,
                                sustain: pe.adsr.sustain as f32,
                                release_secs: pe.adsr.release_secs,
                            },
                        ),
                        None => 0.0,
                    },
                    vowel_filters: [[NotchState::default(); 5]; 2],
                    vowel_coefs: match voice_controls.vowel {
                        Some(vw) => std::array::from_fn(|k| {
                            bandpass_coefs(vw.freqs[k], vw.qs[k], self.sample_rate as f32)
                        }),
                        None => [NotchCoefs::default(); 5],
                    },
                    coarse_hold: [0.0; 2],
                    fx_mod_hold: FxParamHold::default(),
                    source_gone: false,
                };
                // A takeover cut's horizon: a pending onset of the OUTGOING
                // generation that activates here still pre-fades - the cut
                // caught voices already sounding, and this catches one the
                // ghost window is still owed. New-generation onsets fail the
                // generation guard and start untouched.
                if let Some((cut_frame, outgoing)) = self.armed_takeover_cut
                    && voice.generation == outgoing
                    && !voice.piano
                {
                    let stop_at = (cut_frame.saturating_sub(voice.start_frame)) as f32
                        / self.sample_rate as f32
                        + CUT_FADE_SECS;
                    voice.stop_secs = voice.stop_secs.min(stop_at);
                    voice.cut_fade_frame = Some(cut_frame);
                }
                // Change ownership at the new preview's actual onset, after
                // its source has resolved. Notes within one preview remain
                // polyphonic, and untagged score voices keep their tails.
                if voice.preview_epoch > self.preview_epoch {
                    self.preview_epoch = voice.preview_epoch;
                    for old in &mut self.voices {
                        if old.preview_epoch != 0 && old.preview_epoch < voice.preview_epoch {
                            let frame = old.cut_fade_frame
                                .unwrap_or(e.onset_frame).min(e.onset_frame);
                            old.cut_fade_frame = Some(frame);
                            let stop_at = frame.saturating_sub(old.start_frame) as f32
                                / sample_rate as f32 + CUT_FADE_SECS;
                            old.stop_secs = old.stop_secs.min(stop_at);
                        }
                    }
                } else if voice.preview_epoch != 0 && voice.preview_epoch < self.preview_epoch {
                    // A late outgoing event must not revive a retired preview.
                    voice.cut_fade_frame = Some(e.onset_frame);
                    voice.stop_secs = voice.stop_secs.min(CUT_FADE_SECS);
                }
                // Counted where the voice exists: a missing sample or a
                // muted event applies its side effects but sounds nothing,
                // so it must not spend the active-voice ceiling.
                activated += 1;
                pressure.activate(&voice);
                self.voices.push(voice);
                *missing_reverb_events += missed_stage_reverb;
                #[cfg(feature = "device-audio")]
                if confirmation_sample_matches && let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                    consumer.activation(tag, if *missing_reverb_events > missing_reverbs_before {
                        crate::confirmation::ActivationOutcome::DryFallback
                    } else {
                        crate::confirmation::ActivationOutcome::Activated
                    });
                }
                false
            } else {
                true
            }
        });

        for slot in &mut self.pending_chokes {
            let Some((at, group)) = *slot else {
                continue;
            };
            if at >= end {
                continue;
            }
            if let Some(voice) = self
                .voices
                .iter_mut()
                .filter(|voice| voice.cut_group == Some(group) && voice.start_frame <= at)
                .max_by_key(|voice| voice.id)
            {
                let fade = voice
                    .cut_fade_frame
                    .map_or(at.max(start), |running| running.min(at.max(start)));
                voice.cut_fade_frame = Some(fade);
                let end =
                    fade.saturating_sub(voice.start_frame) as f32 / sample_rate as f32 + 0.011;
                voice.stop_secs = voice.stop_secs.min(end);
            }
            *slot = None;
        }

        // A bus receiver hears the CURRENT quantum's mix, not the previous
        // one (graph topological order). Reproduce that by evaluating every
        // pure sender before any receiver: a voice writes its send after
        // computing its own sample, so once the receivers are last in the
        // vec they read a complete `bus_now`. The sort is stable and voices
        // are found by `id`, never by index.
        //
        // A voice that both sends and receives sorts with the receivers and
        // so sees whatever landed before it - that is a graph cycle, which
        // has no same-quantum answer anyway.
        if self.voices.iter().any(Voice::reads_a_bus) {
            // Stable partition by rotation rather than `sort_by_key`, which
            // allocates a merge buffer above ~20 elements - not allowed on the
            // audio callback. Already-ordered voices cost one no-op rotate
            // each, which is the steady state.
            let mut senders = 0;
            for at in 0..self.voices.len() {
                if !self.voices[at].reads_a_bus() {
                    self.voices[senders..=at].rotate_right(1);
                    senders += 1;
                }
            }
        }

        // Orbit modulation is uncommon, but its sums used to be cleared for
        // every frame and updated with three zeroes for every ordinary voice.
        // The arrays start at zero for each block, so a block with no voice
        // capable of producing ModAdds can leave them untouched.
        let any_param_modulators = self.voices.iter().any(|voice| voice.has_param_modulators);
        // The chains with an output: the order has a stage, or voices still
        // feed the chain. An empty order passes these voices dry.
        let effect_buses = effect_order
            .iter()
            .zip(insert_postgains.iter())
            .enumerate()
            .fold(0u64, |mask, (bus, (order, level))| {
                let output =
                    order[0] != crate::insert::NO_STAGE || level.unfed < INSERT_QUIET_FRAMES;
                mask | (u64::from(output) << bus)
            });

        let mut orbit_inputs = [[0.0f32; 2]; MAX_ORBITS];
        let zero_mod_adds = ModAdds::default();
        // `djf` belongs to the ORBIT but is modulated from a VOICE, so the
        // voices' contributions are summed here and handed to the orbit's
        // filter. DJFProcessor reads `parameters.value[0]`, so the sum is
        // latched once per 128-frame quantum.
        let mut djf_mods = [0.0f32; MAX_ORBITS];
        // DelayNode.delayTime is a-rate rather than latched, so its sum is
        // rebuilt every frame instead of every quantum.
        let mut delay_time_mods = [0.0f32; MAX_ORBITS];
        // The GainNode on the delay's feedback edge is shared in the same
        // way, and its `gain` AudioParam is a-rate too.
        let mut delay_feedback_mods = [0.0f32; MAX_ORBITS];
        // Tight-path classification, after every activation of this render:
        // a steal or a polyphony fade lands its ladders on a voice that
        // was plain until then, and the classification has to see them.
        // A field read, not a method: this late, `self` is already split
        // across the block pipeline's borrows.
        #[cfg(test)]
        let plain_disabled = self.disable_plain_path;
        #[cfg(not(test))]
        let plain_disabled = false;
        for voice in &mut self.voices {
            voice.plain = !plain_disabled && plain_sample_voice(voice);
        }
        // Per-orbit block pipeline: voices, delay and reverb returns
        // accumulate into ≤128-frame per-orbit buffers (the reverb is a
        // block convolution on WebAudio's 128-frame quantum), then the duck
        // gain scales each orbit's whole signal into the output - exactly
        // where the per-orbit output gain sits.
        let blocks = &mut self.orbit_blocks;
        let visual_capture_mask = self.ui_visual_capture_mask;
        let visual_capture_generation_floor = self.ui_visual_capture_generation_floor;
        let visual_blocks = &mut self.ui_visual_mix;
        let bus_now = &mut self.bus_now;
        let input = self.input.clone();
        let sample_rate = self.sample_rate;
        let sample_resampling_mode = self.sample_resampling_mode;
        let djfs = &mut self.orbit_djf;
        let sends = &mut self.reverb_sends;
        let mut prepared_block = [[0.0f32; crate::reverb::REVERB_BLOCK]; 2];
        let mut sub_start = 0usize;
        while sub_start < frames {
            let sub_len = (frames - sub_start).min(crate::reverb::REVERB_BLOCK);
            // The keyboard shares the output and master processing, but is
            // not a score orbit. Score faders, ducking, DJF and routing must
            // neither silence it nor redirect it to another output pair.
            let mut piano_mix = [[0.0f32; crate::reverb::REVERB_BLOCK]; 2];
            for buffer in blocks.iter_mut() {
                buffer[0][..sub_len].fill(0.0);
                buffer[1][..sub_len].fill(0.0);
            }
            for buffer in sends.iter_mut() {
                buffer[0][..sub_len].fill(0.0);
                buffer[1][..sub_len].fill(0.0);
            }
            let mut buses = effect_buses;
            while buses != 0 {
                let buffer = &mut insert_blocks[buses.trailing_zeros() as usize];
                buffer[0][..sub_len].fill(0.0);
                buffer[1][..sub_len].fill(0.0);
                buses &= buses - 1;
            }
            // An insert that renders ahead counts its own frames from the
            // block it is collected in. One that waits prepared would count
            // its first block at the onset that swaps it in, so a note the
            // admission delivers early would land that whole wait late.
            // Collect the wait, and drop what it renders: the frames before
            // its first note are silence.
            for insert in prepared_inserts.iter_mut().flatten() {
                if !insert.renders_ahead() {
                    continue;
                }
                if offline {
                    insert.flush(sub_len);
                }
                let (left, right) = prepared_block.split_at_mut(1);
                insert.collect(&mut left[0][..sub_len], &mut right[0][..sub_len]);
            }
            // Arm every duck whose 10 ms hold point this sub-block reaches,
            // in arm order and, within one arm frame, in admission order:
            // the last trigger armed owns the orbit's dip.
            let arm_through = start + (sub_start + sub_len) as u64;
            loop {
                let mut next: Option<usize> = None;
                for (index, pending) in pending_ducks.iter().enumerate() {
                    if pending.arm_frame < arm_through
                        && next.is_none_or(|best| pending.arm_frame < pending_ducks[best].arm_frame)
                    {
                        next = Some(index);
                    }
                }
                let Some(index) = next else { break };
                // An order-preserving remove keeps the queue in admission
                // order; it shifts within capacity and never allocates.
                let pending = pending_ducks.remove(index);
                for target in pending.controls.targets.iter().flatten() {
                    let orbit = usize::from(target.orbit);
                    if orbit < MAX_ORBITS {
                        ducks[orbit].trigger(
                            pending.onset_frame,
                            start + sub_start as u64,
                            sample_rate,
                            *target,
                        );
                    }
                }
            }
            let insert_idle_limit = sample_rate.saturating_mul(INSERT_IDLE_SECONDS);
            let effect_slots = crate::insert::EFFECT_CHAIN * crate::insert::MAX_INSERT_BUSES;
            let (effects, instruments) = inserts.split_at_mut(effect_slots);
            let (leaving_effects, leaving_instruments) = leaving_inserts.split_at_mut(effect_slots);
            let (effect_mixes, instrument_mixes) = insert_mixes.split_at_mut(effect_slots);
            // The jobs of this sub-block, while the plugin workers run. The
            // blocks of an insert that runs on any thread go to a job, and
            // the box leaves its slot until the join.
            let mut jobs = plugin_pool
                .as_mut()
                .filter(|pool| pool.runs(sub_len))
                .map(PoolHandle::begin);
            let mut pooled_instruments = 0u64;
            let mut pooled_effects = 0u64;
            // Instruments can render while native voices produce their input.
            // Their output is collected after the native sends are ready.
            let mut submitted = 0u64;
            for (bus, slot) in instruments.iter_mut().enumerate() {
                let Some(instrument) = slot else { continue };
                let idle = insert_idle_frames
                    [crate::insert::EFFECT_CHAIN * crate::insert::MAX_INSERT_BUSES + bus];
                if idle >= insert_idle_limit && !instrument.busy() {
                    continue;
                }
                submitted |= 1u64 << bus;
                // The instrument that leaves the slot goes with the job while
                // it fades.
                let leaving = &mut leaving_instruments[bus];
                let swapping = instrument_mixes[bus].leaving.is_moving();
                let any_thread = instrument.any_thread()
                    && leaving
                        .as_ref()
                        .filter(|_| swapping)
                        .is_none_or(|leaving| leaving.any_thread());
                if let Some(jobs) = jobs.as_mut().filter(|_| any_thread) {
                    let job = jobs.job(JobKind::Instrument, bus);
                    job.frames = sub_len;
                    job.boxes[0] = slot.take();
                    job.leaving[0] = leaving.take_if(|_| swapping);
                    jobs.post(JobKind::Instrument, bus);
                    pooled_instruments |= 1u64 << bus;
                    continue;
                }
                let (left, right) = instrument_blocks[bus].split_at_mut(1);
                let (left, right) = (&mut left[0][..sub_len], &mut right[0][..sub_len]);
                left.fill(0.0);
                right.fill(0.0);
                instrument.submit(left, right);
            }
            for i in 0..sub_len {
                let abs = start + (sub_start + i) as u64;
                orbit_inputs.fill([0.0; 2]);
                if any_param_modulators {
                    if abs & 127 == 0 {
                        djf_mods = [0.0; MAX_ORBITS];
                    }
                    delay_time_mods.fill(0.0);
                    delay_feedback_mods.fill(0.0);
                }
                for v in &mut self.voices {
                    if abs < v.graph_start_frame {
                        continue;
                    }
                    let sbd_pre_onset =
                        abs < v.start_frame && matches!(v.source, VoiceSource::Sbd { .. });
                    // A plugin chain gets no value before the onset. That
                    // value is no sound, and with it the chain is not
                    // silent at the first frame of the note.
                    if abs < v.start_frame && (!sbd_pre_onset || v.through_insert) {
                        continue;
                    }
                    if !sbd_pre_onset && v.fm_state.is_some() && abs & 127 == 0 {
                        let state = v.fm_state.as_deref_mut().expect("FM state must be present");
                        if state.quantum_skip > 0 {
                            let fm = v.controls.fm.expect("FM state requires FM controls");
                            fm_advance(
                                fm,
                                v.freq_hz,
                                &mut state.phases,
                                &mut state.noise,
                                u32::from(state.quantum_skip),
                                tables,
                                sample_rate,
                            );
                            state.quantum_skip = 0;
                        }
                    }
                    if !sbd_pre_onset && v.pitch_quantum_skip > 0 && abs & 127 == 0 {
                        v.pitch_quantum_skip = 0;
                    }
                    // The oscillator, noise source, and their automation do
                    // not advance before `start_frame`. Only the connected
                    // WaveShaper processes its zero input. Downstream nodes do
                    // process that value, so use onset-time controls while the
                    // source clock remains parked.
                    let t = if sbd_pre_onset {
                        0.0
                    } else {
                        ((abs - v.start_frame) as f32 + v.onset_lead) / sr as f32
                    };
                    if t >= v.stop_secs {
                        continue;
                    }
                    // The plain voice's whole frame, out of line: the
                    // general body below is correct for it too, at the
                    // price of its register spills.
                    if v.plain {
                        render_plain_stereo_sample(
                            v,
                            bank,
                            sample_resampling_mode,
                            i,
                            t,
                            blocks,
                            &mut piano_mix,
                        );
                        continue;
                    }
                    let env = Self::envelope(t, v.duration_secs, v.controls.envelope, v.gate_value);
                    // lfo()/env() modulators: each rides its param
                    // additively. LFO phases free-run from the
                    // onset-aligned phase0.
                    // Modulators only run on 128-frame context quanta
                    // starting strictly after the onset (the begin gate), so
                    // each note opens unmodulated for up to one quantum, and
                    // the envelope clock reads quantum starts.
                    let quantum_start = abs & !127;
                    // Compare against the fractional onset, not its rounded
                    // frame. An onset at frame 127.6 rounds to 128 but must
                    // still open the transient gate at quantum 128.
                    let transient_open =
                        (quantum_start as f64) > v.start_frame as f64 - f64::from(v.onset_lead);
                    // Filter LFOs stop contributing at the hap's hold end.
                    // Other modulators can run until release ends. Neither end
                    // is based on the sample slice's potentially longer duration.
                    let filter_lfo_alive =
                        lfo_quantum_alive(quantum_start, sr, v.controls.filter_lfo_end_secs);
                    // LFOs and the pulse, wavetable and supersaw oscillators
                    // share the f32-rounded begin gate. The transient shaper
                    // uses the fractional-frame comparison above instead.
                    let source_open =
                        lfo_quantum_open(quantum_start, sr, v.controls.worklet_begin_secs);
                    let lfo_live = source_open
                        && lfo_quantum_alive(quantum_start, sr, v.controls.lfo_end_secs);
                    let mut mod_adds = if v.has_param_modulators && lfo_live {
                        Some(ModAdds::default())
                    } else {
                        None
                    };
                    let note_alive = note_end_alive(
                        quantum_start,
                        v.start_frame,
                        v.onset_lead,
                        v.hap_duration_secs,
                        v.controls.modulator_release_secs,
                        sr,
                    );
                    // Parameter and bus modulators stop at hold plus release;
                    // their target parameters then keep their base values.
                    if let Some(mod_adds) = mod_adds.as_mut() {
                        // The f32-rounded gate can open in the quantum that
                        // contains the onset, before `start_frame`. Keep the
                        // subtraction signed and clamp envelope time to zero.
                        let t_env = (((quantum_start as f64 - v.start_frame as f64) as f32
                            + v.onset_lead)
                            / sr as f32)
                            .max(0.0);
                        // Two passes. A modulator aimed at a FILTER's own LFO
                        // has to be summed before that LFO is evaluated -
                        // WebAudio gets the ordering for free by processing a
                        // node that feeds an AudioParam ahead of the node
                        // owning it, and here it has to be arranged. Each
                        // modulator appears in exactly one pass, so phases
                        // still advance once per sample.
                        // A bus modulator's signal is another pattern's audio.
                        // `connectBusModulator` sums the bus into a
                        // `ConstantSourceNode` offset, scales by depth/0.3,
                        // clamps to the param's range, and adds to the param.
                        // A stereo node feeding an AudioParam is down-mixed,
                        // so the two channels average.
                        for slot in v.controls.bus_mods.iter().flatten() {
                            let bus = bus_now[usize::from(slot.bus)];
                            let signal = (bus[0] + bus[1]) * 0.5 + slot.dc;
                            let value = (signal * slot.depth).clamp(slot.min, slot.max);
                            mod_adds.bucket_at(slot.fxi, slot.target, value, slot.param_base);
                        }
                        for first_pass in [true, false] {
                            for (slot, phase) in
                                v.controls.lfos.iter().zip(v.mod_lfo_phases.iter_mut())
                            {
                                let Some(lfo) = slot else { continue };
                                if ModAdds::targets_another_modulator(lfo.target) != first_pass {
                                    continue;
                                }
                                // Dead past the hold end; see `filter_lfo_alive`.
                                if lfo.filter.is_some() && !filter_lfo_alive {
                                    continue;
                                }
                                // Every one of these is block-rate, so
                                // modulation of them is latched per
                                // quantum.
                                let mut depth = lfo.depth;
                                let mut dcoffset = lfo.dcoffset;
                                let mut skew = lfo.skew;
                                let mut curve = lfo.curve;
                                let mut rate = lfo.frequency_hz;
                                if let Some(kind) = lfo.filter {
                                    let hold = v.mod_param_hold.filter[kind.index()];
                                    depth += hold.depth;
                                    dcoffset += hold.dc;
                                    skew += hold.skew;
                                }
                                if let Some(hold) = lfo
                                    .id
                                    .and_then(|id| v.mod_param_hold.lfo.get(usize::from(id)))
                                {
                                    depth += hold.depth;
                                    dcoffset += hold.dcoffset;
                                    skew += hold.skew;
                                    curve += hold.curve;
                                    rate += hold.rate;
                                }
                                let wave = lfo_waveshape_f64(lfo.shape, *phase, skew);
                                let raw = (wave as f32 + dcoffset) * depth;
                                let shaped = raw.powf(curve);
                                // The reference computes the sample in f64.
                                // A small negative value rounds to zero in
                                // f32 and hides the NaN.
                                let nan = shaped.is_nan()
                                    || (raw == 0.0 && {
                                        let exact = (wave + f64::from(dcoffset)) * f64::from(depth);
                                        exact < 0.0 && exact.powf(f64::from(curve)).is_nan()
                                    });
                                // A NaN sample makes the summed param NaN.
                                // The param then takes its default value.
                                let value = if nan {
                                    nan_param_default(lfo.target, &v.source) - lfo.param_base
                                } else {
                                    js_clamp(shaped, lfo.min, lfo.max)
                                };
                                mod_adds.bucket_at(lfo.fxi, lfo.target, value, lfo.param_base);
                                *phase = wrap_phase(*phase + f64::from(rate) / sr);
                            }
                            for env_mod in v.controls.envs.iter().flatten() {
                                if ModAdds::targets_another_modulator(env_mod.target) != first_pass
                                {
                                    continue;
                                }
                                // begin==0 never satisfies the envelope's
                                // begin-changed trigger check, so a note
                                // starting at context time zero stays idle.
                                // The browser's envelope processor writes
                                // nothing for a block whose start is at or
                                // before the trigger, and triggers on the
                                // first block after it.
                                let value = if v.start_frame == 0 || t_env <= 0.0 {
                                    0.0
                                } else {
                                    let hold = env_mod
                                        .id
                                        .and_then(|id| v.mod_param_hold.env.get(usize::from(id)))
                                        .copied()
                                        .unwrap_or_default();
                                    // The envelope clamps sustain to
                                    // 0..1 and keeps every time non-negative,
                                    // so a modulator cannot drive them out of
                                    // range however deep it swings.
                                    let shaped = crate::backend::EnvMod {
                                        attack_secs: (env_mod.attack_secs + hold.attack).max(0.0),
                                        decay_secs: (env_mod.decay_secs + hold.decay).max(0.0),
                                        sustain: (env_mod.sustain + hold.sustain).clamp(0.0, 1.0),
                                        release_secs: (env_mod.release_secs + hold.release)
                                            .max(0.0),
                                        ..*env_mod
                                    };
                                    env_mod_value(&shaped, t_env) * (env_mod.depth + hold.depth)
                                };
                                mod_adds.bucket_at(
                                    env_mod.fxi,
                                    env_mod.target,
                                    js_clamp(value, env_mod.min, env_mod.max),
                                    env_mod.param_base,
                                );
                            }
                            if first_pass && abs & 127 == 0 {
                                v.mod_param_hold = mod_adds.mod_params;
                            }
                        }
                    }
                    // Latch the block-rate params once per render quantum,
                    // then hold for the block.
                    if abs & 127 == 0 {
                        v.fx_mod_hold = mod_adds
                            .as_ref()
                            .map_or_else(FxParamHold::default, ModAdds::fx_hold);
                    }
                    let mod_adds = mod_adds.as_ref().unwrap_or(&zero_mod_adds);
                    // A slider at zero carries the binding but no route, so
                    // the ramp only runs while the matrix exists.
                    let live_fm_index = (v.live_fm_index.binding != 0 && v.controls.fm.is_some())
                        .then(|| v.live_fm_index.next());
                    let fm_matrix_hz = match &v.controls.fm {
                        None => 0.0f32,
                        Some(fm) => {
                            let state =
                                v.fm_state.as_deref_mut().expect("FM state must be present");
                            Self::render_fm_offset(
                                fm,
                                v.freq_hz,
                                t,
                                v.duration_secs,
                                state.quantum_skip,
                                &mut state.phases,
                                &mut state.noise,
                                live_fm_index,
                                mod_adds,
                                tables,
                                sr,
                            )
                        }
                    };
                    let fm_offset_hz = fm_matrix_hz + mod_adds.frequency_hz;
                    if v.live_cutoff.binding != 0 {
                        let cutoff = v.live_cutoff.next();
                        v.filters.set_lowpass_frequency(cutoff);
                        if let Some(right) = &mut v.filters_right {
                            right.set_lowpass_frequency(cutoff);
                        }
                    }
                    if v.live_resonance.binding != 0 {
                        let q = v.live_resonance.next();
                        v.filters.set_lowpass_q(q);
                        if let Some(right) = &mut v.filters_right {
                            right.set_lowpass_q(q);
                        }
                    }
                    let filter_adds = [
                        mod_adds.lowpass_hz,
                        mod_adds.highpass_hz,
                        mod_adds.bandpass_hz,
                    ];
                    let filter_q_adds = [mod_adds.lowpass_q, mod_adds.highpass_q, mod_adds.band_q];
                    let live_gain = if v.live_gain.binding != 0 {
                        v.live_gain.next()
                    } else {
                        1.0
                    };
                    let mut env = env * mod_adds.gain_factor();
                    if let Some(fade) = v.cut_fade_frame
                        && abs >= fade
                    {
                        // setValueAtTime(1, t) + linearRampToValueAtTime(0, t+0.01)
                        env *= (1.0 - (abs - fade) as f32 / (0.01 * sr as f32)).max(0.0);
                    }
                    if let Some(fade) = v.polyphony_fade_frame
                        && abs >= fade
                    {
                        // A linear ramp to 0 over 0.25 s on the voice's own
                        // gain, paired with the stop at t + 0.25.
                        env *= (1.0 - (abs - fade) as f32 / (POLYPHONY_FADE_SECS * sr as f32))
                            .max(0.0);
                    }
                    // The `post` gain sits AFTER the whole chain and is
                    // tapped for every send. Folding it into the source
                    // instead would move the level a non-linear stage sees:
                    // `.distort(2).postgain(.5)` would drive the waveshaper
                    // at half the amplitude and come out 1.7 dB loud, with
                    // a different shape, rather than simply half as loud.
                    let post_gain = v.controls.postgain * mod_adds.postgain_factor();
                    let post_insert = v.through_insert && !v.piano;
                    if post_insert && !sbd_pre_onset {
                        let gain = &mut insert_postgains[v.insert_bus.expect("insert voice bus")];
                        let owner = (v.start_frame, v.id);
                        if owner >= gain.owner {
                            // A new owner while the chain still sounds: a
                            // step is a click on the notes before. A note
                            // after silence takes its level at once.
                            if owner > gain.owner && gain.owned {
                                gain.offset = crate::live_control::Ramp::new(0, 0.0);
                                let sounds = gain.quiet.min(gain.unfed) < INSERT_QUIET_FRAMES;
                                if sounds {
                                    let before =
                                        crate::live_control::Ramp::new(0, gain.applied - post_gain);
                                    gain.offset.declick_from(before, sample_rate);
                                }
                            }
                            gain.owner = owner;
                            gain.value = post_gain;
                            gain.owned = true;
                        }
                    }
                    // Pan modulators ride the StereoPanner's pan param
                    // (base 2·pan−1, clamped to the param's [-1, 1] range).
                    let pan_param = if mod_adds.pan != 0.0 {
                        (v.pan_x + mod_adds.pan).clamp(-1.0, 1.0)
                    } else {
                        v.pan_x
                    };
                    // vib/penv ride the source's DETUNE param (cents), which
                    // multiplies the computed frequency by 2^(cents/1200) after
                    // the additive frequency-param mods. sbd runs its own pitch
                    // envelope, so the general detune path skips it.
                    let pitch_ratio = {
                        let mut cents = 0.0f32;
                        if let Some(vb) = v.controls.vibrato {
                            // Phase INTEGRATES the rate. sin(2*pi*f*t) agrees
                            // only while f is constant; once a modulator moves
                            // it, treating the rate as a position rather than
                            // a speed bends the whole waveform.
                            let rate = vb.freq_hz + mod_adds.vib_rate;
                            let cents_depth = vb.cents + mod_adds.vib_depth;
                            cents += (std::f64::consts::TAU * v.vibrato_phase).sin() as f32
                                * cents_depth;
                            v.vibrato_phase = wrap_phase(v.vibrato_phase + f64::from(rate) / sr);
                        }
                        // In a source's first render quantum the browser reads
                        // its per-frame detune automation from the quantum
                        // start while writing output from the onset, so the
                        // envelope arrives `offset` frames late there and
                        // frames before the automation event read zero.
                        let t_pitch = t - f32::from(v.pitch_quantum_skip) / sr as f32;
                        if let Some(pe) = v.controls.pitch_env.filter(|_| t_pitch >= 0.0) {
                            cents += if pe.exponential {
                                crate::biquad::FilterStage::frequency_at(
                                    pe.adsr,
                                    t_pitch,
                                    v.duration_secs,
                                )
                            } else {
                                let shape = Self::envelope(
                                    t_pitch,
                                    v.duration_secs,
                                    crate::backend::Envelope {
                                        attack_secs: pe.adsr.attack_secs,
                                        decay_secs: pe.adsr.decay_secs,
                                        sustain: pe.adsr.sustain as f32,
                                        release_secs: pe.adsr.release_secs,
                                    },
                                    v.pitch_gate,
                                );
                                (pe.adsr.min_hz
                                    + f64::from(shape) * (pe.adsr.max_hz - pe.adsr.min_hz))
                                    as f32
                            };
                        }
                        if cents == 0.0 || matches!(v.source, VoiceSource::Sbd { .. }) {
                            1.0
                        } else {
                            (cents / 1200.0).exp2()
                        }
                    };
                    // What a SAMPLE plays at this block. `detune` on a buffer
                    // source is k-rate: one value per 128-frame quantum, read
                    // from where the automation ENDS in it, held for the whole
                    // block. Sampling it per frame glides where the browser
                    // steps, which leaves the level right and the waveform
                    // wrong. An oscillator's detune is a-rate and uses
                    // `pitch_ratio` above unchanged.
                    let sample_pitch = if matches!(v.source, VoiceSource::Sample { .. }) {
                        if abs & 127 == 0 || v.sample_pitch_hold.is_none() {
                            // A k-rate param is sampled at the FIRST frame of
                            // the block and held for all of it. Measured both
                            // ways against Chromium: reading the block's end
                            // instead scores 0.18 where this scores 1.000000.
                            let ahead_frames = -((abs & 127) as i32);
                            let block_start = t + ahead_frames as f32 / sr as f32;
                            // A block that began BEFORE this voice did reads
                            // the param where no automation has run yet, which
                            // for detune is zero. The browser plays that whole
                            // block at the undetuned rate, however late in it
                            // the source starts.
                            let mut cents = 0.0f32;
                            if let Some(pe) = v.controls.pitch_env.filter(|_| block_start >= 0.0) {
                                let shifted = block_start;
                                if shifted >= 0.0 {
                                    cents += if pe.exponential {
                                        crate::biquad::FilterStage::frequency_at(
                                            pe.adsr,
                                            shifted,
                                            v.duration_secs,
                                        )
                                    } else {
                                        let shape = Self::envelope(
                                            shifted,
                                            v.duration_secs,
                                            crate::backend::Envelope {
                                                attack_secs: pe.adsr.attack_secs,
                                                decay_secs: pe.adsr.decay_secs,
                                                sustain: pe.adsr.sustain as f32,
                                                release_secs: pe.adsr.release_secs,
                                            },
                                            v.pitch_gate,
                                        );
                                        (pe.adsr.min_hz
                                            + f64::from(shape) * (pe.adsr.max_hz - pe.adsr.min_hz))
                                            as f32
                                    };
                                }
                            }
                            // A modulator aimed at the source's pitch lands
                            // on `detune` here, in cents, because a buffer
                            // source has no frequency param of its own. It is
                            // the same k-rate param the envelope and the
                            // vibrato ride, so it is read at the block start
                            // with them.
                            cents += mod_adds.frequency_hz;
                            if let Some(vb) = v.controls.vibrato.filter(|_| block_start >= 0.0) {
                                // The vibrato oscillator runs on its own clock,
                                // so its value at the block end is read from the
                                // phase this block will reach.
                                let ahead = f64::from(ahead_frames) * f64::from(vb.freq_hz) / sr;
                                cents += (std::f64::consts::TAU * (v.vibrato_phase + ahead)).sin()
                                    as f32
                                    * vb.cents;
                            }
                            v.sample_pitch_hold = Some(if cents == 0.0 {
                                1.0
                            } else {
                                (cents / 1200.0).exp2()
                            });
                        }
                        v.sample_pitch_hold.unwrap_or(1.0)
                    } else {
                        1.0
                    };
                    // Tremolo: gain = max(1−depth, 0) + the LFO's
                    // clamp(pow(shape·depth, 1.5), 0, 1); the LFO obeys the
                    // same onset-quantum gate, so a full-depth tremolo opens
                    // silent for up to one quantum by design.
                    let trem_gain = match v.controls.tremolo {
                        None => 1.0f32,
                        Some(tr) => {
                            // The chain is a gain node the LFO ADDS to:
                            // gain = max(1-depth, 0), lfo -> gain.gain.
                            // `tremolodepth` names that
                            // node's gain, so a modulator moves the floor and
                            // leaves the LFO's own depth where it was.
                            let base = tr.gain_floor() + mod_adds.trem_depth;
                            if lfo_live {
                                let hold = &v.fx_mod_hold;
                                let skew = tr.skew + hold.trem_skew;
                                let shape = if hold.trem_shape == 0.0 {
                                    tr.shape
                                } else {
                                    ((f32::from(tr.shape) + hold.trem_shape) as i32).rem_euclid(5)
                                        as u8
                                };
                                // The seed reads the frequency param, so a
                                // modulated rate shifts where the LFO starts.
                                let freq = tr.frequency_hz + hold.trem_rate;
                                let phase = *v.tremolo_phase.get_or_insert_with(|| {
                                    lfo_phase0(tr.time_secs, freq, tr.phase_offset)
                                });
                                let raw = lfo_waveshape(shape, phase, skew) * tr.depth;
                                let shaped = raw.powf(1.5).clamp(0.0, 1.0);
                                let next = wrap_phase(phase + f64::from(freq) / sr);
                                v.tremolo_phase = Some(next);
                                // A NaN sum gives the gain its default.
                                let gain = base + shaped;
                                if gain.is_nan() { 1.0 } else { gain }
                            } else {
                                base
                            }
                        }
                    };
                    // Phaser: notch at (center+282)·2^(detune/1200), detune from
                    // a default tri LFO clamped to ±sweep cents. Before the
                    // gate opens and after it closes the LFO is silent, so the
                    // notch sits at its static center.
                    let phaser_coefs = match v.controls.phaser {
                        None => None,
                        Some(ph) => {
                            // The phaser's LFO is built with depth =
                            // sweep*2, and phasersweep names that depth
                            // param, so the modulator adds to the doubled
                            // value rather than to the sweep it was derived
                            // from.
                            let lfo_depth =
                                (2.0 * ph.sweep_cents + v.fx_mod_hold.phaser_sweep).max(0.0);
                            let sweep = lfo_depth * 0.5;
                            let rate = ph.rate_hz + v.fx_mod_hold.phaser_rate;
                            let detune = if lfo_live {
                                // getPhaser passes no phaseoffset, and its
                                // `time` is the note's begin.
                                let phase = *v
                                    .phaser_phase
                                    .get_or_insert_with(|| lfo_phase0(ph.time_secs, rate, 0.0));
                                let raw = (lfo_waveshape(0, phase, 0.5) - 0.5) * lfo_depth;
                                let next = wrap_phase(phase + f64::from(rate) / sr);
                                v.phaser_phase = Some(next);
                                js_clamp(raw, -sweep, sweep)
                            } else {
                                0.0
                            };
                            let center = ph.center_hz + mod_adds.phaser_center;
                            let frequency = (center + 282.0) * (detune / 1200.0).exp2();
                            // phaserdepth names the notch's Q, not the depth
                            // control: the modulator lands after the
                            // depth-to-Q conversion, not before it.
                            let q = 2.0 - (ph.depth * 2.0).clamp(0.0, 1.9) + mod_adds.phaser_depth;
                            Some(notch_coefs(frequency, q, sr as f32))
                        }
                    };
                    // All five vowel filters are one `vowel` target, so the
                    // SAME a-rate signal lands on every frequency param.
                    // Keep the precomputed static coefficients when there
                    // is no signal to add.
                    let vowel_coefs = match (v.controls.vowel, mod_adds.vowel_hz) {
                        (Some(vw), add) if add != 0.0 => std::array::from_fn(|k| {
                            bandpass_coefs(vw.freqs[k] + add, vw.qs[k], sr as f32)
                        }),
                        _ => v.vowel_coefs,
                    };
                    let (wave_l, wave_r) = match &mut v.source {
                        VoiceSource::Oscillator {
                            table_selection,
                            phase,
                            partial_limit,
                        } => {
                            let wave = oscillator_sample(
                                tables,
                                &v.controls,
                                *table_selection,
                                phase,
                                partial_limit,
                                v.freq_hz,
                                fm_offset_hz,
                                pitch_ratio,
                                sr,
                            );
                            // `noise` crossfades PINK noise with the
                            // oscillator before the envelope. `wetfade`
                            // holds at one below a half and falls linearly
                            // above it, and the dry leg reads the amount
                            // while the wet leg reads its complement - so a
                            // half leaves both legs open.
                            let wave = if v.controls.noise > 0.0 {
                                let wetfade =
                                    |d: f32| if d < 0.5 { 1.0 } else { 1.0 - (d - 0.5) / 0.5 };
                                let pink = v.source_noise.next(1, 0.0);
                                wave * wetfade(v.controls.noise)
                                    + pink * wetfade(1.0 - v.controls.noise)
                            } else {
                                wave
                            };
                            (wave, wave)
                        }
                        // The supersaw has the same begin gate as the pulse
                        // oscillator: no output and no phase advance until
                        // the gate opens.
                        VoiceSource::Supersaw { .. } if !source_open => (0.0, 0.0),
                        VoiceSource::Supersaw { plan, phases } => {
                            // `detune` and `spread` are params the supersaw
                            // shares with the wavetable, so a modulator on
                            // either moves both.
                            let (mut gain_l, mut gain_r) = plan.pan_gains(mod_adds.panspread);
                            let freq = (v.freq_hz + fm_offset_hz) * pitch_ratio;
                            let static_spread = mod_adds.freqspread.to_bits() == 0;
                            if static_spread
                                && let Some(kernel) = supersaw_kernel
                                && kernel.supports(plan.count)
                            {
                                // SAFETY: the kernel was selected only after
                                // its CPU prerequisites were detected, and
                                // both arrays have MAX_UNISON entries while
                                // count is clamped to that bound.
                                unsafe {
                                    kernel.render(
                                        phases,
                                        &plan.detune_ratios,
                                        plan.count,
                                        freq,
                                        sr as f32,
                                        (gain_l, gain_r),
                                    )
                                }
                            } else {
                                let (scale, center) = if static_spread {
                                    (0.0, 0.0)
                                } else {
                                    supersaw_spread_geometry(
                                        plan.voices,
                                        (plan.freqspread + mod_adds.freqspread).max(0.0),
                                    )
                                };
                                let mut left = 0.0f32;
                                let mut right = 0.0f32;
                                // `count` is clamped to MAX_UNISON when the
                                // plan is built, so the take never shortens
                                // the loop.
                                for (n, phase) in phases.iter_mut().enumerate().take(plan.count) {
                                    let ratio = if static_spread {
                                        plan.detune_ratios[n]
                                    } else {
                                        supersaw_detune_ratio(n, scale, center)
                                    };
                                    let voice_freq = freq * ratio;
                                    // Euclidean remainder, not `fract` - dt
                                    // must stay non-negative.
                                    let dt = saw_step(voice_freq / sr as f32);
                                    let value = saw_blep(*phase, dt);
                                    left += value * gain_l;
                                    right += value * gain_r;
                                    *phase = wrap_phase(f64::from(*phase + dt)) as f32;
                                    std::mem::swap(&mut gain_l, &mut gain_r);
                                }
                                (left, right)
                            }
                        }
                        VoiceSource::Pulse {
                            pulsewidth,
                            width_lfo,
                            width_lfo_phase,
                            phi,
                            y0,
                            y1,
                            dphif,
                            envf,
                            env: pulse_env,
                        } => {
                            use std::f64::consts::PI;
                            // The begin gate: the source emits nothing, and
                            // advances nothing, until the first context
                            // quantum past the note's begin - the begin the
                            // AudioParam holds, narrowed to f32, which is not
                            // always the quantum after the onset FRAME.
                            if !source_open {
                                (0.0, 0.0)
                            } else {
                                // The per-block decay resets each 128
                                // frames of the CONTEXT grid.
                                if abs.is_multiple_of(128) {
                                    *pulse_env = 1.0;
                                }
                                // The pulse synth's own LFO is a-rate at
                                // its output, but its frequency and depth
                                // params are read once per quantum. Its
                                // clamp bounds were baked from the original
                                // sweep, not from a later modulated depth.
                                let width_lfo_add = match width_lfo {
                                    None => 0.0,
                                    Some(lfo) => {
                                        let hold = v.mod_param_hold.pulse_width;
                                        let rate = lfo.frequency_hz + hold.rate;
                                        let depth = lfo.depth + hold.depth;
                                        let phase = *width_lfo_phase.get_or_insert_with(|| {
                                            lfo_phase0(lfo.time_secs, rate, 0.0)
                                        });
                                        let raw = (lfo_waveshape(0, phase, 0.5) - 0.5) * depth;
                                        let add = js_clamp(raw, -0.5 * lfo.depth, 0.5 * lfo.depth);
                                        *width_lfo_phase =
                                            Some(wrap_phase(phase + f64::from(rate) / sr));
                                        add
                                    }
                                };
                                let width = *pulsewidth + mod_adds.pulse_width + width_lfo_add;
                                let pw = (1.0 - f64::from(width.clamp(-0.99, 0.99))) * PI;
                                let freq = f64::from((v.freq_hz + fm_offset_hz) * pitch_ratio);
                                let dphi = freq * (2.0 * PI) / sr;
                                *dphif += 0.1 * (dphi - *dphif);
                                *pulse_env *= 0.9998;
                                *envf += 0.1 * (*pulse_env - *envf);
                                let mut b = 2.3 * (1.0 - 0.0001 * freq);
                                if b < 0.0 {
                                    b = 0.0;
                                }
                                *phi += *dphif;
                                if *phi >= PI {
                                    *phi -= 2.0 * PI;
                                }
                                let out0 = (*phi + b * *y0).cos();
                                *y0 = 0.5 * (out0 + *y0);
                                let out1 = (*phi + b * *y1 + pw).cos();
                                *y1 = 0.5 * (out1 + *y1);
                                let value = (0.15 * (out0 - out1) * *envf) as f32;
                                (value, value)
                            }
                        }
                        VoiceSource::ByteBeat {
                            expression,
                            program,
                            t,
                            start_offset,
                            gate,
                        } => {
                            if abs < *gate {
                                // Gated: the source has not run yet, so it
                                // emits nothing and advances nothing.
                                (0.0, 0.0)
                            } else {
                                // local_t scales the integer counter by
                                // 256/sampleRate and the voice's frequency,
                                // the expression's low byte becomes a
                                // bipolar signal, and the output is clamped
                                // before anything downstream sees it.
                                let scale = 256.0 / sr;
                                let local_t = scale * f64::from(v.freq_hz + fm_offset_hz) * *t
                                    + *start_offset;
                                // A compiled `byteBeatExpression` replaces the
                                // built-in table; `n` still picks a built-in
                                // when the score gave no expression of its own.
                                let value = match program {
                                    Some(program) => program.eval(local_t),
                                    None => crate::backend::bytebeat_sample(*expression, local_t),
                                };
                                let byte = f64::from(crate::backend::bytebeat_byte(value));
                                let signal = (byte / 127.5 - 1.0) * 0.2;
                                let out = signal.clamp(-0.4, 0.4) as f32;
                                *t += 1.0;
                                (out, out)
                            }
                        }
                        VoiceSource::Bus { bus } => {
                            let tap = bus_now[usize::from(*bus)];
                            (tap[0], tap[1])
                        }
                        VoiceSource::ZzFx { voice, ring } => {
                            let sample = voice
                                .step(ring.as_deref_mut().unwrap_or(&mut []))
                                .unwrap_or(0.0);
                            (sample, sample)
                        }
                        VoiceSource::Input { channel, fm_phase } => {
                            let sample = match (&input, input_base) {
                                (Some(ring), Some((base, ratio))) => {
                                    // The FM matrix bends the read rate, as
                                    // it bends an oscillator's frequency: the
                                    // deviation in Hz against the voice's
                                    // nominal frequency is the rate multiplier
                                    // the reader runs at, integrated into
                                    // `fm_phase`. The multiplier deviation is
                                    // the route amount times the operator
                                    // signal, independent of the note. A read
                                    // that runs past the writer lands in
                                    // silence and one far behind in the
                                    // ring's oldest frames.
                                    let position =
                                        base + (sub_start + i) as f64 * ratio + *fm_phase;
                                    if v.controls.fm.is_some() {
                                        // The matrix output is exactly zero
                                        // when the nominal frequency is zero,
                                        // which keeps the division finite.
                                        if fm_matrix_hz != 0.0 {
                                            *fm_phase += f64::from(fm_matrix_hz)
                                                / f64::from(v.freq_hz)
                                                * ratio;
                                        }
                                        // The integral of a modulator that
                                        // starts at phase zero carries its DC
                                        // as a one-sided LEAD into the
                                        // writer, which a reader one block
                                        // behind has no room for. The leak
                                        // re-centers the swing into the
                                        // ring's history and bounds a
                                        // drifting operator; above a hertz it
                                        // is too slow to hear.
                                        *fm_phase -= *fm_phase * INPUT_FM_LEAK;
                                    }
                                    ring.sample_at(position, usize::from(*channel))
                                }
                                _ => 0.0,
                            };
                            (sample, sample)
                        }
                        VoiceSource::Noise {
                            kind,
                            density,
                            state,
                        } => {
                            // One stream feeding both channels: the source is
                            // mono until the panner, like every other synth.
                            let sample = state.next(*kind, *density);
                            (sample, sample)
                        }
                        VoiceSource::One => (1.0, 1.0),
                        VoiceSource::Sbd {
                            controls: sbd,
                            phase,
                            noise,
                        } => {
                            if sbd_pre_onset {
                                // The scheduled oscillator contributes zero
                                // before source start, while the connected
                                // WaveShaper remains active. Its sample-rate-
                                // length curve is one element short of symmetry,
                                // so interpolating at zero yields the small
                                // negative DC value required by the downstream
                                // signal path.
                                let value = sbd_saturation(0.0, sbd_saturation_curve);
                                (value, value)
                            } else {
                                // Pitch env: detune from penv semitones (penv×100
                                // cents) exponentially to 0.001 cents over pdecay,
                                // then held.
                                let start_cents = f64::from(sbd.penv_semitones) * 100.0;
                                let cents = if start_cents.abs() < f64::EPSILON {
                                    0.0
                                } else if t >= sbd.pdecay_secs {
                                    0.001
                                } else {
                                    start_cents
                                        * (0.001 / start_cents.abs())
                                            .powf(f64::from(t / sbd.pdecay_secs))
                                };
                                let freq = f64::from(v.freq_hz) * 2.0f64.powf(cents / 1200.0);
                                // Use a band-limited PeriodicWave table selected
                                // from the swept pitch, not the base note,
                                // because SBD crosses three octaves.
                                let tri = tables.sample(
                                    crate::backend::Waveform::Triangle,
                                    *phase,
                                    tables.selection(freq as f32),
                                );
                                *phase = wrap_phase(*phase + freq / sr);
                                // WaveShaper saturation, then the drum's own envelope:
                                // hold 1 until 20 ms, exponential to 0.001 over decay.
                                let saturated = sbd_saturation(tri, sbd_saturation_curve);
                                let attackhold = 0.02f32;
                                let body_env = if t <= attackhold {
                                    1.0
                                } else {
                                    0.001f32.powf(((t - attackhold) / sbd.decay_secs).min(1.0))
                                };
                                // Brown noise buffer: gain 1.2 exponentially to
                                // 0.001 over 25 ms, then hold the endpoint.
                                let brown = noise.next(2, 0.0);
                                let noise_env = sbd_noise_gain(t);
                                let mut value = saturated * body_env + brown * noise_env;
                                // 10 ms linear fade landing exactly at the stop.
                                let remaining = sbd.stop_secs - t;
                                if remaining < 0.01 {
                                    value *= (remaining / 0.01).max(0.0);
                                }
                                (value, value)
                            }
                        }
                        VoiceSource::Sample {
                            sample,
                            position,
                            increment,
                            reversed,
                            delay_frames,
                            loop_frames,
                        } => {
                            if bank.get(*sample).is_none() {
                                // The slot was emptied under the voice - an
                                // uninstall landed. Silence for the rest of the
                                // block, and the voice ends with it: the id may
                                // go to another sound, and a voice still reading
                                // the slot would play that one under this name.
                                v.source_gone = true;
                                (0.0, 0.0)
                            } else if *delay_frames > 0 {
                                *delay_frames -= 1;
                                (0.0, 0.0)
                            } else if let Some((loop_start, loop_end)) = *loop_frames
                                && loop_end > loop_start
                                && bank.get(*sample).is_some_and(|decoded| {
                                    *increment * f64::from(sample_pitch)
                                        > loop_end.min(decoded.frames() as f64) - loop_start
                                })
                            {
                                // While the step is longer than the loop, its
                                // end clamped to the buffer, the voice is
                                // silent and its cursor holds, as Chromium's
                                // buffer source does.
                                (0.0, 0.0)
                            } else {
                                let wave = bank
                                    .get(*sample)
                                    .and_then(|decoded| {
                                        if *reversed {
                                            let last = (decoded.frames().max(1) - 1) as f64;
                                            let read = last - *position;
                                            if read < 0.0 {
                                                return None;
                                            }
                                            decoded.stereo_at_reversed_with_mode(
                                                read,
                                                sample_resampling_mode,
                                            )
                                        } else {
                                            decoded.stereo_at_with_mode(
                                                *position,
                                                sample_resampling_mode,
                                            )
                                        }
                                    })
                                    .unwrap_or((0.0, 0.0));
                                *position += *increment * f64::from(sample_pitch);
                                if let Some((loop_start, loop_end)) = loop_frames
                                    && *position >= *loop_end
                                    && *loop_end > *loop_start
                                {
                                    *position -= *loop_end - *loop_start;
                                }
                                wave
                            }
                        }
                        // The wavetable has the same begin gate as the pulse
                        // oscillator and the LFOs. Until the first context
                        // quantum strictly after its onset, it writes
                        // nothing and does not advance its phase.
                        VoiceSource::Wavetable { .. } if !source_open => (0.0, 0.0),
                        VoiceSource::Wavetable { controls, .. }
                            if bank.get(controls.table).is_none() =>
                        {
                            // The table's slot was emptied under the voice.
                            v.source_gone = true;
                            (0.0, 0.0)
                        }
                        VoiceSource::Wavetable {
                            controls,
                            phases,
                            lfo_phase,
                            warp_lfo_phase,
                        } => Self::wavetable_sample(
                            bank,
                            controls,
                            phases,
                            lfo_phase,
                            warp_lfo_phase,
                            v.start_frame as f64 / sr,
                            v.freq_hz * pitch_ratio,
                            t,
                            v.duration_secs,
                            sr as f32,
                            WavetableAdds {
                                position: mod_adds.position,
                                freqspread: mod_adds.freqspread,
                                panspread: mod_adds.panspread,
                                lfo_rate: v.mod_param_hold.wt.rate,
                                lfo_depth: v.mod_param_hold.wt.depth,
                                lfo_skew: v.mod_param_hold.wt.skew,
                                warp: mod_adds.warp,
                                warp_lfo_rate: v.mod_param_hold.warp.rate,
                                warp_lfo_depth: v.mod_param_hold.warp.depth,
                                warp_lfo_skew: v.mod_param_hold.warp.skew,
                            },
                            wavetable_kernel,
                        ),
                    };
                    // `.FX(...)` stages run BEFORE the voice's own params:
                    // `FX = [...FX, value]` appends the hap's controls last,
                    // so each stage is a complete effects pass the signal has
                    // already been through by the time the main chain sees it.
                    let block_n = (abs % 128) as f32;
                    // The source's amplitude envelope applies before the
                    // stages: the envelope gain sits between the source and
                    // the FX loop. The order matters. Crush, shape and
                    // distort are non-linear, so the raw oscillator would
                    // quantise and clip at the wrong level.
                    let has_stages = v.fx_stage_state.is_some();
                    let stretched = v.stretch.is_some() && v.controls.stretch.is_some();
                    // The vocoder and the transient shaper take the same
                    // split as the stages: both are fed the source BEFORE
                    // the main gain, the vocoder's delay leads a gain that
                    // moves, and a nonlinear shaper hears the level it is
                    // given.
                    let has_kabel = v.kabel.is_some()
                        || v.fx_stage_state.as_ref().is_some_and(|stages| {
                            stages.iter().flatten().any(|stage| stage.kabel.is_some())
                        });
                    let split_gain = has_kabel
                        || (has_stages || stretched || v.transient.is_some())
                            && v.fx_post_gain != 0.0;
                    // With no stages the split is invisible, so keep the
                    // single multiply the rest of the engine expects.
                    let scale = if split_gain && v.fx_post_gain == 0.0 {
                        0.0
                    } else if split_gain {
                        v.amplitude_scale / v.fx_post_gain * env
                    } else {
                        v.amplitude_scale * env * live_gain
                    };
                    let (mut wave_l, mut wave_r) = (wave_l * scale, wave_r * scale);
                    if let Some(states) = &mut v.fx_stage_state {
                        for (index, state) in states.iter_mut().enumerate() {
                            let Some(state) = state else {
                                continue;
                            };
                            let stage_adds =
                                mod_adds.fx_stage.get(index).copied().unwrap_or_default();
                            (wave_l, wave_r) = apply_fx_stage(
                                wave_l,
                                wave_r,
                                state,
                                t,
                                v.hap_duration_secs,
                                block_n,
                                sr as f32,
                                (abs / 128 * 128) as f64 / sr,
                                transient_open && note_alive,
                                lfo_live,
                                &stage_adds,
                            );
                        }
                    }
                    if let Some(kabel) = &mut v.kabel {
                        (wave_l, wave_r) = kabel.process(wave_l, (abs / 128 * 128) as f64 / sr, sr);
                    }
                    // Signal order: earlier .FX() stages -> vocoder ->
                    // transient shaper -> main gain -> filters -> ... ->
                    // sends. The vocoder comes first, as in each stage, so
                    // everything downstream, the sends included, hears the
                    // shifted signal.
                    if let (Some(vocoder), Some(factor)) = (&mut v.stretch, v.controls.stretch) {
                        wave_l = vocoder[0].process(wave_l, factor);
                        // A mono chain carries one channel; the right is its
                        // copy.
                        wave_r = if v.filters_right.is_some() {
                            vocoder[1].process(wave_r, factor)
                        } else {
                            wave_l
                        };
                    }
                    // The shaper is nonlinear, so moving it across the gain
                    // or filters changes the sound.
                    if v.transient.is_some() {
                        if transient_open && note_alive {
                            if v.filters_right.is_some() {
                                let (l, r) = v
                                    .transient
                                    .as_mut()
                                    .map_or((wave_l, wave_r), |s| s.process(wave_l, wave_r));
                                wave_l = l;
                                wave_r = r;
                            } else {
                                // A mono chain reaches the shaper as ONE
                                // channel, and one follower runs.
                                let mono = v
                                    .transient
                                    .as_mut()
                                    .map_or(wave_l, |s| s.process_mono(wave_l));
                                wave_l = mono;
                                wave_r = mono;
                            }
                        } else {
                            // The shaper feeds silence into the downstream chain
                            // before it opens and after hold plus release.
                            wave_l = 0.0;
                            wave_r = 0.0;
                        }
                    }
                    if split_gain {
                        wave_l *= v.fx_post_gain * live_gain;
                        wave_r *= v.fx_post_gain * live_gain;
                    }
                    if let Some(filters_right) = &mut v.filters_right {
                        // STEREO chain: each channel filters and shapes
                        // independently, then the stereo pan law mixes.
                        let (mut left, mut right) = if v.has_filters {
                            (
                                v.filters.process_modulated(
                                    wave_l,
                                    t,
                                    v.hap_duration_secs,
                                    filter_adds,
                                    filter_q_adds,
                                ),
                                filters_right.process_modulated(
                                    wave_r,
                                    t,
                                    v.hap_duration_secs,
                                    filter_adds,
                                    filter_q_adds,
                                ),
                            )
                        } else {
                            (wave_l, wave_r)
                        };
                        if v.has_pre_distort_fx {
                            let worklets = FxWorkletControls {
                                vowel: v.controls.vowel.as_ref(),
                                coarse: v.controls.coarse,
                                crush: v.controls.crush,
                                shape: v.controls.shape,
                            };
                            let block_n = (abs % 128) as f32;
                            left = voice_fx_pre_distort(
                                left,
                                0,
                                worklets,
                                &vowel_coefs,
                                &mut v.vowel_filters,
                                &mut v.coarse_hold,
                                block_n,
                                &v.fx_mod_hold,
                            );
                            right = voice_fx_pre_distort(
                                right,
                                1,
                                worklets,
                                &vowel_coefs,
                                &mut v.vowel_filters,
                                &mut v.coarse_hold,
                                block_n,
                                &v.fx_mod_hold,
                            );
                        }
                        if let Some(distort) = v.controls.distort {
                            // a-rate: DistortProcessor reads per sample.
                            let curve = match v.distort_curve {
                                Some(curve) if mod_adds.distort == 0.0 => curve,
                                _ => distort.curve(distort.shape_with(mod_adds.distort)),
                            };
                            let (l, r) = (curve.apply(left), curve.apply(right));
                            let vol = mod_adds.distort_vol;
                            let (l, r) = if vol == 0.0 {
                                (l, r)
                            } else {
                                let scale = distort.postgain_scale(vol);
                                (l * scale, r * scale)
                            };
                            left = l;
                            right = r;
                        }
                        left *= trem_gain;
                        right *= trem_gain;
                        let (left, right) = match (&mut v.compressor, v.controls.compressor) {
                            (Some(state), Some(c)) => {
                                state.process(left, right, &mod_adds.compressor(c), sr as f32)
                            }
                            _ => (left, right),
                        };
                        let (left, right) = stereo_pan(left, right, pan_param);
                        // Phaser sits AFTER the panner,
                        // one notch state per channel.
                        let (mut left, mut right) = if let Some(coefs) = &phaser_coefs {
                            (
                                v.phaser_notch[0].process(coefs, left),
                                v.phaser_notch[1].process(coefs, right),
                            )
                        } else {
                            (left, right)
                        };
                        if !post_insert {
                            left *= post_gain;
                            right *= post_gain;
                        }
                        // This bounds the native voice. Insert effects and
                        // their postgain follow it and can exceed its ceiling.
                        if let Some(limiter) = v.limiter.as_mut() {
                            let (l, r) = limiter.process_frame(left, right);
                            left = l;
                            right = r;
                        }
                        let send_gain = if post_insert { post_gain } else { 1.0 };
                        let (send_left, send_right) = (left * send_gain, right * send_gain);
                        let delay_wet = (v.delay_wet + mod_adds.delay_send).max(0.0);
                        if delay_wet > 0.0 && v.delay_wet > 0.0 {
                            orbit_inputs[v.orbit][0] += send_left * delay_wet;
                            orbit_inputs[v.orbit][1] += send_right * delay_wet;
                        }
                        let reverb_wet = (v.reverb_wet + mod_adds.room_send).max(0.0);
                        if reverb_wet > 0.0 && v.reverb_wet > 0.0 {
                            sends[v.orbit][0][i] += send_left * reverb_wet;
                            sends[v.orbit][1][i] += send_right * reverb_wet;
                        }
                        // `effectSend(post, busNode, busgain)` taps the voice
                        // BEFORE its dry gain, so a `.dry(0)` sender is still
                        // heard by whatever modulates off its bus.
                        if let Some(bus) = v.controls.bus {
                            let send = v.controls.busgain;
                            let slot = &mut bus_now[usize::from(bus)];
                            slot[0] += send_left * send;
                            slot[1] += send_right * send;
                        }
                        let dry = v.dry_gain + mod_adds.dry;
                        let (left, right) = (left * dry, right * dry);
                        let mix = if v.piano {
                            &mut piano_mix
                        } else if v.through_insert {
                            &mut insert_blocks[v.insert_bus.expect("insert voice bus")]
                        } else {
                            &mut blocks[v.orbit]
                        };
                        mix[0][i] += left;
                        mix[1][i] += right;
                        let mut slots = if v.generation >= visual_capture_generation_floor {
                            v.ui_visuals & visual_capture_mask
                        } else {
                            0
                        };
                        while slots != 0 {
                            let slot = slots.trailing_zeros() as usize;
                            let at = (block_offset + sub_start + i) * 2;
                            if let Some(block) = visual_blocks.get_mut(slot)
                                && at + 1 < block.len()
                            {
                                block[at] += left * send_gain;
                                block[at + 1] += right * send_gain;
                            }
                            slots &= slots - 1;
                        }
                    } else {
                        let mut sample = if v.has_filters {
                            v.filters.process_modulated(
                                wave_l,
                                t,
                                v.hap_duration_secs,
                                filter_adds,
                                filter_q_adds,
                            )
                        } else {
                            wave_l
                        };
                        // Mono chain: the modulated pan param re-derives the
                        // equal-power channel gains per frame.
                        let (lg, rg) = if mod_adds.pan != 0.0 {
                            Self::channel_gains(Some((pan_param + 1.0) * 0.5))
                        } else {
                            (v.left_gain, v.right_gain)
                        };
                        if v.has_pre_distort_fx {
                            sample = voice_fx_pre_distort(
                                sample,
                                0,
                                FxWorkletControls {
                                    vowel: v.controls.vowel.as_ref(),
                                    coarse: v.controls.coarse,
                                    crush: v.controls.crush,
                                    shape: v.controls.shape,
                                },
                                &vowel_coefs,
                                &mut v.vowel_filters,
                                &mut v.coarse_hold,
                                (abs % 128) as f32,
                                &v.fx_mod_hold,
                            );
                        }
                        // FX-loop distortion runs after the filter chain
                        // and before the panner, one sample at a time with
                        // no oversampling.
                        if let Some(distort) = v.controls.distort {
                            let curve = match v.distort_curve {
                                Some(curve) if mod_adds.distort == 0.0 => curve,
                                _ => distort.curve(distort.shape_with(mod_adds.distort)),
                            };
                            sample = curve.apply(sample);
                            if mod_adds.distort_vol != 0.0 {
                                sample *= distort.postgain_scale(mod_adds.distort_vol);
                            }
                        }
                        sample *= trem_gain;
                        if let (Some(state), Some(c)) = (&mut v.compressor, v.controls.compressor) {
                            let c = mod_adds.compressor(c);
                            sample = state.process(sample, sample, &c, sr as f32).0;
                        }
                        // Phaser belongs post-pan, but the mono path's pan
                        // is a static per-voice gain pair, which commutes
                        // with the (linear) notch - one state on the mono
                        // signal.
                        if let Some(coefs) = &phaser_coefs {
                            sample = v.phaser_notch[0].process(coefs, sample);
                        }
                        if !post_insert {
                            sample *= post_gain;
                        }
                        // The same native limiter as the stereo branch,
                        // with identical input channels.
                        if let Some(limiter) = v.limiter.as_mut() {
                            sample = limiter.process_frame(sample, sample).0;
                        }
                        let send_gain = if post_insert { post_gain } else { 1.0 };
                        let send_sample = sample * send_gain;
                        // The delay send taps after the chain's panner and
                        // post-gain, so echoes keep the voice's stereo image.
                        let delay_wet = (v.delay_wet + mod_adds.delay_send).max(0.0);
                        if delay_wet > 0.0 && v.delay_wet > 0.0 {
                            orbit_inputs[v.orbit][0] += send_sample * lg * delay_wet;
                            orbit_inputs[v.orbit][1] += send_sample * rg * delay_wet;
                        }
                        let reverb_wet = (v.reverb_wet + mod_adds.room_send).max(0.0);
                        if reverb_wet > 0.0 && v.reverb_wet > 0.0 {
                            sends[v.orbit][0][i] += send_sample * lg * reverb_wet;
                            sends[v.orbit][1][i] += send_sample * rg * reverb_wet;
                        }
                        // The same send as the stereo branch above. A mono
                        // voice is the ordinary case for `s("sine")`, so
                        // sending only on the stereo path left every bus empty
                        // while every part looked wired.
                        if let Some(bus) = v.controls.bus {
                            let send = v.controls.busgain;
                            let slot = &mut bus_now[usize::from(bus)];
                            slot[0] += send_sample * lg * send;
                            slot[1] += send_sample * rg * send;
                        }
                        let dry = v.dry_gain + mod_adds.dry;
                        let (left, right) = (sample * lg * dry, sample * rg * dry);
                        let mix = if v.piano {
                            &mut piano_mix
                        } else if v.through_insert {
                            &mut insert_blocks[v.insert_bus.expect("insert voice bus")]
                        } else {
                            &mut blocks[v.orbit]
                        };
                        mix[0][i] += left;
                        mix[1][i] += right;
                        let mut slots = if v.generation >= visual_capture_generation_floor {
                            v.ui_visuals & visual_capture_mask
                        } else {
                            0
                        };
                        while slots != 0 {
                            let slot = slots.trailing_zeros() as usize;
                            let at = (block_offset + sub_start + i) * 2;
                            if let Some(block) = visual_blocks.get_mut(slot)
                                && at + 1 < block.len()
                            {
                                block[at] += left * send_gain;
                                block[at + 1] += right * send_gain;
                            }
                            slots &= slots - 1;
                        }
                    }
                    if v.has_param_modulators && lfo_live {
                        if abs & 127 == 0 {
                            djf_mods[v.orbit] += mod_adds.djf;
                        }
                        delay_time_mods[v.orbit] += mod_adds.delay_time;
                        delay_feedback_mods[v.orbit] += mod_adds.delay_feedback;
                    }
                }
                let mut buses = effect_buses;
                while buses != 0 {
                    let bus = buses.trailing_zeros() as usize;
                    let level = &mut insert_postgains[bus];
                    level.applied = level.value + level.offset.next();
                    insert_postgain_blocks[bus][i] = level.applied;
                    let fed = insert_blocks[bus][0][i] != 0.0 || insert_blocks[bus][1][i] != 0.0;
                    level.unfed = if fed {
                        0
                    } else {
                        level.unfed.saturating_add(1)
                    };
                    buses &= buses - 1;
                }
                // Every voice has now had its say about this sample, so what
                // the senders built becomes what the receivers read next.
                // Doing it here rather than per voice is what keeps a bus
                // independent of the order voices happen to sit in.
                for bus in bus_now.iter_mut() {
                    *bus = [0.0; 2];
                }
                if abs & 127 == 0 {
                    for (orbit, state) in djfs.iter_mut().enumerate() {
                        state.modulation = djf_mods[orbit];
                    }
                }
                for (orbit, bus) in self.orbit_delays.iter_mut().enumerate() {
                    if bus.active {
                        bus.block.input[0][i] = orbit_inputs[orbit][0];
                        bus.block.input[1][i] = orbit_inputs[orbit][1];
                        bus.block.time_mod[i] = delay_time_mods[orbit];
                        bus.block.feedback_mod[i] = delay_feedback_mods[orbit];
                    }
                }
            }
            let add_insert_output =
                |mix: &mut [[f32; crate::reverb::REVERB_BLOCK]; 2], left: &[f32], right: &[f32]| {
                    for (channel, wet) in [(0, left), (1, right)] {
                        for (out, wet) in mix[channel][..sub_len].iter_mut().zip(wet) {
                            *out += *wet;
                        }
                    }
                };
            // The first stage of `bus` that wakes, from the place `first` of
            // its order on: its place, and whether its input is fed.
            let wakes = |bus: usize,
                         first: usize,
                         effects: &[Option<Box<dyn crate::insert::OrbitInsert>>],
                         mixes: &[InsertMix],
                         block: &[[f32; crate::reverb::REVERB_BLOCK]; 2],
                         idle_frames: &[u32]| {
                ordered_stages(&effect_order[bus], first).find_map(|(place, stage)| {
                    let slot = stage * crate::insert::MAX_INSERT_BUSES + bus;
                    let effect = effects[slot].as_deref()?;
                    let (left, right) = (&block[0][..sub_len], &block[1][..sub_len]);
                    let idle = idle_frames[slot];
                    effect_stage_wakes(effect, left, right, idle, insert_idle_limit, &mixes[slot])
                        .map(|fed| (place, fed))
                })
            };
            let submit_effect = |bus: usize,
                                 first: usize,
                                 effects: &mut [Option<Box<dyn crate::insert::OrbitInsert>>],
                                 mixes: &[InsertMix],
                                 dry: &mut [[f32; crate::reverb::REVERB_BLOCK]; 2],
                                 block: &mut [[f32; crate::reverb::REVERB_BLOCK]; 2],
                                 idle_frames: &[u32]| {
                let (place, fed) = wakes(bus, first, effects, mixes, block, idle_frames)?;
                let slot =
                    usize::from(effect_order[bus][place]) * crate::insert::MAX_INSERT_BUSES + bus;
                let effect = effects[slot].as_mut()?;
                let (left, right) = block.split_at_mut(1);
                let (left, right) = (&mut left[0][..sub_len], &mut right[0][..sub_len]);
                // A stage whose levels move keeps its input for the mix.
                if mixes[slot].moving() {
                    dry[0][..sub_len].copy_from_slice(left);
                    dry[1][..sub_len].copy_from_slice(right);
                }
                effect.submit(left, right);
                Some((place, fed))
            };
            // Starts the effects of `bus`. When each effect of the chain runs
            // on any thread, the chain goes to a job from its first stage
            // that wakes, and the job runs each stage after it in the order.
            // A copy in a leaving place goes with the job while it fades. A
            // chain with no stage awake posts nothing.
            let start_effects = |bus: usize,
                                 effects: &mut [Option<Box<dyn crate::insert::OrbitInsert>>],
                                 leaving: &mut [Option<Box<dyn crate::insert::OrbitInsert>>],
                                 mixes: &[InsertMix],
                                 dry: &mut [[f32; crate::reverb::REVERB_BLOCK]; 2],
                                 block: &mut [[f32; crate::reverb::REVERB_BLOCK]; 2],
                                 idle_frames: &[u32],
                                 jobs: Option<&mut JoinGuard<'_>>,
                                 pooled: &mut u64| {
                let Some(jobs) = jobs.filter(|_| {
                    (0..crate::insert::EFFECT_CHAIN).all(|stage| {
                        let slot = stage * crate::insert::MAX_INSERT_BUSES + bus;
                        let swapping = mixes[slot].leaving.is_moving();
                        let leaving = leaving[slot].as_ref().filter(|_| swapping);
                        [effects[slot].as_ref(), leaving]
                            .into_iter()
                            .flatten()
                            .all(|effect| effect.any_thread())
                    })
                }) else {
                    return submit_effect(bus, 0, effects, mixes, dry, block, idle_frames);
                };
                let (first, fed) = wakes(bus, 0, effects, mixes, block, idle_frames)?;
                let job = jobs.job(JobKind::Effect, bus);
                job.frames = sub_len;
                for (input, output) in block.iter().zip(&mut job.block) {
                    output[..sub_len].copy_from_slice(&input[..sub_len]);
                }
                for stage in 0..crate::insert::EFFECT_CHAIN {
                    let slot = stage * crate::insert::MAX_INSERT_BUSES + bus;
                    let swapping = mixes[slot].leaving.is_moving();
                    job.idle[stage] = idle_frames[slot];
                    job.boxes[stage] = effects[slot].take();
                    job.leaving[stage] = leaving[slot].take_if(|_| swapping);
                    job.mixes[stage] = mixes[slot];
                }
                job.order = effect_order[bus];
                (job.first, job.fed) = (first, fed);
                job.idle_limit = insert_idle_limit;
                jobs.post(JobKind::Effect, bus);
                *pooled |= 1u64 << bus;
                None
            };
            let collect_effect_chains =
                |effects: &mut [Option<Box<dyn crate::insert::OrbitInsert>>],
                 leaving: &mut [Option<Box<dyn crate::insert::OrbitInsert>>],
                 mixes: &mut [InsertMix],
                 insert_dry: &mut [[[f32; crate::reverb::REVERB_BLOCK]; 2]],
                 insert_leaving_blocks: &mut [[[f32; crate::reverb::REVERB_BLOCK]; 2]],
                 insert_blocks: &mut [[[f32; crate::reverb::REVERB_BLOCK]; 2]],
                 insert_idle_frames: &mut [u32],
                 pending: &mut [Option<(usize, bool)>]| {
                    // The effects take the notes that ask for an effect, each stage
                    // of the order after the stage before, and the output of the
                    // chain joins the orbit before the duck gain. An effect runs on
                    // silent input so the tail rings out, then sleeps after
                    // `INSERT_IDLE_SECONDS` with no input and no output.
                    // Collect longer remaining chains first to start their next
                    // stages sooner. Snapshot each round so every chain advances once.
                    for _ in 0..crate::insert::EFFECT_CHAIN {
                        let mut depths = [0u64; crate::insert::EFFECT_CHAIN];
                        for (bus, pending) in pending.iter().enumerate() {
                            let Some((place, _)) = pending else { continue };
                            let depth = ordered_stages(&effect_order[bus], *place)
                                .filter(|(_, stage)| {
                                    effects[stage * crate::insert::MAX_INSERT_BUSES + bus].is_some()
                                })
                                .count();
                            if depth != 0 {
                                depths[depth - 1] |= 1u64 << bus;
                            }
                        }
                        for mut buses in depths.into_iter().rev() {
                            while buses != 0 {
                                let bus = buses.trailing_zeros() as usize;
                                buses &= buses - 1;
                                let Some((place, fed)) = pending[bus].take() else {
                                    continue;
                                };
                                let stage = usize::from(effect_order[bus][place]);
                                let slot = stage * crate::insert::MAX_INSERT_BUSES + bus;
                                let Some(effect) = effects[slot].as_mut() else {
                                    continue;
                                };
                                let (left, right) = insert_blocks[bus].split_at_mut(1);
                                let (left, right) =
                                    (&mut left[0][..sub_len], &mut right[0][..sub_len]);
                                effect.collect(left, right);
                                let silent = settle_insert_block(left, right);
                                let idle = &mut insert_idle_frames[slot];
                                *idle = if fed || !silent {
                                    0
                                } else {
                                    idle.saturating_add(sub_len as u32)
                                };
                                if mixes[slot].moving() {
                                    fade_effect_stage(
                                        left,
                                        right,
                                        &insert_dry[bus],
                                        leaving[slot].as_deref_mut(),
                                        &mut insert_leaving_blocks[bus],
                                        &mut mixes[slot],
                                    );
                                }
                                pending[bus] = submit_effect(
                                    bus,
                                    place + 1,
                                    effects,
                                    mixes,
                                    &mut insert_dry[bus],
                                    &mut insert_blocks[bus],
                                    insert_idle_frames,
                                );
                            }
                        }
                    }
                };
            let mut pending = [None; crate::insert::MAX_INSERT_BUSES];
            // Native chains can run while instruments produce their input.
            for (bus, pending) in pending.iter_mut().enumerate() {
                if submitted & (1u64 << bus) != 0 && instrument_to_effect[bus] {
                    continue;
                }
                *pending = start_effects(
                    bus,
                    effects,
                    leaving_effects,
                    effect_mixes,
                    &mut insert_dry[bus],
                    &mut insert_blocks[bus],
                    insert_idle_frames,
                    jobs.as_mut(),
                    &mut pooled_effects,
                );
            }
            // A pooled instrument is out of its slot: the masks say which
            // instruments render.
            let wait_for_dry = (0..crate::insert::MAX_INSERT_BUSES).any(|bus| {
                submitted & (1u64 << bus) != 0
                    && !(instrument_to_effect[bus]
                        && effect_order[bus][0] != crate::insert::NO_STAGE)
            });
            let mut dry_instruments = 0u64;
            let mut collected_instruments = 0u64;
            // Finish effect chains while instruments with no effects keep
            // rendering. Their sends wait below to preserve orbit sum order.
            for collect_effects in [true, false] {
                // Each bus of the pass once, in the order its block is ready:
                // a block a worker finished, else a block on the callback,
                // else the wait for a worker. A pass writes only the state of
                // its own bus, so the order keeps the output.
                let mut buses = (0..crate::insert::MAX_INSERT_BUSES).fold(0u64, |buses, bus| {
                    let through_effect = instrument_to_effect[bus]
                        && effect_order[bus][0] != crate::insert::NO_STAGE;
                    buses | u64::from(through_effect == collect_effects) << bus
                }) & submitted;
                while buses != 0 {
                    let on_workers = buses & pooled_instruments;
                    let bus = match jobs.as_mut().filter(|_| on_workers != 0) {
                        Some(jobs) => match jobs.try_join(JobKind::Instrument, on_workers) {
                            Some(bus) => bus,
                            None if buses != on_workers => {
                                (buses & !on_workers).trailing_zeros() as usize
                            }
                            None => jobs.join(JobKind::Instrument, on_workers),
                        },
                        None => buses.trailing_zeros() as usize,
                    };
                    buses &= !(1u64 << bus);
                    let through_effect = instrument_to_effect[bus]
                        && effect_order[bus][0] != crate::insert::NO_STAGE;
                    let pooled = pooled_instruments & (1u64 << bus) != 0;
                    let slot = &mut instruments[bus];
                    let (left, right) = instrument_blocks[bus].split_at_mut(1);
                    let (left, right) = (&mut left[0][..sub_len], &mut right[0][..sub_len]);
                    // The instrument that leaves the slot in a fade renders on
                    // silence too.
                    let mix = &mut instrument_mixes[bus];
                    let swapping = mix.leaving.is_moving();
                    let leaving_block = &mut insert_leaving_blocks[bus];
                    if let Some(jobs) = jobs.as_mut().filter(|_| pooled) {
                        // The job rendered the blocks, and the boxes come back.
                        let job = jobs.job(JobKind::Instrument, bus);
                        left.copy_from_slice(&job.block[0][..sub_len]);
                        right.copy_from_slice(&job.block[1][..sub_len]);
                        *slot = job.boxes[0].take();
                        if let Some(leaving) = job.leaving[0].take() {
                            for (output, block) in job.leaving_block.iter().zip(&mut *leaving_block)
                            {
                                block[..sub_len].copy_from_slice(&output[..sub_len]);
                            }
                            leaving_instruments[bus] = Some(leaving);
                        }
                    }
                    let Some(instrument) = slot else { continue };
                    collected_instruments |= 1u64 << bus;
                    if !pooled {
                        // An offline render reads faster than real time, so
                        // it waits for the block before it collects it.
                        if offline {
                            instrument.flush(sub_len);
                        }
                        instrument.collect(left, right);
                        if swapping && let Some(leaving) = leaving_instruments[bus].as_deref_mut() {
                            render_leaving_insert(leaving, None, leaving_block, sub_len, offline);
                        }
                    }
                    let mut silent = settle_insert_block(left, right);
                    if swapping {
                        // The instrument that leaves fades out under the new one.
                        let [old_left, old_right] = &*leaving_block;
                        for (i, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate()
                        {
                            let level = mix.leaving.next();
                            *left += old_left[i] * level;
                            *right += old_right[i] * level;
                        }
                    }
                    let key = instrument.key();
                    let gain = &mut instrument_gains[bus];
                    let filters = &mut instrument_filters[bus];
                    // In a swap the level, the place and the sends move from
                    // those of the instrument that leaves.
                    if gain.key != key && !swapping {
                        *gain = InstrumentGain::default();
                        *filters = [FilterChain::default(); 2];
                    }
                    let first = instrument_gain_changes
                        .partition_point(|change| (change.bus, change.frame) < (bus, sub_start));
                    let mut changes = instrument_gain_changes[first..].iter().peekable();
                    for (i, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
                        while let Some(change) = changes.peek()
                            && change.bus == bus
                            && change.frame <= sub_start + i
                        {
                            if change.value.key == key {
                                let mut next = change.value;
                                if gain.key == next.key || swapping {
                                    next.gain.continue_from(gain.gain);
                                    next.cutoff.continue_from(gain.cutoff);
                                    next.resonance.continue_from(gain.resonance);
                                    // The notes before still sound: each
                                    // value moves, and does not step.
                                    let rate = self.sample_rate;
                                    let sounds = gain.quiet < INSERT_QUIET_FRAMES;
                                    if sounds {
                                        next.gain.declick_from(gain.gain, rate);
                                        next.reverb_wet.declick_from(gain.reverb_wet, rate);
                                        next.delay_wet.declick_from(gain.delay_wet, rate);
                                        next.dry.declick_from(gain.dry, rate);
                                        next.pan_at.declick_from(gain.pan_at, rate);
                                    }
                                    // The level after the effects acts on
                                    // their tail too.
                                    let chain = &insert_postgains[bus];
                                    let tail = through_effect
                                        && chain.quiet.min(chain.unfed) < INSERT_QUIET_FRAMES;
                                    if sounds || tail {
                                        next.postgain.declick_from(gain.postgain, rate);
                                    }
                                    next.quiet = gain.quiet;
                                }
                                for filter in &mut *filters {
                                    filter.reconfigure(next.filters, self.sample_rate, next.onset);
                                }
                                *gain = next;
                            }
                            changes.next();
                        }
                        let mut sounds =
                            left.abs() >= INSERT_SILENCE || right.abs() >= INSERT_SILENCE;
                        let frame = start + (sub_start + i) as u64;
                        if frame >= insert_fade_from[bus] {
                            let fade =
                                Self::insert_fade(insert_fade_from[bus], frame, self.sample_rate);
                            *left *= fade;
                            *right *= fade;
                        }
                        let mut level = gain.gain.next();
                        let postgain = gain.postgain.next();
                        let (reverb_wet, delay_wet) =
                            (gain.reverb_wet.next(), gain.delay_wet.next());
                        let dry = gain.dry.next();
                        // A pan in a move to the middle is still a pan.
                        let panned = gain.pan.is_some() || gain.pan_at.is_moving();
                        let pan_at = gain.pan_at.next();
                        let cutoff = gain.cutoff.next();
                        if gain.cutoff.binding != 0 {
                            for filter in &mut *filters {
                                filter.set_lowpass_frequency(cutoff);
                            }
                        }
                        let resonance = gain.resonance.next();
                        if gain.resonance.binding != 0 {
                            for filter in &mut *filters {
                                filter.set_lowpass_q(resonance);
                            }
                        }
                        if filters[0].is_active() || panned {
                            let elapsed =
                                (start + (sub_start + i) as u64).saturating_sub(gain.onset) as f32
                                    / self.sample_rate as f32;
                            *left = filters[0].process_modulated(
                                *left * level,
                                elapsed,
                                gain.duration_secs,
                                [0.0; 3],
                                [0.0; 3],
                            );
                            *right = filters[1].process_modulated(
                                *right * level,
                                elapsed,
                                gain.duration_secs,
                                [0.0; 3],
                                [0.0; 3],
                            );
                            if panned {
                                (*left, *right) = stereo_pan(*left, *right, pan_at * 2.0 - 1.0);
                            }
                            silent &= left.abs() < INSERT_SILENCE && right.abs() < INSERT_SILENCE;
                            // A filter rings after the end of its input.
                            sounds |= left.abs() >= INSERT_SILENCE || right.abs() >= INSERT_SILENCE;
                            level = 1.0;
                        }
                        gain.quiet = if sounds {
                            0
                        } else {
                            gain.quiet.saturating_add(1)
                        };
                        let send_left = *left * (level * postgain);
                        let send_right = *right * (level * postgain);
                        instrument_reverb_sends[bus][0][i] = send_left * reverb_wet;
                        instrument_reverb_sends[bus][1][i] = send_right * reverb_wet;
                        instrument_delay_sends[bus][0][i] = send_left * delay_wet;
                        instrument_delay_sends[bus][1][i] = send_right * delay_wet;
                        if through_effect {
                            insert_postgain_blocks[bus][i] = postgain;
                            let chain = &mut insert_postgains[bus];
                            (chain.value, chain.applied) = (postgain, postgain);
                        } else {
                            level *= postgain;
                        }
                        *left *= level;
                        *right *= level;
                        *left *= dry;
                        *right *= dry;
                    }
                    if through_effect {
                        add_insert_output(&mut insert_blocks[bus], left, right);
                    } else {
                        dry_instruments |= 1u64 << bus;
                    }
                    let idle = &mut insert_idle_frames
                        [crate::insert::EFFECT_CHAIN * crate::insert::MAX_INSERT_BUSES + bus];
                    *idle = if silent {
                        idle.saturating_add(sub_len as u32)
                    } else {
                        0
                    };
                    if through_effect {
                        pending[bus] = start_effects(
                            bus,
                            effects,
                            leaving_effects,
                            effect_mixes,
                            &mut insert_dry[bus],
                            &mut insert_blocks[bus],
                            insert_idle_frames,
                            jobs.as_mut(),
                            &mut pooled_effects,
                        );
                    }
                }
                if !collect_effects {
                    break;
                }
                if wait_for_dry {
                    collect_effect_chains(
                        effects,
                        leaving_effects,
                        effect_mixes,
                        insert_dry,
                        insert_leaving_blocks,
                        insert_blocks,
                        insert_idle_frames,
                        &mut pending,
                    );
                }
            }
            while collected_instruments != 0 {
                let bus = collected_instruments.trailing_zeros() as usize;
                collected_instruments &= collected_instruments - 1;
                let orbit = insert_outputs[bus];
                for i in 0..sub_len {
                    sends[orbit][0][i] += instrument_reverb_sends[bus][0][i];
                    sends[orbit][1][i] += instrument_reverb_sends[bus][1][i];
                    self.orbit_delays[orbit].block.input[0][i] += instrument_delay_sends[bus][0][i];
                    self.orbit_delays[orbit].block.input[1][i] += instrument_delay_sends[bus][1][i];
                }
            }
            // Instrument sends join native sends before their returns are mixed.
            for (bus, block) in self.orbit_delays.iter_mut().zip(blocks.iter_mut()) {
                if !bus.active {
                    continue;
                }
                let (left, right) = block.split_at_mut(1);
                for (i, (left, right)) in left[0][..sub_len]
                    .iter_mut()
                    .zip(&mut right[0][..sub_len])
                    .enumerate()
                {
                    // WebAudio cycles carry an implicit render-quantum (128
                    // frames) of extra latency on the FEEDBACK edge, so the n-th
                    // echo lands at n·time + (n−1)·128 frames. The line stores
                    // the delay INPUT stream: out(t) = in(t−D), and the feedback
                    // term re-injects out delayed by one quantum
                    // (in(t−D−128)·feedback).
                    const FEEDBACK_QUANTUM: usize = 128;
                    let delay_frames =
                        bus.time_frames as f32 + bus.block.time_mod[i] * self.sample_rate as f32;
                    let out_left = OrbitDelay::read(&bus.left, bus.write, delay_frames);
                    let out_right = OrbitDelay::read(&bus.right, bus.write, delay_frames);
                    *left += out_left;
                    *right += out_right;

                    let feedback_left = OrbitDelay::read(
                        &bus.left,
                        bus.write,
                        delay_frames + FEEDBACK_QUANTUM as f32,
                    );
                    let feedback_right = OrbitDelay::read(
                        &bus.right,
                        bus.write,
                        delay_frames + FEEDBACK_QUANTUM as f32,
                    );
                    // Each voice adds its modulator sum. A feedback above 1
                    // makes the delay line grow without limit.
                    let feedback = (bus.feedback + bus.block.feedback_mod[i]).clamp(-1.0, 1.0);
                    bus.store(
                        bus.block.input[0][i] + feedback * feedback_left,
                        bus.block.input[1][i] + feedback * feedback_right,
                    );
                }
            }
            for (orbit, slot) in reverbs.iter_mut().enumerate() {
                if let Some(reverb) = slot {
                    let (send, block) = (&sends[orbit], &mut blocks[orbit]);
                    let (left, right) = block.split_at_mut(1);
                    reverb.process_block(
                        &send[0][..sub_len],
                        &send[1][..sub_len],
                        &mut left[0][..sub_len],
                        &mut right[0][..sub_len],
                    );
                }
            }
            if !wait_for_dry {
                collect_effect_chains(
                    effects,
                    leaving_effects,
                    effect_mixes,
                    insert_dry,
                    insert_leaving_blocks,
                    insert_blocks,
                    insert_idle_frames,
                    &mut pending,
                );
            }
            // Keep dry instrument outputs after the native send returns.
            while dry_instruments != 0 {
                let bus = dry_instruments.trailing_zeros() as usize;
                dry_instruments &= dry_instruments - 1;
                let block = &instrument_blocks[bus];
                add_insert_output(
                    &mut blocks[insert_outputs[bus]],
                    &block[0][..sub_len],
                    &block[1][..sub_len],
                );
            }
            // Each pooled chain joins in the order its job ends. Its block,
            // its idle frames, its levels and its boxes come back.
            if let Some(jobs) = jobs.as_mut() {
                while pooled_effects != 0 {
                    let bus = jobs.join(JobKind::Effect, pooled_effects);
                    pooled_effects &= !(1u64 << bus);
                    let job = jobs.job(JobKind::Effect, bus);
                    for (output, block) in job.block.iter().zip(&mut insert_blocks[bus]) {
                        block[..sub_len].copy_from_slice(&output[..sub_len]);
                    }
                    for stage in 0..crate::insert::EFFECT_CHAIN {
                        let slot = stage * crate::insert::MAX_INSERT_BUSES + bus;
                        insert_idle_frames[slot] = job.idle[stage];
                        effects[slot] = job.boxes[stage].take();
                        if let Some(leaving) = job.leaving[stage].take() {
                            leaving_effects[slot] = Some(leaving);
                        }
                        effect_mixes[slot] = job.mixes[stage];
                    }
                }
            }
            // Preserve bus order when independent chains join an orbit.
            let mut buses = effect_buses;
            while buses != 0 {
                let bus = buses.trailing_zeros() as usize;
                buses &= buses - 1;
                let (left, right) = insert_blocks[bus].split_at_mut(1);
                let (left, right) = (&mut left[0][..sub_len], &mut right[0][..sub_len]);
                let level = &mut insert_postgains[bus];
                for ((left, right), gain) in left
                    .iter_mut()
                    .zip(right.iter_mut())
                    .zip(&insert_postgain_blocks[bus])
                {
                    let sounds = left.abs() >= INSERT_SILENCE || right.abs() >= INSERT_SILENCE;
                    level.quiet = if sounds {
                        0
                    } else {
                        level.quiet.saturating_add(1)
                    };
                    *left *= *gain;
                    *right *= *gain;
                }
                let first = start + sub_start as u64;
                if first + sub_len as u64 > insert_fade_from[bus] {
                    for (i, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
                        let fade =
                            Self::insert_fade(insert_fade_from[bus], first + i as u64, sample_rate);
                        *left *= fade;
                        *right *= fade;
                    }
                }
                add_insert_output(&mut blocks[insert_outputs[bus]], left, right);
            }
            let orbit_pairs = self.orbit_pairs;
            let orbit_gains = self.orbit_gains.0;
            let orbit_peaks = &mut self.orbit_peaks;
            let orbit_mix = &mut self.orbit_mix;
            let mut stems = self.stems.as_mut();
            for i in 0..sub_len {
                let abs = start + (sub_start + i) as u64;
                let frame = sub_start + i;
                let mut l = piano_mix[0][i];
                let mut r = piano_mix[1][i];
                for (orbit, block) in blocks.iter().enumerate() {
                    let (mut bl, mut br) = (block[0][i], block[1][i]);
                    let djf = &mut djfs[orbit];
                    if djf.active {
                        // The filter keeps processing zeros, so its state
                        // rings out - run it even on silent frames.
                        bl = djf.process(0, bl, sr);
                        br = djf.process(1, br, sr);
                    }
                    if bl == 0.0 && br == 0.0 {
                        continue;
                    }
                    // The whole orbit - dry and sends alike - lands on the
                    // outputs its FIRST voice keyed; see `orbit_channels`.
                    let (bl, br) = Self::route_channels(self.orbit_channels[orbit], bl, br);
                    let gain = ducks[orbit].value_at(abs) * orbit_gains[orbit];
                    let (ol, or) = (bl * gain, br * gain);
                    orbit_peaks[orbit] = orbit_peaks[orbit].max(ol.abs().max(or.abs()));
                    // A stems render taps every orbit here: after its delay,
                    // reverb, DJF, channel map, duck and fader, before the
                    // pair it is routed to decides where it goes. Offline
                    // renders route nothing away, so the taps sum to the
                    // pre-limiter main mix - the only voice the main pair
                    // seeds beyond them is the live keyboard's, which no
                    // score can reach.
                    if let Some(stems) = stems.as_deref_mut() {
                        stems.tap(orbit, frame, ol, or);
                    }
                    // An orbit routed to another pair is kept apart for the
                    // device to place; the main pair sums the rest.
                    if routing && orbit_pairs[orbit] != 0 {
                        let capture_frame = block_offset + frame;
                        if capture_frame < ORBIT_MIX_FRAMES {
                            let at = (orbit * ORBIT_MIX_FRAMES + capture_frame) * 2;
                            orbit_mix[at] = ol;
                            orbit_mix[at + 1] = or;
                        }
                        continue;
                    }
                    l += ol;
                    r += or;
                }
                out[frame * 2] = l;
                out[frame * 2 + 1] = r;
            }
            sub_start += sub_len;
        }

        // Drop finished voices, returning leased state.
        let fm_state_pool = &mut self.fm_state_pool;
        let pool = &mut self.compressor_pool;
        let limiters = &mut self.limiter_pool;
        let delay_pool = &mut self.fx_delay_pool;
        let reverb_pool = &mut self.fx_reverb_pool;
        let zzfx_pool = &mut self.zzfx_ring_pool;
        let stretch_pool = &mut self.stretch_pool;
        let fx_stage_state_pool = &mut self.fx_stage_state_pool;
        let kabel_pool = &mut self.kabel_pool;
        let pressure = &mut self.pressure;
        self.voices.retain_mut(|v| {
            let age = (end.saturating_sub(v.start_frame)) as f32 / sr as f32;
            if age < v.stop_secs && !v.source_gone {
                true
            } else {
                retire_voice_resources(
                    v,
                    pressure,
                    fm_state_pool,
                    pool,
                    limiters,
                    delay_pool,
                    reverb_pool,
                    zzfx_pool,
                    stretch_pool,
                    fx_stage_state_pool,
                    kabel_pool,
                );
                false
            }
        });

        self.finish_insert_fades();
        self.frame = end;
        if self.allow_inline_reverb {
            self.retired_inserts.clear();
        }
        self.reclaim_insert_chains();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod plain_path_tests {
        use super::*;

        /// A stereo sample the test installs, so the voice is a plain
        /// sample voice the way a piano voice is: the bundled `bd` is mono
        /// and would classify away from the tight path.
        const TEST_SAMPLE: crate::sample::SampleId = crate::sample::SampleId(40);

        fn install_stereo_sample(backend: &mut ScalarBackend) {
            let pcm: Vec<f32> = (0..48_000)
                .map(|frame| {
                    let phase = frame as f32 * 440.0 * 2.0 * std::f32::consts::PI / 48_000.0;
                    (phase.sin() * (1.0 - frame as f32 / 48_000.0)) * 0.4
                })
                .collect();
            let decoded = crate::sample::DecodedSample::from_parts(48_000, 2, pcm).unwrap();
            backend
                .install_sample(TEST_SAMPLE, Box::new(decoded))
                .expect("the test slot installs");
        }

        fn sample_note(frame: u64, rate: f32) -> OnsetEvent {
            let mut event = OnsetEvent::new(frame, 220.0, 0.5, 2.0);
            event.sample = Some(crate::SampleControls {
                sample: TEST_SAMPLE,
                playback_rate: rate,
                begin: 0.0,
                end: 1.0,
                hold: crate::sample::SampleHold::Hap,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            });
            event
        }

        fn render(disable_plain: bool) -> (Vec<f32>, usize) {
            let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
            backend.disable_plain_path = disable_plain;
            install_stereo_sample(&mut backend);
            for (frame, rate) in [(0u64, 1.0), (200, 0.5), (1024, 1.5)] {
                backend.note(sample_note(frame, rate));
            }
            let mut out = vec![0.0f32; 2 * 128];
            let mut all = Vec::new();
            for _ in 0..48 {
                backend.process_block(&mut out[..], 128);
                all.extend_from_slice(&out);
            }
            let plain_voices = backend.voices.iter().filter(|voice| voice.plain).count();
            (all, plain_voices)
        }

        /// The tight path computes the same frame the general body does,
        /// bit for bit, for every plain sample voice.
        #[test]
        fn the_plain_sample_path_matches_the_general_body_bit_for_bit() {
            let (tight, plain_voices) = render(false);
            let (general, _) = render(true);
            assert!(
                plain_voices > 0,
                "the test's voices must classify as plain or it proves nothing"
            );
            assert!(
                tight.iter().any(|value| value.abs() > 0.001),
                "the test's samples must sound or the comparison proves nothing"
            );
            assert_eq!(tight.len(), general.len());
            for (tight, general) in tight.iter().zip(&general) {
                assert_eq!(tight.to_bits(), general.to_bits());
            }
        }

        /// A plain sample voice classifies for the tight path.
        #[test]
        fn a_plain_sample_voice_classifies_for_the_tight_path() {
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            install_stereo_sample(&mut backend);
            backend.note(sample_note(0, 1.0));
            let mut out = [0.0f32; 256];
            backend.process_block(&mut out[..], 128);
            assert!(backend.voices.iter().any(|voice| voice.plain));
        }
    }

    mod dispatch_tests {
        use super::*;

        #[test]
        fn external_fx_reverbs_adopt_the_receiving_backend_dispatch() {
            let mut backend = ScalarBackend::with_dispatch(DspDispatch::portable());
            let reverb = crate::reverb::OrbitReverb::generate_streaming(
                24_000,
                crate::reverb::ReverbParams {
                    ir: None,
                    size_secs: 0.2,
                    fade_secs: 0.01,
                    lp_start_hz: 8_000.0,
                    lp_end_hz: 1_000.0,
                },
            );
            assert!(!reverb.dispatch().is_forced_portable());
            assert!(backend.install_fx_reverb(Box::new(reverb)).is_none());
            assert!(backend.fx_reverb_pool[0].dispatch().is_forced_portable());
        }
    }
}
