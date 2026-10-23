use super::*;

impl<K> From<OscillatorControls<K>> for VoiceRenderControls {
    fn from(controls: OscillatorControls<K>) -> Self {
        let OscillatorControls {
            kabel: _,
            live_controls: _,
            preview_epoch: _,
            choke_only: _,
            piano: _,
            limit: _,
            noise,
            waveform,
            envelope,
            worklet_begin_secs,
            lfo_end_secs,
            filter_lfo_end_secs,
            modulator_release_secs,
            velocity: _,
            postgain,
            pan: _,
            filters: _,
            distort,
            delay: _,
            duck: _,
            reverb: _,
            dry: _,
            stretch,
            fm,
            orbit: _,
            insert_chain: _,
            effects: _,
            instrument: _,
            lfos,
            bus_mods,
            bus,
            busgain,
            channels: _,
            envs,
            phaser,
            tremolo,
            vowel,
            coarse,
            crush,
            shape,
            vibrato,
            pitch_env,
            djf: _,
            compressor,
            transient: _,
            fx_stages: _,
            partials,
        } = controls;
        Self {
            noise,
            waveform,
            envelope,
            worklet_begin_secs,
            lfo_end_secs,
            filter_lfo_end_secs,
            modulator_release_secs,
            postgain,
            distort,
            stretch,
            fm,
            lfos,
            bus_mods,
            bus,
            busgain,
            envs,
            phaser,
            tremolo,
            vowel,
            coarse,
            crush,
            shape,
            vibrato,
            pitch_env,
            compressor,
            partials,
        }
    }
}

impl Voice {
    /// Whether this voice TAKES from a bus, either as its source or as a
    /// modulation input. Used only to order receivers after senders.
    pub(super) fn reads_a_bus(&self) -> bool {
        matches!(self.source, VoiceSource::Bus { .. })
            || self.controls.bus_mods.iter().any(Option::is_some)
    }
}

/// Move every heap-backed lease home before `Voice` is dropped; both ordinary
/// retirement and a stop/reset run inside the audio callback.
#[allow(clippy::too_many_arguments, clippy::vec_box)]
pub(super) fn retire_voice_resources(
    voice: &mut Voice,
    pressure: &mut ScalarPressureState,
    fm_state_pool: &mut Vec<Box<FmState>>,
    compressor_pool: &mut Vec<Box<CompressorState>>,
    limiter_pool: &mut Vec<Box<crate::limiter::InlineLimiter>>,
    delay_pool: &mut Vec<FxDelayLine>,
    reverb_pool: &mut Vec<Box<crate::reverb::OrbitReverb>>,
    zzfx_pool: &mut Vec<Box<[f32]>>,
    stretch_pool: &mut Vec<Box<[crate::stretch::Stretch; 2]>>,
    fx_stage_state_pool: &mut Vec<Box<FxStageStates>>,
    kabel_pool: &mut Vec<Box<rustel_kabel::Graph>>,
) {
    pressure.retire(voice);
    if let Some(kabel) = voice.kabel.take() {
        kabel_pool.push(kabel.graph);
    }
    if let Some(state) = voice.fm_state.take() {
        fm_state_pool.push(state);
    }
    if let Some(state) = voice.compressor.take() {
        compressor_pool.push(state);
    }
    if let Some(limiter) = voice.limiter.take() {
        limiter_pool.push(limiter);
    }
    if let Some(mut stretch) = voice.stretch.take() {
        stretch[0].reset();
        stretch[1].reset();
        stretch_pool.push(stretch);
    }
    if let VoiceSource::ZzFx { ring, .. } = &mut voice.source
        && let Some(mut ring) = ring.take()
    {
        ring.fill(0.0);
        zzfx_pool.push(ring);
    }
    if let Some(mut stages) = voice.fx_stage_state.take() {
        for slot in stages.iter_mut() {
            if let Some(stage) = slot {
                if let Some(kabel) = stage.kabel.take() {
                    kabel_pool.push(kabel.graph);
                }
                if let Some(state) = stage.compressor.take() {
                    compressor_pool.push(state);
                }
                if let Some(line) = stage.delay.take() {
                    delay_pool.push(line);
                }
                if let Some(reverb) = stage.room.take() {
                    reverb_pool.push(reverb);
                }
                if let Some(mut stretch) = stage.stretch.take() {
                    stretch[0].reset();
                    stretch[1].reset();
                    stretch_pool.push(stretch);
                }
            }
            *slot = None;
        }
        fx_stage_state_pool.push(stages);
    }
}

/// Whether this quantum starts before the note's hold plus release ends.
/// Uses the hap duration, not the sample slice length: a sample can keep
/// sounding after its modulators stop. `onset_lead` preserves the fractional
/// frame offset lost when the onset was rounded to `start_frame`.
pub(super) fn note_end_alive(
    quantum_start: u64,
    start_frame: u64,
    onset_lead: f32,
    hap_duration_secs: f32,
    release_secs: f32,
    sample_rate: f64,
) -> bool {
    let life = f64::from(hap_duration_secs) + f64::from(release_secs);
    (quantum_start as f64) < start_frame as f64 - f64::from(onset_lead) + life * sample_rate
}

/// Where a nudged source starts, as whole frames of silence plus the
/// sub-sample lead left over.
///
/// The nudge and `onset_lead` are floats, so the start can fall between output
/// frames. `ceil` gives the first output frame at or after the start, which is
/// the contract of `onset_lead`. The residue is how far the source has
/// advanced by then. A negative nudge uses the same expression: the delay is
/// zero and the whole offset becomes lead.
///
/// A truncation gives a delay one frame short. `nudge(0.02)` is 959.9999785
/// frames at 48 kHz, because the f32 is just under 0.02. With no `onset_lead`,
/// the delay is 960 frames, not 959.
pub(super) fn nudged_start(nudge_secs: f32, onset_lead: f32, sample_rate: f64) -> (u32, f64) {
    let start_offset = f64::from(nudge_secs) * sample_rate - f64::from(onset_lead);
    let delay = start_offset.ceil().max(0.0);
    (delay as u32, delay - start_offset)
}

/// Where `begin()` starts reading, in frames.
///
/// A grain offset resolves to a whole frame, so a buffer source at playback
/// rate 1 reads samples directly. A fractional offset makes every read
/// interpolate from the first sample, which acts as a low-pass filter.
///
/// A resampled buffer can have a fractional offset. A 44.1 kHz file converted
/// to 48 kHz has a frame count that can put `begin()` between two samples.
pub(super) fn begin_frame(begin: f32, source_frames: f64, playback_rate: f64) -> f64 {
    let frames = f64::from(begin) * source_frames;
    // At rate 1 a buffer source snaps its grain offset to a whole frame so
    // playback stays sample-exact; at any other rate it keeps the fraction
    // and interpolates from it.
    if playback_rate == 1.0 {
        frames.round()
    } else {
        frames
    }
}

/// One sample of an oscillator voice at `phase`, which then advances by the
/// frequency the voice plays, `(freq_hz + fm_offset_hz) * pitch_ratio`, and
/// wraps into one period.
///
/// While FM or the pitch ratio moves that frequency, each sample reads the
/// band-limited table for it. As in Firefox's OscillatorNode, the frequency
/// is not clamped past Nyquist (Chromium clamps it to ±Nyquist): the table
/// for a frequency past Nyquist has no partials, and a sine aliases. A steady
/// frequency reads `table_selection`, the table chosen at the note's start.
#[allow(clippy::too_many_arguments)]
#[inline]
pub(super) fn oscillator_sample(
    tables: &PeriodicWaveTables,
    controls: &VoiceRenderControls,
    table_selection: TableSelection,
    phase: &mut f64,
    partial_limit: &mut usize,
    freq_hz: f32,
    fm_offset_hz: f32,
    pitch_ratio: f32,
    sr: f64,
) -> f32 {
    let frequency = (freq_hz + fm_offset_hz) * pitch_ratio;
    let moving = fm_offset_hz != 0.0 || pitch_ratio != 1.0;
    let wave = match controls.partials.map(|partials| partials.wave) {
        // A note whose wave no lease holds plays silence.
        Some(None) => 0.0,
        Some(Some(wave)) => {
            let wave = wave.wave();
            let selection = if moving {
                wave.selection(frequency, sr as f32)
            } else {
                table_selection
            };
            *partial_limit = (*partial_limit).max(wave.partial_limit(frequency, sr as f32));
            wave.sample(*phase, selection) * wave.norm(*partial_limit)
        }
        None if moving => tables.sample_for_frequency(controls.waveform, *phase, frequency),
        None => tables.sample(controls.waveform, *phase, table_selection),
    };
    *phase += f64::from(frequency) / sr;
    *phase -= phase.floor();
    wave
}

impl PreparedSupersaw {
    pub(super) fn new(voices: f32, freqspread: f32, panspread: f32) -> Self {
        let voices = voices.max(1.0);
        let count = (voices.ceil() as usize).clamp(1, MAX_UNISON);
        let (scale, center) = supersaw_spread_geometry(voices, freqspread.max(0.0));
        let mut detune_ratios = [1.0; MAX_UNISON];
        for (lane, ratio) in detune_ratios.iter_mut().enumerate().take(count) {
            *ratio = supersaw_detune_ratio(lane, scale, center);
        }
        Self {
            voices,
            count,
            freqspread,
            panspread,
            detune_ratios,
            pan_gains: supersaw_pan_gains(panspread),
        }
    }

    #[inline]
    pub(super) fn pan_gains(&self, addition: f32) -> (f32, f32) {
        if addition.to_bits() == 0 {
            self.pan_gains
        } else {
            // Outside -1..1 a gain is the square root of a negative value.
            supersaw_pan_gains((self.panspread + addition).clamp(-1.0, 1.0))
        }
    }
}

/// How many of the oldest sounds are killed before a new one is admitted.
///
/// The kill loop re-reads its own condition as the pool shrinks, so it does
/// not clamp straight back to the cap - 130 active sounds lose two and 131
/// also lose two, converging over the onsets that follow rather than all at
/// once.
fn polyphony_kills(active: usize, max_polyphony: usize) -> usize {
    let mut remaining = active as i64;
    let mut killed = 0i64;
    while killed <= remaining - max_polyphony as i64 {
        remaining -= 1;
        killed += 1;
    }
    killed as usize
}

/// Advance a voice's FM modulators by `steps` samples, with no envelope and
/// no per-voice modulation.
///
/// The two things left out are the two that would make a trajectory belong
/// to one note rather than to the configuration, and neither has happened
/// yet at the point this is used: an operator's envelope is scheduled AT
/// the note's start, so the gain it rides still holds the 1 it was
/// constructed with, and a modulator aimed at an FM parameter has the same
/// begin gate and has written nothing.
///
/// Three passes, needing no topological order: read every operator's output
/// at this sample, sum what each target receives, and only then advance.
pub(super) fn fm_advance(
    fm: crate::backend::FmControls,
    freq_hz: f32,
    phases: &mut [f64; crate::backend::MAX_FM_OPERATORS + 1],
    noise: &mut [NoiseGen; crate::backend::MAX_FM_OPERATORS + 1],
    steps: u32,
    tables: &PeriodicWaveTables,
    sample_rate: u32,
) {
    for _ in 0..steps {
        let mut signal = [0.0f32; crate::backend::MAX_FM_OPERATORS + 1];
        for (slot, operator) in fm.operators.iter().enumerate() {
            let Some(op) = operator else {
                continue;
            };
            let base = freq_hz * op.harmonicity;
            signal[slot + 1] = match op.waveform.oscillator() {
                Some(waveform) => tables.sample_for_frequency(waveform, phases[slot], base),
                None => {
                    let crate::backend::FmWave::Noise(kind) = op.waveform else {
                        unreachable!("only a noise shape has no oscillator")
                    };
                    noise[slot].next(kind, 0.0)
                }
            };
        }
        let mut incoming = [0.0f32; crate::backend::MAX_FM_OPERATORS + 1];
        for route in fm.routes.iter().flatten() {
            let Some(Some(op)) = fm.operators.get(usize::from(route.source) - 1) else {
                continue;
            };
            incoming[usize::from(route.target)] +=
                signal[usize::from(route.source)] * route.amount * freq_hz * op.harmonicity;
        }
        for (slot, operator) in fm.operators.iter().enumerate() {
            let Some(op) = operator else {
                continue;
            };
            if !op.waveform.accepts_modulation() {
                continue;
            }
            let base = freq_hz * op.harmonicity;
            let phase = &mut phases[slot];
            *phase += f64::from(base + incoming[slot + 1]) / f64::from(sample_rate);
            *phase -= phase.floor();
        }
    }
}

impl NoiseGen {
    /// Buffer length is `2 * sampleRate`, and the seed is fixed so renders
    /// are reproducible.
    fn new(sample_rate: u32) -> Self {
        Self {
            rng: 0x5EED_C0DE_u64 | 1,
            pink: [0.0; 7],
            brown_last: 0.0,
            index: 0,
            len: sample_rate.saturating_mul(2).max(1),
            frac: 0.0,
            pending: None,
        }
    }

    /// A noise source started at the note's own time, which can land
    /// between output frames. See `frac`.
    pub(super) fn at_onset(sample_rate: u32, onset_lead: f32) -> Self {
        Self {
            frac: if onset_lead.is_finite() {
                onset_lead.clamp(0.0, 1.0)
            } else {
                0.0
            },
            ..Self::new(sample_rate)
        }
    }

    /// One output sample, read from the buffer at `frac` past `index`.
    pub(super) fn next(&mut self, kind: u8, density: f32) -> f32 {
        let current = match self.pending.take() {
            Some(sample) => sample,
            None => self.raw(kind, density),
        };
        let next = self.raw(kind, density);
        self.pending = Some(next);
        // `frac` is 0 for every frame-aligned source, which returns `current`
        // unchanged and leaves the sequence exactly as it was.
        current + (next - current) * self.frac
    }

    /// The buffer's own contents - the per-type noise recurrences, before
    /// any playback interpolation.
    fn raw(&mut self, kind: u8, density: f32) -> f32 {
        if self.index >= self.len {
            // Only the generator restarts; the read offset belongs to the
            // playback and survives the loop, as does the lookahead.
            *self = Self {
                len: self.len,
                frac: self.frac,
                pending: self.pending,
                ..Self::new(self.len / 2)
            };
        }
        self.index += 1;
        // crackle draws its probability FIRST and only reaches for a value on
        // a hit, so it is handled before the shared white draw below.
        if kind == 3 {
            let roll = (noise_sample_u64(&mut self.rng) + 1.0) * 0.5;
            return if roll < density * 0.01 {
                noise_sample_u64(&mut self.rng)
            } else {
                0.0
            };
        }
        let white = noise_sample_u64(&mut self.rng);
        match kind {
            // brown: a leaky integrator over white.
            2 => {
                self.brown_last = (self.brown_last + 0.02 * white) / 1.02;
                self.brown_last
            }
            // pink: Paul Kellett's filter bank, verbatim.
            1 => {
                let b = &mut self.pink;
                b[0] = 0.99886 * b[0] + white * 0.0555179;
                b[1] = 0.99332 * b[1] + white * 0.0750759;
                b[2] = 0.969 * b[2] + white * 0.153852;
                b[3] = 0.8665 * b[3] + white * 0.3104856;
                b[4] = 0.55 * b[4] + white * 0.5329522;
                b[5] = -0.7616 * b[5] - white * 0.016898;
                let out = (b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + white * 0.5362) * 0.11;
                b[6] = white * 0.115926;
                out
            }
            _ => white,
        }
    }
}

impl FmState {
    fn new(sample_rate: u32) -> Self {
        Self {
            phases: [0.0; crate::backend::MAX_FM_OPERATORS + 1],
            quantum_skip: 0,
            noise: [NoiseGen::new(sample_rate); crate::backend::MAX_FM_OPERATORS + 1],
        }
    }

    pub(super) fn restart(&mut self, onset_frame: u64, sample_rate: u32) {
        self.phases.fill(0.0);
        self.quantum_skip = (onset_frame % 128) as u8;
        self.noise.fill(NoiseGen::new(sample_rate));
    }
}

/// SBD noise-gain automation: start at 1.2, ramp exponentially to 0.001 over
/// 25 ms, then hold that endpoint until the voice stops.
pub(super) fn sbd_noise_gain(t: f32) -> f32 {
    const START: f32 = 1.2;
    const END: f32 = 0.001;
    const DECAY: f32 = 0.025;
    if t >= DECAY {
        END
    } else {
        START * (END / START).powf((t / DECAY).max(0.0))
    }
}

pub(super) fn sbd_graph_lead_frames(sample_rate: u32) -> u64 {
    (f64::from(sample_rate) * SBD_GRAPH_LEAD_SECS).round() as u64
}

fn kept_onset_reach_frames(sample_rate: u32) -> u64 {
    (f64::from(sample_rate) * KEPT_ONSET_REACH_SECS).round() as u64
}

/// The sample-rate-length transfer curve used by SBD's WaveShaper.
pub(super) fn sbd_saturation_curve(sample_rate: u32) -> Box<[f32]> {
    let curve_len = sample_rate.max(2) as usize;
    (0..curve_len)
        .map(|index| {
            let x = index as f64 * 2.0 / curve_len as f64 - 1.0;
            (x * 2.0).tanh() as f32
        })
        .collect()
}

/// Linearly sample SBD's precomputed WaveShaper curve instead of evaluating
/// `tanh(2x)` directly. The curve is prepared with the backend, outside the
/// audio callback.
pub(super) fn sbd_saturation(input: f32, curve: &[f32]) -> f32 {
    let curve_len = curve.len();
    let last = curve_len - 1;
    let virtual_index = ((input + 1.0) * 0.5 * last as f32).clamp(0.0, last as f32);
    let index = virtual_index.floor() as usize;
    let current = curve[index];
    if index == last {
        current
    } else {
        let next = curve[index + 1];
        current + (next - current) * (virtual_index - index as f32)
    }
}

/// The phase step `cycles_per_sample.rem_euclid(1.0)`. An audible step is
/// already in `0..1`, where the remainder is the step itself, so it skips
/// the library call.
#[inline]
pub(crate) fn saw_step(cycles_per_sample: f32) -> f32 {
    if (0.0..1.0).contains(&cycles_per_sample) {
        cycles_per_sample
    } else {
        cycles_per_sample.rem_euclid(1.0)
    }
}

/// Polyblep-corrected saw.
pub(crate) fn saw_blep(phase: f32, mut dt: f32) -> f32 {
    let v = 2.0 * phase - 1.0;
    dt = dt.min(1.0 - dt);
    if dt <= 0.0 {
        return v;
    }
    let invdt = 1.0 / dt;
    let blep = if phase < dt {
        let p = phase * invdt;
        2.0 * p - p * p - 1.0
    } else if phase > 1.0 - dt {
        let p = (phase - 1.0) * invdt;
        p * p + 2.0 * p + 1.0
    } else {
        0.0
    };
    v - blep
}

#[inline]
pub(super) fn supersaw_spread_geometry(voices: f32, spread: f32) -> (f32, f32) {
    if voices < 2.0 {
        (0.0, 0.0)
    } else {
        (spread / (voices - 1.0), spread * 0.5)
    }
}

#[inline]
pub(super) fn supersaw_detune_ratio(lane: usize, scale: f32, center: f32) -> f32 {
    let detune = lane as f32 * scale - center;
    2.0f32.powf(detune / 12.0)
}

#[inline]
fn supersaw_pan_gains(panspread: f32) -> (f32, f32) {
    let panspread01 = panspread * 0.5 + 0.5;
    ((1.0 - panspread01).sqrt(), panspread01.sqrt())
}

impl Default for PolyphonyLimit {
    fn default() -> Self {
        Self(MAX_POLYPHONY)
    }
}

/// White noise on a per-voice splitmix64 stream - seeded, like the unison
/// phases, so renders are reproducible.
fn noise_sample_u64(state: &mut u64) -> f32 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58476D1CE4E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    ((z >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// Deterministic per-voice initial phase for `phaserand`: a hash of onset
/// id and voice index keeps renders reproducible.
pub(super) fn seeded_phase(onset_id: u64, voice: usize) -> f32 {
    // Full splitmix64 finalizer: the single-round mix used first left
    // consecutive voice indices nearly collinear in the top bits, which
    // collapsed the unison field to mono (probed: phases 0.997, 0.995, …).
    let mut z = (onset_id ^ (voice as u64).wrapping_mul(0x9E3779B97F4A7C15))
        .wrapping_add(0x9E3779B97F4A7C15u64.wrapping_mul(voice as u64 + 1));
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58476D1CE4E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}

impl PendingEvents {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            order: Vec::with_capacity(capacity),
            ..Self::default()
        }
    }

    pub(super) fn len(&self) -> usize {
        self.order.len()
    }

    fn capacity(&self) -> usize {
        self.slots.capacity().min(self.order.capacity())
    }

    pub(super) fn push(&mut self, event: OnsetEvent<CompactKabelControls>) {
        self.push_with_confirmation(event, None, None);
    }

    fn push_with_confirmation(
        &mut self,
        event: OnsetEvent<CompactKabelControls>,
        confirmation: Option<crate::confirmation::ConfirmationOnset>,
        expected_sample_identity: Option<u64>,
    ) {
        let index = match self.free {
            Some(index) => {
                self.free = self.slots[index].next_free;
                self.slots[index] = PendingEventSlot {
                    event: Some(event),
                    confirmation,
                    expected_sample_identity,
                    next_free: None,
                };
                index
            }
            None => {
                let index = self.slots.len();
                self.slots.push(PendingEventSlot {
                    event: Some(event),
                    confirmation,
                    expected_sample_identity,
                    next_free: None,
                });
                index
            }
        };
        self.order.push(index);
    }

    /// Visit in insertion order and recycle rejected slots without moving any
    /// surviving event. Reused slots are always appended to `order`, so
    /// simultaneous onsets retain trigger order even after many removals.
    #[cfg(test)]
    fn retain(&mut self, mut keep: impl FnMut(&OnsetEvent<CompactKabelControls>) -> bool) {
        self.retain_with_confirmation(|event, _, _| keep(event));
    }

    pub(super) fn retain_with_confirmation(
        &mut self,
        mut keep: impl FnMut(
            &OnsetEvent<CompactKabelControls>,
            Option<crate::confirmation::ConfirmationOnset>,
            Option<u64>,
        ) -> bool,
    ) {
        let mut write = 0;
        for read in 0..self.order.len() {
            let index = self.order[read];
            let keep_event = keep(
                self.slots[index]
                    .event
                    .as_ref()
                    .expect("pending order may only contain occupied slots"),
                self.slots[index].confirmation,
                self.slots[index].expected_sample_identity,
            );
            if keep_event {
                self.order[write] = index;
                write += 1;
            } else {
                self.slots[index].event = None;
                self.slots[index].confirmation = None;
                self.slots[index].expected_sample_identity = None;
                self.slots[index].next_free = self.free;
                self.free = Some(index);
            }
        }
        self.order.truncate(write);
    }

    pub(super) fn clear(&mut self) {
        self.slots.clear();
        self.order.clear();
        self.free = None;
    }
}

impl ScalarBackend {
    pub(super) fn ensure_fm_state_pool(&mut self, capacity: usize) {
        let leased = self
            .voices
            .iter()
            .filter(|voice| voice.fm_state.is_some())
            .count();
        let target = capacity.min(MAX_ACTIVE_VOICES);
        let available = self.fm_state_pool.len().saturating_add(leased);
        let sample_rate = self.sample_rate;
        self.fm_state_pool
            .extend((available..target).map(|_| Box::new(FmState::new(sample_rate))));
    }

    /// One stereo frame of the wavetable oscillator.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn wavetable_sample(
        bank: &SampleBank,
        controls: &crate::backend::WavetableControls,
        phases: &mut [f64; MAX_UNISON],
        lfo_phase: &mut Option<f64>,
        warp_lfo_phase: &mut Option<f64>,
        onset_secs: f64,
        freq_hz: f32,
        t: f32,
        duration_secs: f32,
        sample_rate: f32,
        adds: WavetableAdds,
        wavetable_kernel: Option<WavetableKernel>,
    ) -> (f32, f32) {
        let Some(decoded) = bank.get(controls.table) else {
            return (0.0, 0.0);
        };
        let frame_len = (controls.frame_len as usize).max(1);
        let num_frames = (decoded.frames() / frame_len).max(1);
        let pcm = decoded.pcm();

        // Position = linear ADSR from the base (min) to base+amount, plus
        // the absolute-time-locked LFO, clamped 0..1 in the processor.
        let mut position = controls.position;
        if controls.pos_env_amount != 0.0 {
            let env = crate::backend::Envelope {
                attack_secs: controls.pos_attack,
                decay_secs: controls.pos_decay,
                sustain: controls.pos_sustain,
                release_secs: controls.pos_release,
            };
            let gate = Self::gate_value(duration_secs, env);
            position += controls.pos_env_amount * Self::envelope(t, duration_secs, env, gate);
        }
        if controls.lfo_depth != 0.0 {
            // The LFO's own params are block-latched; `position` is a-rate.
            let rate = controls.lfo_rate + adds.lfo_rate;
            let depth = controls.lfo_depth + adds.lfo_depth;
            let skew = controls.lfo_skew + adds.lfo_skew;
            let phase =
                *lfo_phase.get_or_insert_with(|| (onset_secs * f64::from(rate)).rem_euclid(1.0));
            let raw = (lfo_shape(controls.lfo_shape, phase as f32, skew) + controls.lfo_dc) * depth;
            // `min`/`max` were computed from the depth the LFO was BUILT
            // with, and nothing modulates them - so a modulated depth
            // widens `raw` while the bounds stay where they were.
            let lo = controls.lfo_dc * controls.lfo_depth;
            position += raw.clamp(
                lo.min(lo + controls.lfo_depth),
                lo.max(lo + controls.lfo_depth),
            );
            let next = phase + f64::from(rate) / f64::from(sample_rate);
            *lfo_phase = Some(if next > 1.0 { next - 1.0 } else { next });
        }
        position += adds.position;
        let position = position.clamp(0.0, 1.0);

        // Warp rides an envelope + LFO pair identical to position's, and
        // the result clamps 0..1 before use.
        let mode = crate::warp::WarpMode::from_index(i32::from(controls.warp_mode));
        let mut warp = controls.warp;
        if controls.warp_env_amount != 0.0 {
            let env = crate::backend::Envelope {
                attack_secs: controls.warp_attack,
                decay_secs: controls.warp_decay,
                sustain: controls.warp_sustain,
                release_secs: controls.warp_release,
            };
            let gate = Self::gate_value(duration_secs, env);
            warp += controls.warp_env_amount * Self::envelope(t, duration_secs, env, gate);
        }
        if controls.warp_lfo_depth != 0.0 {
            let rate = controls.warp_lfo_rate + adds.warp_lfo_rate;
            let depth = controls.warp_lfo_depth + adds.warp_lfo_depth;
            let skew = controls.warp_lfo_skew + adds.warp_lfo_skew;
            let phase = *warp_lfo_phase
                .get_or_insert_with(|| (onset_secs * f64::from(rate)).rem_euclid(1.0));
            let raw = (lfo_shape(controls.warp_lfo_shape, phase as f32, skew)
                + controls.warp_lfo_dc)
                * depth;
            let lo = controls.warp_lfo_dc * controls.warp_lfo_depth;
            warp += raw.clamp(
                lo.min(lo + controls.warp_lfo_depth),
                lo.max(lo + controls.warp_lfo_depth),
            );
            let next = phase + f64::from(rate) / f64::from(sample_rate);
            *warp_lfo_phase = Some(if next > 1.0 { next - 1.0 } else { next });
        }
        let warp = (warp + adds.warp).clamp(0.0, 1.0);

        let idx = position * (num_frames - 1) as f32;
        let frame_index = (idx as usize).min(num_frames - 1);
        let interp = idx - frame_index as f32;
        let frame_b = (frame_index + 1).min(num_frames - 1);

        let sample_frame = |frame: usize, phase: f32| -> f32 {
            let base = frame * frame_len;
            let pos = phase * frame_len as f32;
            // Derive the fraction before wrapping: warped phase can equal 1.
            let mut i = pos as usize;
            let frac = pos - i as f32;
            if i >= frame_len {
                i = 0;
            }
            let a = pcm.get(base + i).copied().unwrap_or(0.0);
            let mut i1 = i + 1;
            if i1 >= frame_len {
                i1 = 0;
            }
            let b = pcm.get(base + i1).copied().unwrap_or(0.0);
            a + (b - a) * frac
        };

        let voices_raw = controls.voices.max(1.0);
        let voices = (voices_raw.ceil() as usize).clamp(1, MAX_UNISON);
        let panspread = if voices_raw > 1.0 {
            (controls.panspread + adds.panspread).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let gain1 = (0.5 - 0.5 * panspread).sqrt();
        let gain2 = (0.5 + 0.5 * panspread).sqrt();
        let normalizer = 1.0 / (voices as f32).sqrt();
        let spread = (controls.freqspread + adds.freqspread).max(0.0);
        // getDetuner(unison, detune): raw-float division, `< 2` on the raw.
        let (scale, center) = if voices_raw < 2.0 {
            (0.0, 0.0)
        } else {
            (spread / (voices_raw - 1.0), spread * 0.5)
        };
        let mut left = 0.0f32;
        let mut right = 0.0f32;
        if let Some(kernel) = wavetable_kernel
            && let Some(sample) = Self::accelerated_wavetable_sample(
                kernel,
                pcm,
                frame_len,
                frame_index,
                frame_b,
                interp,
                phases,
                voices,
                gain1,
                gain2,
                normalizer,
                scale,
                center,
                freq_hz,
                sample_rate,
                warp,
                mode,
            )
        {
            return sample;
        }
        // `voices` is clamped to MAX_UNISON above, which is `phases.len()`, so
        // the take never shortens the loop.
        for (n, phase) in phases.iter_mut().enumerate().take(voices) {
            let (gain_l, gain_r) = if n & 1 == 1 {
                (gain2, gain1)
            } else {
                (gain1, gain2)
            };
            let detune = f64::from(n as f32 * scale - center);
            let voice_freq = f64::from(freq_hz) * 2.0f64.powf(detune / 12.0);
            let d_phase = voice_freq / f64::from(sample_rate);
            let raw_phase = *phase;
            let ph = crate::warp::warp_phase(raw_phase as f32, warp, mode);
            let s0 = sample_frame(frame_index, ph);
            let s1 = sample_frame(frame_b, ph);
            let mut value = s0 + (s1 - s0) * interp;
            // FLIP is not a phase transform: it inverts the SAMPLE over the
            // first `warp` of the cycle, and it tests the raw phase.
            if mode.flips_sample(raw_phase as f32, warp) {
                value = -value;
            }
            left += value * gain_l * normalizer;
            right += value * gain_r * normalizer;
            // The RAW phase is what advances. Warping is a read transform on
            // the way into the table, not a change to the oscillator's rate -
            // feeding the warped value back here would make the pitch depend
            // on the warp mode.
            *phase = (raw_phase + d_phase).fract();
        }
        (left, right)
    }

    /// Keep architecture-specific table work out of the already large source
    /// dispatcher. This boundary also leaves the original scalar loop compact
    /// and directly available for unsupported tables and CPUs.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn accelerated_wavetable_sample(
        kernel: WavetableKernel,
        pcm: &[f32],
        frame_len: usize,
        frame_index: usize,
        frame_b: usize,
        interp: f32,
        phases: &mut [f64; MAX_UNISON],
        voices: usize,
        gain1: f32,
        gain2: f32,
        normalizer: f32,
        scale: f32,
        center: f32,
        freq_hz: f32,
        sample_rate: f32,
        warp: f32,
        mode: crate::warp::WarpMode,
    ) -> Option<(f32, f32)> {
        if voices < 8 {
            return None;
        }

        let mut warped_phases = [0.0f32; MAX_UNISON];
        let mut phase_steps = [0.0f64; MAX_UNISON];
        let mut flips = [false; MAX_UNISON];
        for n in 0..voices {
            let detune = f64::from(n as f32 * scale - center);
            let voice_freq = f64::from(freq_hz) * 2.0f64.powf(detune / 12.0);
            phase_steps[n] = voice_freq / f64::from(sample_rate);
            let raw_phase = phases[n];
            warped_phases[n] = crate::warp::warp_phase(raw_phase as f32, warp, mode);
            flips[n] = mode.flips_sample(raw_phase as f32, warp);
        }

        let mut values = [0.0f32; MAX_UNISON];
        if !kernel.try_interpolate(
            pcm,
            frame_len,
            frame_index,
            frame_b,
            interp,
            &warped_phases,
            &mut values,
            voices,
        ) {
            return None;
        }

        let mut left = 0.0f32;
        let mut right = 0.0f32;
        for n in 0..voices {
            let (gain_l, gain_r) = if n & 1 == 1 {
                (gain2, gain1)
            } else {
                (gain1, gain2)
            };
            let mut value = values[n];
            if flips[n] {
                value = -value;
            }
            left += value * gain_l * normalizer;
            right += value * gain_r * normalizer;
            phases[n] = (phases[n] + phase_steps[n]).fract();
        }
        Some((left, right))
    }

    /// Submit only if the preallocated real-time capacity is sufficient.
    /// Returns false rather than allocating inside the callback.
    pub fn try_note_prepared(&mut self, event: OnsetEvent<CompactKabelControls>) -> bool {
        self.try_note_prepared_with_confirmation(event, None, None)
    }

    pub(super) fn queue_choke<K>(&mut self, event: OnsetEvent<K>) -> bool {
        let Some(group) = event.cut else {
            return true;
        };
        let group = group.to_bits();
        if let Some(slot) = self
            .pending_chokes
            .iter_mut()
            .flatten()
            .find(|(_, key)| *key == group)
        {
            slot.0 = slot.0.max(event.onset_frame);
            return true;
        }
        if let Some(slot) = self.pending_chokes.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some((event.onset_frame, group));
            return true;
        }
        false
    }

    pub(crate) fn try_note_prepared_with_confirmation(
        &mut self,
        event: OnsetEvent<CompactKabelControls>,
        confirmation: Option<crate::confirmation::ConfirmationOnset>,
        expected_sample_identity: Option<u64>,
    ) -> bool {
        if event.controls.choke_only {
            return self.queue_choke(event);
        }
        // The LIVE consumer arms the sidechain at ring intake (arm_duck_at),
        // the first time it SEES the event - a schedule lead before the
        // onset. Admission happens only in the onset's own block, far too
        // late to start the 10 ms pre-hold ramp.
        let required = self
            .voices
            .len()
            .saturating_add(self.pending.len())
            .saturating_add(1);
        if self.pending.len() == self.pending.capacity() {
            self.pressure.pending_ceiling_drops =
                self.pressure.pending_ceiling_drops.saturating_add(1);
            #[cfg(feature = "device-audio")]
            if let (Some(consumer), Some(tag)) = (&mut self.confirmations, confirmation) {
                consumer.admission(tag, event.onset_frame, event.generation, false);
            }
            return false;
        }
        if required > self.voices.capacity() {
            self.pressure.voice_ceiling_drops = self.pressure.voice_ceiling_drops.saturating_add(1);
            #[cfg(feature = "device-audio")]
            if let (Some(consumer), Some(tag)) = (&mut self.confirmations, confirmation) {
                consumer.admission(tag, event.onset_frame, event.generation, false);
            }
            return false;
        }
        #[cfg(feature = "device-audio")]
        if let (Some(consumer), Some(tag)) = (&mut self.confirmations, confirmation) {
            consumer.admission(tag, event.onset_frame, event.generation, true);
        }
        self.pending
            .push_with_confirmation(event, confirmation, expected_sample_identity);
        true
    }

    /// Drop everything scheduled but not yet sounding; voices already
    /// sounding keep ringing through their envelopes. Live-update
    /// semantics: voices are independent, and a new evaluation never cuts
    /// one - a full reset would audibly click on every edit.
    ///
    /// The stop ramp calls this for each of its blocks, so nothing new
    /// starts under the ramp. The call also sets the clock to `frame`.
    pub fn retire_pending_at(&mut self, frame: u64) {
        self.pending.clear();
        self.frame = frame;
    }

    /// Silence everything sounding, over the choke ramp.
    ///
    /// A reload deliberately lets voices ring: an edit that cut them would
    /// click on every save. Auditioning is the other case - the snippet
    /// being replaced must stop, not play under the one asked for - so it
    /// has its own verb rather than a flag on the reload. The ramp is the
    /// choke group's, which is what a cut already sounds like here.
    pub fn cut_sounding_voices(&mut self, at_frame: u64) {
        self.cut_instrument_notes(at_frame);
        for voice in &mut self.voices {
            if voice.cut_fade_frame.is_none() {
                voice.cut_fade_frame = Some(at_frame);
            }
            let stop_at = (at_frame.saturating_sub(voice.start_frame)) as f32
                / self.sample_rate as f32
                + CUT_FADE_SECS;
            voice.stop_secs = voice.stop_secs.min(stop_at);
        }
    }

    /// An instrument holds its own notes. A cut of the engine voices at
    /// `at_frame` ends them at the same frame.
    fn cut_instrument_notes(&mut self, at_frame: u64) {
        let frames = at_frame.saturating_sub(self.frame).min(u64::from(u32::MAX)) as u32;
        let instruments = crate::insert::EFFECT_CHAIN * crate::insert::MAX_INSERT_BUSES;
        for instrument in self.orbit_inserts.iter_mut().skip(instruments).flatten() {
            instrument.cut_notes(frames);
        }
    }

    /// A choke is also used as an internal note-off. It never needs a
    /// voice slot and must remain possible when the polyphony cap is full.
    pub(super) fn choke_previous<K>(
        voices: &mut [Voice],
        event: &OnsetEvent<K>,
        frame: u64,
        sample_rate: u32,
    ) {
        let Some(cut) = event
            .cut
            .or_else(|| event.sample.and_then(|sample| sample.cut))
        else {
            return;
        };
        let nudge = event
            .sample
            .map(|sample| (f64::from(sample.nudge_secs) * f64::from(sample_rate)) as u64)
            .unwrap_or(0);
        let cut_at = event.onset_frame.saturating_add(nudge).max(frame);
        let key = cut.to_bits();
        // Voices retain trigger order, with no lifetime group registry limit.
        if let Some(previous) = voices
            .iter_mut()
            .filter(|voice| voice.cut_group == Some(key))
            .max_by_key(|voice| voice.id)
        {
            let fade = previous
                .cut_fade_frame
                .map_or(cut_at, |running| running.min(cut_at));
            previous.cut_fade_frame = Some(fade);
            let fade_end =
                fade.saturating_sub(previous.start_frame) as f32 / sample_rate as f32 + 0.011;
            previous.stop_secs = previous.stop_secs.min(fade_end);
        }
    }

    /// Takeover cut - the from-zero reload's (a rewind's) signature.
    ///
    /// Every voice of the OUTGOING rendition falls silent at `cut_frame`
    /// over the same choke ramp a cut group uses - click-free, so the
    /// restarted loop is heard alone, like a retriggered sample. An
    /// ordinary reload never calls this: its voices ring out by contract.
    ///
    /// `cut_frame` is the flip (the consumer's current block start) for an
    /// immediate restart: onsets the outgoing generation had already
    /// scheduled into the ghost window up to its takeover are part of what
    /// a restart replaces, so the consumer drops them from its ring and
    /// this arm only has to catch what is sounding or still activates.
    /// A QUANTISED rewind passes the takeover frame instead - the line the
    /// launch waited for - so the old score plays its countdown and every
    /// countdown activation pre-fades to stop at the line.
    ///
    /// The voices loop here is deliberately UNGUARDED by generation: it is
    /// only ever armed at a flip, where every sounding voice is by
    /// construction outgoing (the new generation has not admitted
    /// anything), or - for the quantised arm - before the line, where the
    /// new generation's first events target at/after the takeover and
    /// cannot have activated. Tails from generations older than the
    /// immediate outgoing one are choked too; a restart silences the past
    /// whole. The ACTIVATION pre-fade (at construction) stays
    /// generation-guarded: activations after the arm must prove they
    /// belong to the outgoing rendition before it stops them.
    ///
    /// Live input windows (`s("in")`) are cut like any other voice, on
    /// purpose. The input is a score event with a window, and a restarted
    /// loop opens its own window again from the top: the old one fading over
    /// the ramp while the new one opens hands the monitored signal over
    /// without a gap, where leaving it out of the cut would sound the input
    /// twice, at double level, until the old window's end.
    pub fn begin_takeover_cut(&mut self, cut_frame: u64, outgoing_generation: u64) {
        // A cut frame already behind the clock (a quantised flip published
        // late, or a line arm whose block has passed it) still fades from
        // NOW: the ramp's gain is `1 - (frame - fade) / ramp`, so a fade
        // pinned in the past is already at zero - a step to silence, the
        // click the choke ramp exists to avoid. The arm keeps the requested
        // frame so a late-activating outgoing onset is pre-faded to it.
        let fade_from = cut_frame.max(self.frame);
        self.cut_instrument_notes(fade_from);
        for voice in self.voices.iter_mut().filter(|voice| !voice.piano) {
            voice.cut_fade_frame = Some(match voice.cut_fade_frame {
                Some(fade) => fade.min(fade_from),
                None => fade_from,
            });
            let stop_at = (fade_from.saturating_sub(voice.start_frame)) as f32
                / self.sample_rate as f32
                + CUT_FADE_SECS;
            voice.stop_secs = voice.stop_secs.min(stop_at);
        }
        self.armed_takeover_cut = Some((cut_frame, outgoing_generation));
    }

    /// Reload takeover: the new generation re-queries from `takeover_frame`,
    /// so it replaces pending onsets at or after that frame. Earlier pending
    /// onsets cover the horizon the old generation already scheduled and
    /// must still sound, or every save cuts the music for one continuity
    /// margin. Like [`Self::retire_pending_at`], the clock syncs to the
    /// consumer's block cursor. The sync does nothing in steady playback. A
    /// consumer that begins mid-stream needs it.
    pub fn retire_pending_from(&mut self, takeover_frame: u64, at_frame: u64) {
        self.retire_pending_by(at_frame, |_| takeover_frame);
    }

    /// [`Self::retire_pending_from`] for an edit's flip to the generation
    /// `incoming`. The outgoing voices ring out, and those whose onset the
    /// incoming generation plays again stand in for its copies: see
    /// [`Self::keep_started_onsets`].
    pub(crate) fn hand_over_from(&mut self, incoming: u64, takeover_frame: u64, at_frame: u64) {
        self.retire_pending_by(at_frame, |_| takeover_frame);
        self.keep_started_onsets(incoming, at_frame, |_| takeover_frame);
    }

    /// [`Self::hand_over_from`] for an edit's flip that lands while a
    /// takeover cut is still ahead of it: a control requery inside a
    /// quantised rewind's head-room. The requery's takeover belongs to the
    /// score it re-queries, the restarted one, which has nothing before the
    /// line. Retired from there, the countdown's own onsets between the
    /// requery's takeover and the line were lost and nothing replaced them:
    /// moving a slider in the last bar muted the rest of the countdown. So
    /// the generation the arm names as outgoing is retired from the cut
    /// frame, its line, and every other generation from the takeover.
    pub(crate) fn retire_pending_keeping_countdown(
        &mut self,
        incoming: u64,
        takeover_frame: u64,
        (cut_frame, outgoing): (u64, u64),
        at_frame: u64,
    ) {
        let horizon = |generation| {
            if generation == outgoing {
                cut_frame
            } else {
                takeover_frame
            }
        };
        self.retire_pending_by(at_frame, horizon);
        self.keep_started_onsets(incoming, at_frame, horizon);
    }

    /// Retire every pending onset at or after the frame `horizon` names for
    /// its generation, reporting each as superseded, and sync the clock to
    /// `at_frame`. A borrowed closure over a fixed pool: nothing allocates.
    ///
    /// A voice is never retired here, also not an SBD voice whose graph runs
    /// ahead of its source: see [`Self::keep_started_onsets`].
    fn retire_pending_by(&mut self, at_frame: u64, horizon: impl Fn(u64) -> u64) {
        #[cfg(feature = "device-audio")]
        let confirmations = &mut self.confirmations;
        self.pending
            .retain_with_confirmation(|event, confirmation, _| {
                let keep = event.controls.piano || event.onset_frame < horizon(event.generation);
                #[cfg(feature = "device-audio")]
                if !keep && let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                    consumer.superseded(tag);
                }
                #[cfg(not(feature = "device-audio"))]
                let _ = confirmation;
                keep
            });
        self.frame = at_frame;
    }

    /// Mark the voices that stand in for events of `incoming`, the
    /// generation an edit's flip installs on the block at `at_frame`.
    ///
    /// The incoming generation plays every onset from its takeover frame
    /// again. A voice of an older generation with its onset on that frame or
    /// after it has started, so the takeover keeps it and the onset sounds.
    /// The caller leaves out the incoming copy, or the onset sounds twice.
    /// Such a voice is one of two kinds:
    ///
    /// - An SBD voice. Its graph starts one lead before its onset.
    /// - Any voice, when the flip lands after its takeover frame. A control
    ///   requery can publish that late.
    ///
    /// ```text
    ///              flip      takeover         onset
    /// old voice  ---|-- graph runs, source parked --x==== kept
    /// new event     |           |                 x       left out
    /// ```
    ///
    /// The two generations can round one hit to either side of the takeover
    /// frame. An SBD voice with its onset one frame before that frame is
    /// marked too, for the copy on the takeover frame only.
    ///
    /// `horizon` names the takeover frame for a voice's generation, as for
    /// [`Self::retire_pending_by`]. Frame zero names no takeover: the
    /// incoming generation starts a new lifetime and plays nothing again.
    fn keep_started_onsets(&mut self, incoming: u64, at_frame: u64, horizon: impl Fn(u64) -> u64) {
        let mut last_onset = None;
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| !voice.piano && voice.generation < incoming)
        {
            let takeover_frame = horizon(voice.generation);
            let one_frame_before = takeover_frame.checked_sub(1) == Some(voice.start_frame)
                && voice.graph_start_frame < voice.start_frame;
            let kept =
                one_frame_before || (takeover_frame != 0 && voice.start_frame >= takeover_frame);
            voice.replayed_by = kept.then_some(incoming);
            voice.replayed_on_next_frame = one_frame_before;
            if kept {
                last_onset = last_onset.max(Some(voice.start_frame));
            }
        }
        let reach = kept_onset_reach_frames(self.sample_rate);
        self.kept_onsets =
            last_onset.map(|frame| (incoming, at_frame, frame.saturating_add(reach)));
    }

    /// Whether a kept voice stands in for the event of `generation` aimed
    /// at `onset_frame`. It is the nearest voice that
    /// [`Self::keep_started_onsets`] marked for that generation, at most
    /// [`KEPT_ONSET_REACH_SECS`] away. The voice then stands in for no
    /// other event, so an event with no voice of its own near it sounds.
    ///
    /// `early_graph` says that the event is an SBD hit, and an event pairs
    /// only with a voice of its kind. Any other voice started before the
    /// block the flip landed on. It pairs only with an event aimed before
    /// that block, so an event that is on time always sounds.
    ///
    /// A voice one frame before the takeover frame pairs only with the
    /// event on that frame, so it never takes the next hit of a roll.
    ///
    /// A voice that a cut silences from its onset stands in for nothing. For
    /// a voice one frame before the takeover frame, that is a cut on the
    /// takeover frame or before it.
    /// Runs on the audio callback: a scan of the voice pool, no allocation.
    pub(crate) fn kept_onset_stands_for(
        &mut self,
        onset_frame: u64,
        generation: u64,
        early_graph: bool,
    ) -> bool {
        let Some((incoming, landed_at, last_frame)) = self.kept_onsets else {
            return false;
        };
        if generation != incoming
            || onset_frame > last_frame
            || (!early_graph && onset_frame >= landed_at)
        {
            return false;
        }
        let reach = kept_onset_reach_frames(self.sample_rate);
        let nearest = self
            .voices
            .iter_mut()
            .filter(|voice| {
                let (copy_frame, reach) = if voice.replayed_on_next_frame {
                    (voice.start_frame.saturating_add(1), 0)
                } else {
                    (voice.start_frame, reach)
                };
                voice.replayed_by == Some(generation)
                    && (voice.graph_start_frame < voice.start_frame) == early_graph
                    && voice.cut_fade_frame.is_none_or(|fade| fade > copy_frame)
                    && copy_frame.abs_diff(onset_frame) <= reach
            })
            .min_by_key(|voice| voice.start_frame.abs_diff(onset_frame));
        match nearest {
            Some(voice) => {
                voice.replayed_by = None;
                true
            }
            None => false,
        }
    }

    /// Whether an older generation started the SBD graph of the onset at
    /// `onset_frame`, at most one frame away. This is the rule for an event
    /// of a generation that a newer flip replaced. Two flips can land before
    /// the device reads the events of the first one, and then no voice is
    /// marked for that generation: see [`Self::keep_started_onsets`]. The
    /// event is still a copy of the onset that the voice plays.
    ///
    /// A voice that a cut silences from its onset stands in for nothing.
    /// Runs on the audio callback: a scan of the voice pool, no allocation.
    pub(crate) fn started_graph_plays(&self, onset_frame: u64, generation: u64) -> bool {
        self.voices.iter().any(|voice| {
            !voice.piano
                && voice.graph_start_frame < voice.start_frame
                && voice.generation < generation
                && voice
                    .cut_fade_frame
                    .is_none_or(|fade| fade > voice.start_frame)
                && voice.start_frame.abs_diff(onset_frame) <= 1
        })
    }

    /// Kill the oldest sounds when the polyphony cap is exceeded, run
    /// before every new sound is admitted.
    ///
    /// The kill condition is re-read as the pool shrinks (see
    /// [`polyphony_kills`]), so it settles at the configured limit. A
    /// killed sound leaves the pool at once and cannot be chosen again,
    /// though it is still audible through its quarter-second fade - which
    /// is why the count here ignores voices already fading.
    ///
    /// Without a cap, dense music grows steadily louder as voices pile up:
    /// a generated 916-onset score measured +3.899 dB overall.
    pub(super) fn cull_for_polyphony(
        voices: &mut [Voice],
        onset_frame: u64,
        sample_rate: u32,
        max_polyphony: usize,
    ) -> usize {
        let active = voices
            .iter()
            .filter(|voice| voice.polyphony_fade_frame.is_none())
            .count();
        Self::fade_oldest_voices(
            voices,
            polyphony_kills(active, max_polyphony),
            onset_frame,
            sample_rate,
        )
    }

    pub(super) fn fade_oldest_voices(
        voices: &mut [Voice],
        count: usize,
        onset_frame: u64,
        sample_rate: u32,
    ) -> usize {
        let mut faded = 0;
        for _ in 0..count {
            // `voices` is pushed in trigger order and `retain_mut` keeps it, so
            // the first still-counted voice is the oldest.
            let Some(oldest) = voices
                .iter_mut()
                .find(|voice| voice.polyphony_fade_frame.is_none())
            else {
                break;
            };
            oldest.polyphony_fade_frame = Some(onset_frame);
            let stop_at = (onset_frame.saturating_sub(oldest.start_frame)) as f32
                / sample_rate as f32
                + POLYPHONY_FADE_SECS;
            oldest.stop_secs = oldest.stop_secs.min(stop_at);
            faded += 1;
        }
        faded
    }

    pub(super) fn gate_value(duration: f32, envelope: Envelope) -> f32 {
        if duration < envelope.attack_secs {
            duration / envelope.attack_secs
        } else if duration < envelope.attack_secs + envelope.decay_secs {
            1.0 + (envelope.sustain - 1.0)
                * ((duration - envelope.attack_secs) / envelope.decay_secs)
        } else {
            envelope.sustain
        }
    }

    pub(super) fn envelope(t: f32, duration: f32, envelope: Envelope, gate_value: f32) -> f32 {
        if duration < 0.0 || t < 0.0 {
            return 0.0;
        }
        let attack = envelope.attack_secs;
        let decay = envelope.decay_secs;
        let sustain = envelope.sustain;
        let release = envelope.release_secs;
        if t >= duration {
            if release <= 0.0 || t >= duration + release {
                return 0.0;
            }
            return gate_value * (1.0 - (t - duration) / release);
        }
        if t < attack {
            return t / attack;
        }
        if t < attack + decay {
            let u = (t - attack) / decay;
            return 1.0 + (sustain - 1.0) * u;
        }
        sustain
    }

    /// Render one sample of the complete FM operator matrix.
    ///
    /// Read every operator at its current phase, sum routes into each target,
    /// then advance phases. This order gives every route the same phase
    /// snapshot and keeps floating-point accumulation consistent.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_fm_offset(
        fm: &FmControls,
        freq_hz: f32,
        t: f32,
        duration_secs: f32,
        quantum_skip: u8,
        phases: &mut [f64; crate::backend::MAX_FM_OPERATORS + 1],
        noise: &mut [NoiseGen; crate::backend::MAX_FM_OPERATORS + 1],
        mod_adds: &ModAdds,
        tables: &PeriodicWaveTables,
        sample_rate: f64,
    ) -> f32 {
        // The depth automation is read from the start of the onset quantum.
        // Before that automation begins, depth remains at unity.
        let param_t = t - f32::from(quantum_skip) / sample_rate as f32;
        let depth_env = |envelope: Option<Envelope>, exponential: bool| match envelope {
            None => 1.0,
            _ if param_t < 0.0 => 1.0,
            Some(envelope) if exponential => crate::biquad::FilterStage::frequency_at(
                crate::backend::FilterEnvelope {
                    attack_secs: envelope.attack_secs,
                    decay_secs: envelope.decay_secs,
                    sustain: f64::from(envelope.sustain),
                    release_secs: envelope.release_secs,
                    min_hz: 0.0,
                    max_hz: 1.0,
                },
                param_t,
                duration_secs,
            ),
            Some(envelope) => Self::envelope(
                param_t,
                duration_secs,
                envelope,
                Self::gate_value(duration_secs, envelope),
            ),
        };

        let mut signal = [0.0f32; crate::backend::MAX_FM_OPERATORS + 1];
        for (slot, operator) in fm.operators.iter().enumerate() {
            let Some(op) = operator else {
                continue;
            };
            let base = freq_hz * op.harmonicity;
            let shape = match op.waveform.oscillator() {
                Some(waveform) => tables.sample_for_frequency(waveform, phases[slot], base),
                None => {
                    let crate::backend::FmWave::Noise(kind) = op.waveform else {
                        unreachable!("only a noise shape has no oscillator")
                    };
                    noise[slot].next(kind, 0.0)
                }
            };
            signal[slot + 1] = shape * depth_env(op.env, op.env_exponential);
        }

        // The route depth uses the source's static nominal frequency. Live
        // frequency modulation moves that oscillator but not the gain of the
        // route carrying it.
        let mut incoming = [0.0f32; crate::backend::MAX_FM_OPERATORS + 1];
        for route in fm.routes.iter().flatten() {
            let Some(Some(op)) = fm.operators.get(usize::from(route.source) - 1) else {
                continue;
            };
            let amount = route.amount
                + route
                    .mod_slot
                    .and_then(|slot| mod_adds.fm_index.get(usize::from(slot)).copied())
                    .unwrap_or(0.0);
            let base = freq_hz * op.harmonicity;
            incoming[usize::from(route.target)] +=
                signal[usize::from(route.source)] * amount * base;
        }

        for (slot, operator) in fm.operators.iter().enumerate() {
            let Some(op) = operator else {
                continue;
            };
            if !op.waveform.accepts_modulation() {
                continue;
            }
            let base = freq_hz * op.harmonicity;
            let osc_hz =
                base + mod_adds.fm_freq.get(slot).copied().unwrap_or(0.0) + incoming[slot + 1];
            let phase = &mut phases[slot];
            *phase += f64::from(osc_hz) / sample_rate;
            // A large excursion can cross more than one period in a sample.
            *phase -= phase.floor();
        }

        incoming[0]
    }

    pub(super) fn channel_gains(pan: Option<f32>) -> (f32, f32) {
        let Some(pan) = pan else {
            return (1.0, 1.0);
        };
        let angle = f64::from(pan.clamp(0.0, 1.0)) * std::f64::consts::FRAC_PI_2;
        (angle.cos() as f32, angle.sin() as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod preview_replacement_tests {
        use super::*;

        fn note(frame: u64, frequency: f32, epoch: u64) -> OnsetEvent {
            let mut event = OnsetEvent::new(frame, frequency, 0.1, 3.0);
            event.controls.preview_epoch = epoch;
            event
        }

        #[test]
        fn replacing_preview_fades_only_older_preview_voices_at_the_new_onset() {
            let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
            backend.note(note(0, 220.0, 0)); // the user's score
            backend.note(note(0, 330.0, 1));
            backend.note(note(0, 440.0, 1)); // a chord, not a choke group
            backend.note(note(512, 550.0, 2));
            backend.note(note(512, 660.0, 2));
            let mut out = [0.0; 256];
            for _ in 0..4 {
                backend.process_block(&mut out, 128);
            }
            assert_eq!(backend.voices.len(), 3);
            assert!(backend.voices.iter().all(|v| v.cut_fade_frame.is_none()));
            backend.process_block(&mut out, 128);
            assert_eq!(backend.voices.len(), 5);
            for voice in &backend.voices {
                assert_eq!(
                    voice.cut_fade_frame,
                    (voice.preview_epoch == 1).then_some(512)
                );
            }
            // Even a late outgoing onset cannot cut the new preview or revive the old one.
            backend.note(note(640, 770.0, 1));
            for _ in 0..6 {
                backend.process_block(&mut out, 128);
            }
            assert_eq!(backend.voices.len(), 3);
            assert!(backend.voices.iter().all(|v| v.preview_epoch != 1));
            assert!(backend.voices.iter().all(|v| v.cut_fade_frame.is_none()));
            assert!(out.iter().all(|v| v.is_finite()));
            assert!(out.iter().any(|v| v.abs() > 0.001));
        }
    }

    mod held_piano_tests {
        use super::*;

        fn held(frequency: f32, group: Option<f32>) -> OnsetEvent {
            let mut event = OnsetEvent::new(0, frequency, 0.1, f32::INFINITY).with_cut(group);
            event.controls.waveform = Waveform::Triangle;
            event.controls.envelope = Envelope {
                attack_secs: 0.005,
                decay_secs: 0.1,
                sustain: 0.7,
                release_secs: 0.02,
            };
            event.controls.lfo_end_secs = f32::INFINITY;
            event.controls.filter_lfo_end_secs = f32::INFINITY;
            event
        }

        fn release(frame: u64, group: f32) -> OnsetEvent {
            let mut event = OnsetEvent::new(frame, 440.0, 0.0, 0.01).with_cut(Some(group));
            event.controls.choke_only = true;
            event
        }

        fn advance(backend: &mut ScalarBackend, blocks: usize) {
            let mut out = [0.0; 256];
            for _ in 0..blocks {
                backend.process_block(&mut out, 128);
                assert!(out.iter().all(|sample| sample.is_finite()));
            }
            assert!(out.iter().any(|sample| sample.abs() > 0.0001));
        }

        #[test]
        fn held_piano_chord_survives_long_hold_and_releases_only_its_key() {
            let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
            backend.note(held(110.0, None)); // score
            backend.note(held(220.0, Some(f32::MAX))); // browser preview
            backend.note(held(330.0, Some(100.0))); // physical piano key one
            backend.note(held(440.0, Some(101.0))); // physical piano key two
            advance(&mut backend, 2000); // 5.3s: beyond every browser preview gate
            assert_eq!(backend.voices.len(), 4);
            assert!(
                backend
                    .voices
                    .iter()
                    .all(|voice| voice.cut_fade_frame.is_none())
            );
            backend.note(release(backend.frame, 100.0));
            advance(&mut backend, 16);
            assert_eq!(backend.voices.len(), 3);
            assert!(!backend.voices.iter().any(|voice| voice.freq_hz == 330.0));
            assert!(backend.voices.iter().any(|voice| voice.freq_hz == 440.0));
            backend.note(release(backend.frame, 101.0));
            advance(&mut backend, 16);
            assert_eq!(backend.voices.len(), 2);
            assert!(
                backend
                    .voices
                    .iter()
                    .all(|voice| voice.cut_fade_frame.is_none())
            );
        }

        #[test]
        fn held_piano_key_release_never_steals_a_voice_at_full_polyphony() {
            for limit in [2, 4] {
                let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
                backend.set_max_polyphony(limit);
                backend.note(held(110.0, None)); // oldest score voice
                if limit == 4 {
                    backend.note(held(220.0, Some(f32::MAX))); // browser
                    backend.note(held(330.0, Some(100.0))); // other piano key
                }
                backend.note(held(440.0, Some(101.0))); // key being released
                advance(&mut backend, 8);
                assert_eq!(backend.voices.len(), limit);
                backend.note(release(backend.frame, 101.0));
                advance(&mut backend, 1);
                assert_eq!(
                    backend.voices.len(),
                    limit,
                    "release must not add a silent voice"
                );
                assert!(
                    backend
                        .voices
                        .iter()
                        .all(|voice| voice.polyphony_fade_frame.is_none())
                );
                advance(&mut backend, 16);
                assert_eq!(backend.voices.len(), limit - 1);
                assert!(
                    backend
                        .voices
                        .iter()
                        .all(|voice| voice.cut_fade_frame.is_none())
                );
                assert!(backend.voices.iter().any(|voice| voice.freq_hz == 110.0));
            }
        }

        #[test]
        fn held_piano_release_bypasses_full_prepared_voice_and_pending_capacity() {
            let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
            backend.set_max_polyphony(2);
            assert!(backend.try_note_prepared(held(110.0, None).into_prepared()));
            assert!(backend.try_note_prepared(held(440.0, Some(1.0)).into_prepared()));
            advance(&mut backend, 1);
            assert_eq!(backend.voices.len(), 2);
            // Normal admission would refuse this because all prepared voice
            // capacity is occupied. A key-up is not a third voice.
            assert!(backend.try_note_prepared(release(backend.frame, 1.0).into_prepared()));
            advance(&mut backend, 16);
            assert_eq!(backend.voices.len(), 1);
            assert_eq!(backend.voices[0].freq_hz, 110.0);
            assert!(backend.voices[0].polyphony_fade_frame.is_none());

            backend.reset();
            for (freq, group) in [(110.0, None), (440.0, Some(1.0))] {
                let mut event = held(freq, group);
                event.onset_frame = 128;
                assert!(backend.try_note_prepared(event.into_prepared()));
            }
            assert_eq!(backend.pending.len(), backend.pending.capacity());
            assert!(backend.try_note_prepared(release(129, 1.0).into_prepared()));
            advance(&mut backend, 18);
            assert_eq!(backend.voices.len(), 1);
            assert_eq!(backend.voices[0].freq_hz, 110.0);
        }

        #[test]
        fn held_piano_pending_notes_survive_score_retirement_and_rewind() {
            let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
            let mut event = held(440.0, Some(1.0));
            event.onset_frame = 1024;
            event.generation = 4;
            event.controls.piano = true;
            backend.note(event);
            backend.retire_pending_from(512, 0);
            assert_eq!(backend.pending.len(), 1);
            backend.begin_takeover_cut(1024, 4);
            advance(&mut backend, 16);
            assert_eq!(backend.voices.len(), 1);
            assert!(backend.voices[0].cut_fade_frame.is_none());
            backend.begin_takeover_cut(backend.frame, 5);
            advance(&mut backend, 16);
            assert_eq!(backend.voices.len(), 1);
            backend.note(release(backend.frame, 1.0));
            let mut out = [0.0; 256];
            for _ in 0..16 {
                backend.process_block(&mut out, 128);
            }
            assert!(backend.voices.is_empty());
            assert!(
                out.iter()
                    .all(|sample| sample.is_finite() && *sample == 0.0)
            );
        }

        #[test]
        fn held_piano_release_cannot_exhaust_choke_groups() {
            let mut backend = ScalarBackend::prepared(48_000, 128).unwrap();
            // More simultaneous groups than the old device-lifetime registry.
            for group in 0..96 {
                backend.note(held(110.0 + group as f32, Some(group as f32)));
            }
            advance(&mut backend, 1);
            assert_eq!(backend.voices.len(), 96);
            backend.note(release(backend.frame, 95.0));
            advance(&mut backend, 16);
            assert_eq!(backend.voices.len(), 95);
            assert!(
                backend
                    .voices
                    .iter()
                    .all(|voice| voice.cut_group != Some(95.0f32.to_bits()))
            );
            assert!(
                backend
                    .voices
                    .iter()
                    .all(|voice| voice.cut_fade_frame.is_none())
            );
        }
    }

    mod voice_layout_tests {
        use super::{
            FmState, FxStageStates, PreparedSupersaw, ScalarBackend, Voice, VoiceRenderControls,
        };
        use crate::backend::{
            AudioBackend, FilterControls, FmControls, FmOperator, FmRoute, FmWave, FxStage,
            MAX_FM_OPERATORS, MAX_FM_ROUTES, MAX_FX_STAGES, OnsetEvent, OscillatorControls,
            ShapeControls, StaticBiquad, VowelControls,
        };

        fn prepared_pre_distort_activity(controls: OscillatorControls) -> bool {
            let mut backend = ScalarBackend::prepared(48_000, 1).expect("backend");
            backend.note(OnsetEvent::new(0, 220.0, 0.5, 0.1).with_controls(controls));
            backend.process_block(&mut [0.0; 2], 1);
            backend.voices[0].has_pre_distort_fx
        }

        #[test]
        fn optional_state_stays_out_of_the_hot_voice_layout() {
            assert!(std::mem::size_of::<FxStageStates>() > 4 * 1024);
            assert!(std::mem::size_of::<FmState>() > 512);
            assert_eq!(
                std::mem::size_of::<Option<Box<FxStageStates>>>(),
                std::mem::size_of::<usize>()
            );
            assert_eq!(
                std::mem::size_of::<Option<Box<FmState>>>(),
                std::mem::size_of::<usize>()
            );
            assert!(
                std::mem::size_of::<Voice>() <= 5 * 1024,
                "the per-sample voice stride grew beyond 5 KiB"
            );
            assert!(
                std::mem::size_of::<VoiceRenderControls>()
                    + std::mem::size_of::<[Option<FxStage>; MAX_FX_STAGES]>()
                    < std::mem::size_of::<OscillatorControls>(),
                "activation-only or pooled FX controls leaked back into the hot voice stride"
            );
        }

        fn fm_controls() -> OscillatorControls {
            let mut operators = [None; MAX_FM_OPERATORS];
            operators[0] = Some(FmOperator {
                harmonicity: 2.0,
                waveform: FmWave::Sine,
                env: None,
                env_exponential: true,
            });
            let mut routes = [None; MAX_FM_ROUTES];
            routes[0] = Some(FmRoute {
                source: 1,
                target: 0,
                amount: 1.0,
                mod_slot: Some(0),
            });
            OscillatorControls {
                fm: Some(FmControls { operators, routes }),
                ..OscillatorControls::default()
            }
        }

        #[test]
        fn fm_state_is_leased_only_where_used_and_recycled() {
            let mut backend = ScalarBackend::prepared(48_000, 2).expect("backend");
            assert_eq!(backend.fm_state_pool.len(), 2);

            assert!(backend.try_note_prepared(OnsetEvent::new(0, 220.0, 0.5, 0.1)));
            assert!(
                backend.try_note_prepared(
                    OnsetEvent::new(0, 330.0, 0.5, 0.1)
                        .with_controls(fm_controls())
                        .into_prepared()
                )
            );
            backend.process_block(&mut [0.0; 2], 1);

            assert_eq!(backend.voices.len(), 2);
            assert_eq!(
                backend
                    .voices
                    .iter()
                    .filter(|voice| voice.fm_state.is_some())
                    .count(),
                1
            );
            assert_eq!(backend.fm_state_pool.len(), 1);

            backend.reset_at(0);
            assert!(backend.voices.is_empty());
            assert_eq!(backend.fm_state_pool.len(), 2);

            for frequency in [440.0, 550.0] {
                assert!(
                    backend.try_note_prepared(
                        OnsetEvent::new(0, frequency, 0.5, 0.1)
                            .with_controls(fm_controls())
                            .into_prepared()
                    )
                );
            }
            backend.process_block(&mut [0.0; 2], 1);

            assert_eq!(backend.voices.len(), 2);
            assert!(backend.voices.iter().all(|voice| voice.fm_state.is_some()));
            assert!(backend.fm_state_pool.is_empty());
        }

        #[test]
        fn supersaw_plan_preserves_the_original_static_lane_math() {
            for (voices, freqspread, panspread) in [
                (1.0_f32, 0.0_f32, 0.0_f32),
                (5.0, 0.6, 0.6),
                (8.0, 0.35, 0.8),
                (32.0, 1.25, -0.2),
                (64.0, 0.25, 1.2),
            ] {
                let plan = PreparedSupersaw::new(voices, freqspread, panspread);
                let voices = voices.max(1.0);
                let expected_count = (voices.ceil() as usize).clamp(1, super::MAX_UNISON);
                assert_eq!(plan.count, expected_count);

                let spread = freqspread.max(0.0);
                let (scale, center) = if voices < 2.0 {
                    (0.0, 0.0)
                } else {
                    (spread / (voices - 1.0), spread * 0.5)
                };
                for (lane, actual) in plan.detune_ratios.iter().take(expected_count).enumerate() {
                    let detune = lane as f32 * scale - center;
                    let expected = 2.0_f32.powf(detune / 12.0);
                    assert_eq!(actual.to_bits(), expected.to_bits());
                }

                let panspread01 = panspread * 0.5 + 0.5;
                let expected = ((1.0 - panspread01).sqrt(), panspread01.sqrt());
                assert_eq!(plan.pan_gains.0.to_bits(), expected.0.to_bits());
                assert_eq!(plan.pan_gains.1.to_bits(), expected.1.to_bits());
            }
        }

        #[test]
        fn filter_activity_is_prepared_when_the_voice_activates() {
            let mut backend = ScalarBackend::prepared(48_000, 1).expect("backend");

            backend.note(OnsetEvent::new(0, 220.0, 0.5, 0.1));
            backend.process_block(&mut [0.0; 2], 1);
            assert!(!backend.voices[0].has_filters);

            backend.reset_at(0);
            let controls = OscillatorControls {
                filters: FilterControls {
                    lowpass: Some(StaticBiquad {
                        frequency_hz: 800.0,
                        q: 1.0,
                    }),
                    ..FilterControls::default()
                },
                ..OscillatorControls::default()
            };
            backend.note(OnsetEvent::new(0, 220.0, 0.5, 0.1).with_controls(controls));
            backend.process_block(&mut [0.0; 2], 1);
            assert!(backend.voices[0].has_filters);
        }

        #[test]
        fn pre_distort_activity_is_prepared_when_the_voice_activates() {
            assert!(!prepared_pre_distort_activity(OscillatorControls::default()));
            assert!(prepared_pre_distort_activity(OscillatorControls {
                vowel: Some(VowelControls {
                    freqs: [300.0; 5],
                    gains: [1.0; 5],
                    qs: [1.0; 5],
                }),
                ..OscillatorControls::default()
            }));
            assert!(prepared_pre_distort_activity(OscillatorControls {
                coarse: Some(2.0),
                ..OscillatorControls::default()
            }));
            assert!(prepared_pre_distort_activity(OscillatorControls {
                crush: Some(6.0),
                ..OscillatorControls::default()
            }));
            assert!(prepared_pre_distort_activity(OscillatorControls {
                shape: Some(ShapeControls {
                    shape: 0.5,
                    postgain: 1.0,
                }),
                ..OscillatorControls::default()
            }));
        }
    }

    mod fm_carrier_tests {
        //! A frequency-modulated oscillator reads, at each sample, the band-limited
        //! table for the frequency it has then, and follows that frequency past
        //! Nyquist unclamped, as Firefox does; Chromium clamps the frequency there.

        use super::{VoiceRenderControls, oscillator_sample};
        use crate::Waveform;
        use crate::backend::{
            FmControls, FmOperator, FmRoute, FmWave, MAX_FM_OPERATORS, MAX_FM_ROUTES, OnsetEvent,
            OscillatorControls,
        };
        use crate::periodic_wave::prepared_tables;

        const RATE: usize = 48_000;

        /// `fm(index)`: one sine operator at the carrier's own frequency.
        fn simple_fm(index: f32) -> FmControls {
            let mut operators = [None; MAX_FM_OPERATORS];
            operators[0] = Some(FmOperator {
                harmonicity: 1.0,
                waveform: FmWave::Sine,
                env: None,
                env_exponential: false,
            });
            let mut routes = [None; MAX_FM_ROUTES];
            routes[0] = Some(FmRoute {
                source: 1,
                target: 0,
                amount: index,
                mod_slot: None,
            });
            FmControls { operators, routes }
        }

        /// One second of the left channel of a sawtooth at `freq_hz`, from a quarter
        /// of a second into a note held for two: the envelope is steady throughout.
        fn held_sawtooth(freq_hz: f32, fm: Option<FmControls>) -> Vec<f32> {
            let controls = OscillatorControls {
                waveform: Waveform::Sawtooth,
                fm,
                ..OscillatorControls::default()
            };
            let event = OnsetEvent::new(0, freq_hz, 0.5, 2.0).with_controls(controls);
            let mut backend = super::ScalarBackend::new();
            let pcm = crate::render::render_pcm(&mut backend, RATE as u32, 2 * RATE, &[event])
                .expect("render");
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|frame| frame[0])
                .skip(RATE / 4)
                .take(RATE)
                .collect()
        }

        /// The share of a one-second `signal`'s energy on the harmonics of `f0`
        /// (whole hertz, so each harmonic falls on a bin), DC aside.
        fn harmonic_share(signal: &[f32], f0: usize) -> f64 {
            let mut spectrum: Vec<rustfft::num_complex::Complex<f64>> = signal
                .iter()
                .map(|&sample| rustfft::num_complex::Complex::new(f64::from(sample), 0.0))
                .collect();
            rustfft::FftPlanner::new()
                .plan_fft_forward(spectrum.len())
                .process(&mut spectrum);
            let power = |bin: usize| spectrum[bin].norm_sqr();
            let total: f64 = (1..=RATE / 2).map(power).sum();
            let harmonic: f64 = (f0..=RATE / 2).step_by(f0).map(power).sum();
            harmonic / total
        }

        /// FM with the modulator at the carrier's frequency puts every partial on a
        /// harmonic of it. What lands between them is aliasing: partials a table
        /// chosen for the unmodulated note carries past Nyquist once the frequency
        /// swings up. At 293 Hz and an index of 40 the frequency swings to 12 kHz
        /// and back through zero, inside the band throughout.
        #[test]
        fn a_modulated_sawtooth_keeps_its_partials_below_nyquist() {
            let plain = harmonic_share(&held_sawtooth(293.0, None), 293);
            assert!(plain > 0.999, "the unmodulated sawtooth aliases: {plain}");
            let modulated = harmonic_share(&held_sawtooth(293.0, Some(simple_fm(40.0))), 293);
            assert!(
                modulated > 0.99,
                "{:.1}% of the modulated sawtooth's energy is aliasing",
                100.0 * (1.0 - modulated)
            );
        }

        /// Where the modulated frequency passes Nyquist, the band-limited table for
        /// it has no partials left: the carrier is silent for that part of each
        /// modulator cycle instead of folding back, and sounds for the rest of it,
        /// whichever way the frequency swings.
        #[test]
        fn a_carrier_swept_past_nyquist_is_silent_there_and_only_there() {
            let f0 = 830.61f64;
            let index = 100.0;
            let played = held_sawtooth(f0 as f32, Some(simple_fm(index as f32)));
            let peak = played
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            assert!(peak > 0.05, "the carrier is silent throughout: {peak}");
            let silent = played.iter().filter(|sample| sample.abs() < 1e-6).count() as f64
                / played.len() as f64;
            // The share of a modulator cycle spent at or past Nyquist.
            let steps = 100_000;
            let past = (0..steps)
                .filter(|step| {
                    let angle = std::f64::consts::TAU * f64::from(*step) / f64::from(steps);
                    (f0 * (1.0 + index * angle.sin())).abs() >= RATE as f64 / 2.0
                })
                .count() as f64
                / f64::from(steps);
            assert!(
                (silent - past).abs() <= 0.01,
                "the carrier is past Nyquist {:.1}% of the time and silent {:.1}% of it",
                100.0 * past,
                100.0 * silent
            );
        }

        /// With the modulator at the carrier's frequency, the carrier's phase gains
        /// exactly one cycle per modulator cycle, so every partial stays on a
        /// harmonic, including while the frequency is past Nyquist. Chromium's clamp
        /// there drops part of that cycle and moves every partial off the series.
        #[test]
        fn a_carrier_swept_past_nyquist_keeps_its_harmonic_series() {
            // g#5 and c6, to the nearest hertz, at fm(32).
            for f0 in [831usize, 1047] {
                let played = held_sawtooth(f0 as f32, Some(simple_fm(32.0)));
                let share = harmonic_share(&played, f0);
                assert!(
                    share > 0.8,
                    "only {:.1}% of the {f0} Hz carrier's energy is on its harmonics",
                    100.0 * share
                );
            }
        }

        /// A sine carrier past Nyquist plays the sine of its unclamped phase, which
        /// aliases, as Firefox's directly computed sine does. Chromium clamps the
        /// frequency to Nyquist, where its band-limited sine table is silent.
        #[test]
        fn a_sine_carrier_past_nyquist_follows_its_unclamped_phase() {
            let tables = prepared_tables(RATE as u32);
            let controls = VoiceRenderControls::from(OscillatorControls::<crate::KabelControls> {
                waveform: Waveform::Sine,
                ..OscillatorControls::default()
            });
            let freq = 440.0f32;
            for offset in [30_000.0f32, -30_000.0] {
                let played_hz = f64::from(freq + offset);
                let mut phase = 0.0f64;
                for frame in 0..RATE {
                    let played = oscillator_sample(
                        &tables,
                        &controls,
                        tables.selection(freq),
                        &mut phase,
                        &mut 0,
                        freq,
                        offset,
                        1.0,
                        RATE as f64,
                    );
                    let cycles = (played_hz * frame as f64 / RATE as f64).fract();
                    let expected = (std::f64::consts::TAU * cycles).sin() as f32;
                    assert!(
                        (played - expected).abs() < 1e-6,
                        "{played_hz} Hz, frame {frame}: {played} against {expected}"
                    );
                }
            }
        }
    }

    mod steady_oscillator_tests {
        //! An oscillator whose frequency holds still reads the table chosen at its
        //! start and steps its phase by one period's wrap, bit for bit. Only a moving
        //! frequency picks its table at each sample.

        use super::{VoiceRenderControls, oscillator_sample};
        use crate::Waveform;
        use crate::backend::{OscillatorControls, PartialsControls};
        use crate::periodic_wave::prepared_tables;

        const RATE: f64 = 48_000.0;

        /// Eight partials with both cosine and sine terms, so a phase that drifts
        /// shows in either. The second value holds the wave while the test reads it.
        fn custom_wave() -> (
            PartialsControls,
            std::sync::Arc<crate::periodic_wave::CustomWave>,
        ) {
            let real: Vec<f32> = (0..8).map(|k| 1.0 / (k + 1) as f32).collect();
            let imag: Vec<f32> = (0..8).map(|k| 0.5 / (k + 1) as f32).collect();
            let mut partials = crate::register_partials(8, &real, &imag);
            let wave = crate::partials::lease(&mut partials).expect("a registered wave");
            (partials, wave)
        }

        /// Against the steady oscillator written out: the table picked once from the
        /// start frequency, and the phase stepped by `freq / sr` and brought back by
        /// one period either way, all a frequency below the sample rate needs. The
        /// frequencies run from low to past Nyquist, and one runs backwards.
        #[test]
        fn a_steady_oscillator_reads_its_start_table_and_wraps_by_one_period() {
            let tables = prepared_tables(RATE as u32);
            let (custom, _held) = custom_wave();
            let voices = [
                (Waveform::Sine, None),
                (Waveform::Square, None),
                (Waveform::Sawtooth, None),
                (Waveform::Triangle, None),
                (Waveform::Sine, Some(custom)),
            ];
            for (waveform, partials) in voices {
                let controls =
                    VoiceRenderControls::from(OscillatorControls::<crate::KabelControls> {
                        waveform,
                        partials,
                        ..OscillatorControls::default()
                    });
                let voice = if partials.is_some() {
                    "custom wave".to_owned()
                } else {
                    format!("{waveform:?}")
                };
                let wave = partials.and_then(|partials| partials.wave);
                for freq in [27.5f32, 440.0, 3_520.0, 18_000.0, 30_000.0, -440.0] {
                    let selection = match wave {
                        Some(wave) => wave.wave().selection(freq, RATE as f32),
                        None => tables.selection(freq),
                    };
                    let mut phase = 0.0f64;
                    let mut limit = 0;
                    let mut expected_phase = 0.0f64;
                    for frame in 0..RATE as usize {
                        let played = oscillator_sample(
                            &tables, &controls, selection, &mut phase, &mut limit, freq, 0.0, 1.0,
                            RATE,
                        );
                        let expected = match wave {
                            Some(wave) => {
                                let wave = wave.wave();
                                wave.sample(expected_phase, selection)
                                    * wave.norm(wave.partial_limit(freq, RATE as f32))
                            }
                            None => tables.sample(waveform, expected_phase, selection),
                        };
                        expected_phase += f64::from(freq) / RATE;
                        if expected_phase >= 1.0 {
                            expected_phase -= 1.0;
                        }
                        if expected_phase < 0.0 {
                            expected_phase += 1.0;
                        }
                        assert_eq!(
                            played.to_bits(),
                            expected.to_bits(),
                            "{voice} at {freq} Hz, frame {frame}: sample"
                        );
                        assert_eq!(
                            phase.to_bits(),
                            expected_phase.to_bits(),
                            "{voice} at {freq} Hz, frame {frame}: phase"
                        );
                    }
                }
            }
        }
    }

    mod pending_event_tests {
        use super::PendingEvents;
        use crate::backend::OnsetEvent;

        fn event(onset_frame: u64) -> OnsetEvent<crate::CompactKabelControls> {
            OnsetEvent::new(onset_frame, onset_frame as f32, 1.0, 0.1)
        }

        #[test]
        fn reused_pending_slots_stay_in_admission_order() {
            let mut pending = PendingEvents::with_capacity(4);
            pending.push(event(10));
            pending.push(event(20));
            pending.push(event(30));

            pending.retain(|event| event.onset_frame != 20);
            assert_eq!(pending.len(), 2);
            let capacity = pending.capacity();

            // This reuses the physical slot formerly occupied by frame 20. Its
            // logical position must still be the tail, after the older frame 30.
            pending.push(event(40));
            let mut seen = Vec::new();
            pending.retain(|event| {
                seen.push(event.onset_frame);
                true
            });
            assert_eq!(seen, [10, 30, 40]);
            assert_eq!(pending.capacity(), capacity);

            pending.retain(|event| event.onset_frame >= 35);
            pending.push(event(50));
            let mut seen = Vec::new();
            pending.retain(|event| {
                seen.push(event.onset_frame);
                true
            });
            assert_eq!(seen, [40, 50]);
            assert_eq!(pending.capacity(), capacity);

            pending.clear();
            assert_eq!(pending.len(), 0);
            assert_eq!(pending.capacity(), capacity);
        }

        #[test]
        fn pending_slots_match_vec_order_through_repeated_reuse() {
            const CAPACITY: usize = 32;
            let mut pending = PendingEvents::with_capacity(CAPACITY);
            let mut expected = Vec::with_capacity(CAPACITY);
            let mut next_frame = 0u64;
            let capacity = pending.capacity();

            for round in 0..1_000u64 {
                let additions = ((round * 7 + 3) % 6) as usize;
                for _ in 0..additions.min(CAPACITY - expected.len()) {
                    let next = event(next_frame);
                    pending.push(next);
                    expected.push(next);
                    next_frame += 1;
                }

                let divisor = round % 7 + 2;
                pending.retain(|event| (event.onset_frame + round) % divisor != 0);
                expected.retain(|event| (event.onset_frame + round) % divisor != 0);

                let mut observed = Vec::with_capacity(expected.len());
                pending.retain(|event| {
                    observed.push(*event);
                    true
                });
                assert_eq!(
                    observed, expected,
                    "admission order changed in round {round}"
                );
                assert_eq!(pending.len(), expected.len());
                assert_eq!(pending.capacity(), capacity);
            }
        }
    }

    mod polyphony_tests {
        use super::{MAX_CONFIGURABLE_POLYPHONY, MAX_POLYPHONY, ScalarBackend, polyphony_kills};
        use crate::{AudioBackend, OnsetEvent};

        /// The kill loop re-reads its own condition as the pool shrinks, so it
        /// converges on the cap over successive onsets rather than clamping to
        /// it.
        #[test]
        fn the_polyphony_cap_kills_the_oldest_and_converges_rather_than_clamping() {
            assert_eq!(MAX_POLYPHONY, 128, "DEFAULT_MAX_POLYPHONY");

            // Under the cap nothing is touched - this is the ordinary case, and
            // a cap that fired here would thin out music it must leave alone.
            for active in [0, 1, 64, 127] {
                assert_eq!(
                    polyphony_kills(active, MAX_POLYPHONY),
                    0,
                    "{active} sounds is under the cap"
                );
            }

            // At and just over it, one goes.
            assert_eq!(polyphony_kills(128, MAX_POLYPHONY), 1);
            assert_eq!(polyphony_kills(129, MAX_POLYPHONY), 1);

            // Further over, it does NOT clamp back in one pass: `i` advances while
            // the size falls, so they meet in the middle.
            assert_eq!(
                polyphony_kills(130, MAX_POLYPHONY),
                2,
                "130 loses two, leaving 128"
            );
            assert_eq!(
                polyphony_kills(131, MAX_POLYPHONY),
                2,
                "131 loses two, leaving 129"
            );
            // 140 and the index meet in the middle at 133, so seven go.
            assert_eq!(polyphony_kills(140, MAX_POLYPHONY), 7);

            // Whatever the excess, it always leaves at least the cap standing.
            for active in [128, 150, 200, 512] {
                let left = active - polyphony_kills(active, MAX_POLYPHONY);
                assert!(
                    left >= MAX_POLYPHONY - 1,
                    "{active} active left only {left} sounding"
                );
                assert!(left < active, "{active} active must lose something");
            }
        }
        fn populated(limit: usize) -> ScalarBackend {
            let mut backend = ScalarBackend::prepared(8_000, 256).expect("backend");
            backend.set_max_polyphony(limit);
            for _ in 0..160 {
                backend.note(OnsetEvent::new(0, 220.0, 0.001, 2.0));
            }
            backend.process_block(&mut [0.0; 256], 128);
            backend
        }

        #[test]
        fn raising_polyphony_keeps_more_than_128_voices_sounding() {
            let mut default = populated(MAX_POLYPHONY);
            let mut raised = populated(MAX_CONFIGURABLE_POLYPHONY);
            assert_eq!(default.pressure.semantic_polyphony_fades, 32);
            assert_eq!(raised.pressure.semantic_polyphony_fades, 0);
            let mut default_output = [0.0; 256];
            let mut raised_output = [0.0; 256];
            for _ in 0..20 {
                default.process_block(&mut default_output, 128);
                raised.process_block(&mut raised_output, 128);
            }
            assert_eq!(default.voices.len(), 128);
            assert_eq!(raised.voices.len(), 160);
            let energy =
                |samples: &[f32]| samples.iter().map(|sample| sample * sample).sum::<f32>();
            assert!(energy(&raised_output) > energy(&default_output) * 1.4);
        }

        #[test]
        fn lowering_polyphony_fades_oldest_voices_without_cutting_or_restarting() {
            let mut backend = populated(MAX_CONFIGURABLE_POLYPHONY);
            let frame = backend.frame;
            backend.set_max_polyphony(64);
            assert_eq!(backend.frame, frame);
            assert_eq!(
                backend.voices.len(),
                160,
                "retiring voices must keep ringing"
            );
            assert_eq!(backend.pressure.semantic_polyphony_fades, 96);
            assert!(
                backend.voices[..96]
                    .iter()
                    .all(|voice| voice.polyphony_fade_frame == Some(frame))
            );
            assert!(
                backend.voices[96..]
                    .iter()
                    .all(|voice| voice.polyphony_fade_frame.is_none())
            );
            backend.set_max_polyphony(64);
            assert_eq!(
                backend.pressure.semantic_polyphony_fades, 96,
                "reapplying the setting must not fade more voices"
            );
            for _ in 0..20 {
                backend.process_block(&mut [0.0; 256], 128);
            }
            assert_eq!(backend.voices.len(), 64);
        }

        #[test]
        fn polyphony_is_bounded_per_backend_and_survives_reset_and_init() {
            let mut first = ScalarBackend::new();
            let second = ScalarBackend::new();
            assert_eq!(first.max_polyphony(), MAX_POLYPHONY);
            first.set_max_polyphony(0);
            assert_eq!(first.max_polyphony(), 1);
            first.set_max_polyphony(usize::MAX);
            assert_eq!(first.max_polyphony(), MAX_CONFIGURABLE_POLYPHONY);
            assert_eq!(second.max_polyphony(), MAX_POLYPHONY);
            first.init(8_000).expect("backend");
            first.reset();
            assert_eq!(first.max_polyphony(), MAX_CONFIGURABLE_POLYPHONY);
        }
    }

    mod ring_out_tests {
        /// Filters outlive the source that fed them; a voice that stops dead at
        /// its envelope loses the tail.
        #[test]
        fn a_ringing_voice_outlives_its_source_and_a_dry_one_does_not() {
            let sr = 48_000.0;
            let allowance = 0.01 + 256.0 / sr;
            let bare = super::stop_secs_for(0.6, false, sr);
            assert!(
                (bare - 0.6).abs() < 1e-6,
                "nothing ringing means nothing to wait for, got {bare}"
            );

            // vowel is the case this exists for: it lives outside FilterChain, so
            // it used to be missed even though its x8 makeup makes it the loudest
            // ring of the lot.
            let ringing = super::stop_secs_for(0.6, true, sr);
            assert!(
                (ringing - (0.6 + allowance)).abs() < 1e-6,
                "a ringing voice must outlive its source, got {ringing}"
            );
            assert!(ringing > bare);

            // The allowance rides on whatever stopped the source, so a slice that
            // ended early because the buffer ran out still gets it. A chopped
            // sample through a vowel is the case: its slices are short, and
            // truncating each ring cost corr 0.982.
            let sliced = super::stop_secs_for(0.05, true, sr);
            assert!(
                (sliced - (0.05 + allowance)).abs() < 1e-6,
                "a short slice rings just as long, got {sliced}"
            );
        }
    }

    mod note_end_tests {
        /// A tremolo or phaser LFO ends at the note's hold end PLUS its release,
        /// and the note here is an eighth of a second at 48 kHz with the default
        /// 10 ms release: 6480 frames in all.
        #[test]
        fn a_tremolo_lfo_outlives_the_note_by_its_release_and_no_further() {
            let sr = 48_000.0;
            let alive = |q| super::note_end_alive(q, 0, 0.0, 0.125, 0.01, sr);

            assert!(alive(0), "it runs from the note's own start");
            assert!(
                alive(6_400),
                "the last quantum before the release end is live"
            );
            assert!(!alive(6_528), "the first quantum past it is not");

            // This is the bug it exists for: a sample held for its own slice goes
            // on sounding long after the LFO has stopped, and every one of those
            // quanta is unmodulated.
            for q in (6_528..24_000).step_by(128) {
                assert!(!alive(q), "the ringing tail at {q} must be unmodulated");
            }
        }

        /// A long release carries the LFO with it, which is what makes the same
        /// score with `.release(2)` match sample for sample where the default
        /// release does not.
        #[test]
        fn a_long_release_keeps_the_lfo_running_over_the_whole_tail() {
            let sr = 48_000.0;
            let alive = |q| super::note_end_alive(q, 0, 0.0, 0.125, 2.0, sr);
            for q in (0..100_000).step_by(128) {
                assert!(alive(q), "the quantum at {q} is inside the release");
            }
            assert!(!alive(102_144), "past two seconds it is over");
        }

        /// A later note carries its own end, and a sub-sample onset shifts that
        /// end by the same fraction it shifts the start.
        #[test]
        fn each_note_ends_its_own_lfo() {
            let sr = 48_000.0;
            assert!(super::note_end_alive(48_000, 48_000, 0.0, 0.125, 0.01, sr));
            assert!(!super::note_end_alive(54_528, 48_000, 0.0, 0.125, 0.01, sr));
            assert!(!super::note_end_alive(6_480, 0, 0.5, 0.125, 0.01, sr));
        }
    }

    mod nudge_tests {
        /// `nudge` shifts the SOURCE, not the envelope, by a float number of
        /// seconds.
        #[test]
        fn a_nudge_starts_on_the_frame_at_or_after_it_never_the_one_before() {
            let sr = 48_000.0;

            // The values a person actually types all land a hair UNDER a whole
            // frame, so truncation dropped one every time.
            for (nudge, expected) in [(0.005_f32, 240_u32), (0.01, 480), (0.02, 960), (0.03, 1440)]
            {
                let (delay, lead) = super::nudged_start(nudge, 0.0, sr);
                assert_eq!(
                    delay, expected,
                    "nudge({nudge}) should wait {expected} frames"
                );
                assert!(lead < 0.001, "nudge({nudge}) left a lead of {lead}");
            }

            // And one that lands just OVER, which must not round back down.
            let (delay, lead) = super::nudged_start(0.025, 0.0, sr);
            assert_eq!(delay, 1_201);
            assert!(
                lead > 0.99,
                "0.025 starts just past frame 1200, lead was {lead}"
            );

            // No nudge must be bit-identical to before the fix: the lead is the
            // onset's own, and nothing waits.
            for onset_lead in [0.0_f32, 0.25, 0.5, 0.999] {
                let (delay, lead) = super::nudged_start(0.0, onset_lead, sr);
                assert_eq!(delay, 0, "an unnudged source must not wait");
                assert!(
                    (lead - f64::from(onset_lead)).abs() < 1e-9,
                    "expected the onset's own lead {onset_lead}, got {lead}"
                );
            }

            // A negative nudge cannot play before the voice exists, so it floors at
            // zero delay and turns the whole offset into lead.
            let (delay, lead) = super::nudged_start(-0.01, 0.0, sr);
            assert_eq!(delay, 0);
            assert!(
                (lead - 480.0).abs() < 0.01,
                "expected ~480 frames of lead, got {lead}"
            );

            // The lead is always a real sub-sample residue except where the delay
            // floored, which is the only case that can exceed one frame.
            for nudge in [0.0_f32, 0.001, 0.0123, 0.02, 0.5] {
                for onset_lead in [0.0_f32, 0.3, 0.7] {
                    let (_, lead) = super::nudged_start(nudge, onset_lead, sr);
                    assert!(
                        (0.0..=1.0).contains(&lead),
                        "nudge {nudge}/{onset_lead}: lead {lead}"
                    );
                }
            }
        }
    }

    mod noise_playback_tests {
        #[test]
        fn sbd_noise_gain_holds_the_exponential_ramp_endpoint() {
            let midpoint = super::sbd_noise_gain(0.0125);
            assert!((super::sbd_noise_gain(0.0) - 1.2).abs() < 1e-7);
            assert!((midpoint - (1.2_f32 * 0.001).sqrt()).abs() < 1e-6);
            assert!((super::sbd_noise_gain(0.025) - 0.001).abs() < 1e-7);
            assert!((super::sbd_noise_gain(0.25) - 0.001).abs() < 1e-7);
        }

        /// The noise buffer is played, not generated inline: playback starts at
        /// a float time and therefore reads between samples.
        #[test]
        fn a_noise_buffer_read_between_samples_loses_the_level_blink_loses() {
            // A noise note starts at the note's own float time, so the buffer
            // is read at a fractional position and linearly interpolated.
            // Adjacent white samples are independent, so that mixing attenuates
            // by sqrt((1-f)^2 + f^2) - the curve held to within 0.4% against
            // Chromium at eight forced offsets, so allow 2%.
            fn rms_of(frac: f32) -> f32 {
                let mut source = super::NoiseGen::at_onset(48_000, frac);
                let n = 40_000;
                let power: f32 = (0..n).map(|_| source.next(0, 0.0)).map(|s| s * s).sum();
                (power / n as f32).sqrt()
            }

            let aligned = rms_of(0.0);
            assert!(
                aligned > 0.5,
                "white noise should sit near 1/sqrt(3), got {aligned}"
            );
            for frac in [0.125_f32, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875] {
                let expected = ((1.0 - frac).powi(2) + frac.powi(2)).sqrt();
                let ratio = rms_of(frac) / aligned;
                assert!(
                    (ratio - expected).abs() < 0.02,
                    "offset {frac}: expected {expected:.4} of full level, got {ratio:.4}"
                );
            }

            // Half a sample is the worst case, and it is a full -3.01 dB.
            assert!((rms_of(0.5) / aligned - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02);

            // A frame-aligned source must be untouched - same sequence, not just
            // the same level, or every unshifted render moves.
            let mut plain = super::NoiseGen::new(48_000);
            let mut onset = super::NoiseGen::at_onset(48_000, 0.0);
            for i in 0..1_000 {
                assert_eq!(plain.next(0, 0.0), onset.next(0, 0.0), "diverged at {i}");
            }
        }
    }

    mod source_gone_tests {
        use crate::sample::{DecodedSample, SampleControls, SampleHold, SampleId};
        use crate::{AudioBackend, OnsetEvent, ScalarBackend};

        const RATE: u32 = 48_000;
        const BLOCK: usize = 128;

        fn sample_event(id: SampleId) -> OnsetEvent {
            let mut event = OnsetEvent::new(0, 440.0, 1.0, 4.0);
            event.sample = Some(SampleControls {
                sample: id,
                playback_rate: 1.0,
                begin: 0.0,
                end: 1.0,
                hold: SampleHold::Hap,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            });
            event
        }

        fn render(backend: &mut ScalarBackend) -> Vec<f32> {
            let mut output = vec![0.0; BLOCK * 2];
            backend.process_block(&mut output, BLOCK);
            output
        }

        /// An uninstall lands under a sounding voice: the voice goes silent for
        /// the rest of the block and is gone at its end, so the id can go to
        /// another sound without this voice playing that sound under its name.
        #[test]
        fn a_voice_whose_slot_was_cleared_retires_at_the_block_end() {
            let id = SampleId(9);
            let mut backend = ScalarBackend::prepared(RATE, 4).expect("backend");
            let pcm =
                DecodedSample::from_parts(RATE, 1, vec![0.5; RATE as usize * 4]).expect("pcm");
            assert!(backend.install_sample(id, Box::new(pcm)).is_ok());
            assert!(backend.try_note_prepared(sample_event(id).into_prepared()));

            let sounding = render(&mut backend);
            assert!(
                sounding.iter().any(|sample| sample.abs() > 1e-3),
                "the voice reads its slot"
            );
            assert_eq!(backend.voices.len(), 1);

            assert!(backend.clear_sample(id).is_some(), "the slot held the PCM");
            let cleared = render(&mut backend);
            assert!(
                cleared.iter().all(|sample| *sample == 0.0),
                "silence once the slot is empty"
            );
            assert!(
                backend.voices.is_empty(),
                "the voice retired with the block, well before its 4 s duration"
            );

            // The same id installed again is a different sound: nothing from the
            // old voice reads it.
            let other =
                DecodedSample::from_parts(RATE, 1, vec![-0.25; RATE as usize]).expect("pcm");
            assert!(backend.install_sample(id, Box::new(other)).is_ok());
            let quiet = render(&mut backend);
            assert!(
                quiet.iter().all(|sample| *sample == 0.0),
                "no voice survived to play the newcomer"
            );
        }

        /// The mixer's orbit fader sits after the duck and before the sum:
        /// half the fader, half the orbit, and the orbit's own peak reads
        /// post-fader. A backend built from nothing opens at unity.
        #[test]
        fn an_orbit_fader_scales_the_orbit_and_its_meter() {
            assert_eq!(super::OrbitGains::default().0, [1.0; super::MAX_ORBITS]);
            let id = SampleId(9);
            let loud = |gains: [f32; super::MAX_ORBITS]| {
                let mut backend = ScalarBackend::prepared(RATE, 4).expect("backend");
                let pcm =
                    DecodedSample::from_parts(RATE, 1, vec![0.5; RATE as usize]).expect("pcm");
                assert!(backend.install_sample(id, Box::new(pcm)).is_ok());
                backend.set_orbit_gains(&gains);
                assert!(backend.try_note_prepared(sample_event(id).into_prepared()));
                let out = render(&mut backend);
                let peak = out
                    .iter()
                    .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
                let orbit_peak = backend
                    .take_orbit_peaks()
                    .iter()
                    .fold(0.0f32, |peak, level| peak.max(*level));
                (peak, orbit_peak)
            };
            let (full, full_orbit) = loud([1.0; super::MAX_ORBITS]);
            let (half, half_orbit) = loud([0.5; super::MAX_ORBITS]);
            assert!(full > 0.1, "the voice sounds: {full}");
            assert!(
                (half - full * 0.5).abs() < 1e-3,
                "half the fader, half the orbit: {half} vs {full}"
            );
            assert!(
                (half_orbit - full_orbit * 0.5).abs() < 1e-3,
                "the meter is post-fader"
            );
        }

        /// A voice whose slot was never filled is dropped at onset, as before;
        /// this is the missing-sample path, not a retirement.
        #[test]
        fn a_voice_with_no_slot_never_starts() {
            let mut backend = ScalarBackend::prepared(RATE, 4).expect("backend");
            let started = backend.try_note_prepared(sample_event(SampleId(9)).into_prepared());
            let _ = render(&mut backend);
            assert!(backend.voices.is_empty(), "started: {started}");
            assert!(render(&mut backend).iter().all(|sample| *sample == 0.0));
        }
    }

    mod sample_resampling_mode_tests {
        use crate::sample::{
            DecodedSample, SampleControls, SampleHold, SampleId, SampleResamplingMode,
        };
        use crate::{AudioBackend, OnsetEvent, ScalarBackend};

        #[test]
        fn selected_mode_changes_the_sample_voice_render_path() {
            const OUTPUT_RATE: u32 = 48_000;
            const SOURCE_RATE: u32 = 24_000;
            const FRAMES: usize = 128;
            let id = SampleId(9);

            let render = |mode: Option<SampleResamplingMode>| {
                let mut backend = ScalarBackend::prepared(OUTPUT_RATE, 1).expect("backend");
                if let Some(mode) = mode {
                    backend.set_sample_resampling_mode(mode);
                    assert_eq!(backend.sample_resampling_mode(), mode);
                }
                let pcm = (0..SOURCE_RATE / 10)
                    .map(|frame| if frame % 2 == 0 { 0.0 } else { 0.2 })
                    .collect();
                let decoded = DecodedSample::from_parts(SOURCE_RATE, 1, pcm).expect("sample");
                assert!(backend.install_sample(id, Box::new(decoded)).is_ok());
                let mut event = OnsetEvent::new(0, 440.0, 1.0, 0.1);
                event.sample = Some(SampleControls {
                    sample: id,
                    playback_rate: 1.0,
                    begin: 0.0,
                    end: 1.0,
                    hold: SampleHold::Hap,
                    muted: false,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    reversed: false,
                    nudge_secs: 0.0,
                    cut: None,
                });
                assert!(backend.try_note_prepared(event.into_prepared()));
                let mut output = [0.0; FRAMES * 2];
                backend.process_block(&mut output, FRAMES);
                output
            };

            assert_eq!(
                ScalarBackend::default().sample_resampling_mode(),
                SampleResamplingMode::Linear
            );
            let linear = render(Some(SampleResamplingMode::Linear));
            let raw = render(Some(SampleResamplingMode::Raw));
            assert_eq!(render(None), linear, "the default retains the old sound");
            assert!(
                linear
                    .iter()
                    .zip(raw.iter())
                    .any(|(a, b)| (a - b).abs() > 1e-4),
                "fractional source positions must produce different audio"
            );
            assert!(linear.iter().any(|sample| sample.abs() > 1e-4));
            assert!(raw.iter().any(|sample| sample.abs() > 1e-4));
        }
    }

    mod sample_lifetime_tests {
        use crate::{
            AudioBackend, AudioEvent, DecodedSample, OnsetEvent, SampleControls, SampleHold,
            SampleId, ScalarBackend, StaticBiquad, SynthSource, WavetableControls,
        };

        const RATE: u32 = 48_000;
        const BLOCK: usize = 128;
        const ID: SampleId = SampleId(9);

        fn sample_event() -> OnsetEvent {
            let mut event = OnsetEvent::new(1_024, 440.0, 1.0, 0.1).with_onset_lead(0.75);
            event.sample = Some(SampleControls {
                sample: ID,
                playback_rate: 1.0,
                begin: 0.0,
                end: 1.0,
                hold: SampleHold::Slice,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            });
            event
        }

        fn queued(event: OnsetEvent) -> AudioEvent {
            AudioEvent {
                onset_id: 1,
                generation: event.generation,
                ui_visuals: event.ui_visuals,
                target_frame: event.onset_frame,
                onset_lead: event.onset_lead,
                freq_hz: event.freq_hz,
                gain: event.gain,
                duration_secs: event.duration_secs,
                controls: event.controls,
                sample: event.sample,
                wavetable: event.wavetable,
                synth: event.synth,
                cut: event.cut,
            }
        }

        fn wavetable() -> WavetableControls {
            WavetableControls {
                table: ID,
                frame_len: 64,
                voices: 1.0,
                lfo_shape: 0,
                phaserand: 0.0,
                freqspread: 0.0,
                panspread: 0.7,
                position: 0.0,
                pos_env_amount: 0.0,
                pos_attack: 0.0,
                pos_decay: 0.5,
                pos_sustain: 0.0,
                pos_release: 0.1,
                lfo_depth: 0.0,
                lfo_rate: 1.0,
                lfo_skew: 0.5,
                lfo_dc: 0.0,
                warp: 0.0,
                warp_mode: 0,
                warp_env_amount: 0.0,
                warp_attack: 0.0,
                warp_decay: 0.5,
                warp_sustain: 0.0,
                warp_release: 0.1,
                warp_lfo_depth: 0.0,
                warp_lfo_rate: 1.0,
                warp_lfo_skew: 0.5,
                warp_lfo_dc: 0.0,
                warp_lfo_shape: 0,
            }
        }

        /// Render through the last complete block before the retention deadline.
        /// The voice must sound past `audible_after_secs`, then retire before its
        /// bank slot can be reclaimed.
        fn assert_rendered_lifetime(
            event: OnsetEvent,
            decoded: DecodedSample,
            audible_after_secs: f32,
        ) {
            let (id, end_frame) = queued(event.clone())
                .sample_end_frame(&decoded, RATE)
                .unwrap();
            assert_eq!(id, ID);
            let audible_after =
                event.onset_frame + (audible_after_secs * RATE as f32).ceil() as u64;
            let mut backend = ScalarBackend::prepared(RATE, 4).expect("backend");
            assert!(backend.install_sample(id, Box::new(decoded)).is_ok());
            assert!(backend.try_note_prepared(event.into_prepared()));
            let mut frame = 0;
            let mut sounded_late = false;
            let mut output = [0.0; BLOCK * 2];
            while frame + BLOCK as u64 <= end_frame {
                backend.process_block(&mut output, BLOCK);
                if frame >= audible_after && output.iter().any(|sample| sample.abs() > 1e-5) {
                    sounded_late = true;
                }
                frame += BLOCK as u64;
            }
            assert!(sounded_late, "the fixture must exercise its long tail");
            assert!(
                backend.voices.is_empty(),
                "the voice must retire before sample retention expires at {end_frame}"
            );
        }

        #[test]
        fn sample_lifetime_covers_a_slow_reverse_slice_beyond_the_hap() {
            let decoded = DecodedSample::from_parts(44_100, 1, vec![0.5; 44_100]).unwrap();
            let mut event = sample_event();
            let sample = event.sample.as_mut().unwrap();
            sample.playback_rate = 0.25;
            sample.begin = 0.25;
            sample.end = 0.875;
            sample.reversed = true;
            sample.nudge_secs = 0.3;
            event.controls.envelope.release_secs = 0.05;
            let (_, end_frame) = queued(event.clone())
                .sample_end_frame(&decoded, RATE)
                .unwrap();
            let lifetime = (end_frame - event.onset_frame) as f64 / f64::from(RATE);
            // A 5/8 slice of a one-second source at quarter speed gates for 2.5 s,
            // regardless of its 0.1 s source hap, then takes its release and ring.
            assert!((2.55..2.58).contains(&lifetime), "{lifetime}");
            assert_rendered_lifetime(event, decoded, 2.4);
        }

        #[test]
        fn sample_lifetime_keeps_a_nudged_natural_end_and_its_filter_tail() {
            let decoded = DecodedSample::from_parts(RATE, 1, vec![0.5; 9_600]).unwrap();
            let mut event = sample_event();
            event.duration_secs = 2.0;
            let sample = event.sample.as_mut().unwrap();
            sample.hold = SampleHold::Hap;
            sample.begin = 0.5;
            sample.playback_rate = 0.5;
            sample.nudge_secs = 0.3;
            event.controls.filters.lowpass = Some(StaticBiquad {
                frequency_hz: 800.0,
                q: 2.0,
            });
            let (_, end_frame) = queued(event.clone())
                .sample_end_frame(&decoded, RATE)
                .unwrap();
            let lifetime = (end_frame - event.onset_frame) as f64 / f64::from(RATE);
            // The remaining 0.1 s takes 0.2 s at half speed, beginning at 0.3 s.
            assert!((0.515..0.52).contains(&lifetime), "{lifetime}");
            assert_rendered_lifetime(event, decoded, 0.5);
        }

        #[test]
        fn sample_lifetime_keeps_looped_zones_and_wavetables_through_release() {
            let decoded = DecodedSample::from_parts(RATE, 1, vec![0.5; 960]).unwrap();
            for use_wavetable in [false, true] {
                let mut event = sample_event();
                event.duration_secs = 0.4;
                event.controls.envelope.release_secs = 0.15;
                let sample = event.sample.as_mut().unwrap();
                sample.hold = SampleHold::Hap;
                sample.loop_secs = Some((0.001, 0.019));
                sample.nudge_secs = 0.05;
                if use_wavetable {
                    event.sample = None;
                    event.wavetable = Some(wavetable());
                }
                let (_, end_frame) = queued(event.clone())
                    .sample_end_frame(&decoded, RATE)
                    .unwrap();
                let lifetime = (end_frame - event.onset_frame) as f64 / f64::from(RATE);
                assert!((0.565..0.57).contains(&lifetime), "{lifetime}");
                assert_rendered_lifetime(event, decoded.clone(), 0.45);
            }
        }

        #[test]
        fn sample_lifetime_saturates_uncertain_ends_and_follows_source_precedence() {
            let decoded = DecodedSample::from_parts(RATE, 1, vec![0.5; 64]).unwrap();
            let mut event = queued(sample_event());
            event.wavetable = Some(wavetable());
            event.sample.as_mut().unwrap().sample = SampleId(10);
            assert_eq!(event.sample_end_frame(&decoded, RATE).unwrap().0, ID);
            for duration in [f32::NAN, f32::INFINITY, f32::MAX] {
                event.duration_secs = duration;
                assert_eq!(event.sample_end_frame(&decoded, RATE), Some((ID, u64::MAX)));
            }
            event.duration_secs = 1.0;
            assert_eq!(event.sample_end_frame(&decoded, 0), Some((ID, u64::MAX)));
            event.target_frame = u64::MAX - 1;
            assert_eq!(event.sample_end_frame(&decoded, RATE), Some((ID, u64::MAX)));
            event.synth = Some(SynthSource::Input { channel: 0 });
            assert_eq!(event.sample_end_frame(&decoded, RATE), None);
            event.synth = None;
            event.wavetable = None;
            event.sample = None;
            assert_eq!(event.sample_end_frame(&decoded, RATE), None);
        }

        #[test]
        fn sample_lifetime_rounds_past_the_renderers_long_duration_clock() {
            let decoded = DecodedSample::from_parts(RATE, 1, vec![0.5; 64]).unwrap();
            let mut event = queued(sample_event());
            event.wavetable = Some(wavetable());
            // At this duration, f32 frame spacing is much larger than 128 frames.
            event.duration_secs = 1e10;
            let (_, end_frame) = event.sample_end_frame(&decoded, RATE).unwrap();
            let age = (end_frame - event.target_frame - BLOCK as u64) as f32 / RATE as f32;
            assert!(age >= event.duration_secs);
            assert!(end_frame < u64::MAX);
        }
    }

    mod sbd_signal_path_tests {
        use crate::{AudioBackend, OnsetEvent, ScalarBackend, SynthSource};

        fn event(gain: f32) -> OnsetEvent {
            let mut event = OnsetEvent::new(0, 43.653_53, gain, 2.0);
            event.synth = Some(SynthSource::Sbd {
                decay_secs: 0.5,
                pdecay_secs: 0.5,
                penv_semitones: 36.0,
                stop_secs: 0.51,
            });
            event
        }

        fn render(sample_rate: u32, gain: f32, frames: usize) -> Vec<f32> {
            let mut backend = ScalarBackend::prepared(sample_rate, 1).expect("SBD backend");
            assert!(backend.try_note_prepared(event(gain).into_prepared()));
            let mut output = vec![0.0; frames * 2];
            backend.process_block(&mut output, frames);
            output.into_iter().step_by(2).collect()
        }

        /// A takeover keeps an SBD voice whose graph already runs. The voice
        /// stands in for one copy of its onset from the newer generation, also
        /// when a changed tempo moved the copy.
        #[test]
        fn a_takeover_keeps_a_started_sbd_graph_and_names_its_onset() {
            const ONSET: u64 = 9_600;
            let reach = super::kept_onset_reach_frames(48_000);
            let mut hit = event(1.0);
            hit.onset_frame = ONSET;
            hit.generation = 1;
            let mut backend = ScalarBackend::prepared(48_000, 2).expect("backend");
            assert!(backend.try_note_prepared(hit.into_prepared()));
            assert!(!backend.kept_onset_stands_for(ONSET, 2, true), "pending");
            // 150 ms: the graph is running, 50 ms before its source starts.
            let mut output = vec![0.0; 7_200 * 2];
            backend.process_block(&mut output, 7_200);
            backend.hand_over_from(2, 8_000, 7_200);
            assert_eq!(backend.voices.len(), 1, "a started graph is kept");
            assert!(!backend.kept_onset_stands_for(ONSET, 1, true), "its own");
            assert!(
                !backend.kept_onset_stands_for(ONSET, 2, false),
                "no SBD hit"
            );
            assert!(!backend.kept_onset_stands_for(ONSET + reach + 1, 2, true));
            assert!(backend.kept_onset_stands_for(ONSET + reach, 2, true));
            assert!(!backend.kept_onset_stands_for(ONSET, 2, true), "one event");
            // The next generation plays the onset again.
            backend.hand_over_from(3, 8_200, 7_200);
            assert!(backend.kept_onset_stands_for(ONSET - reach, 3, true));
        }

        /// The takeover gives an onset before its frame to the outgoing
        /// generation alone. The incoming generation has no copy of it, so the
        /// voice must not take the next hit of a roll.
        #[test]
        fn a_started_sbd_graph_before_the_takeover_stands_in_for_nothing() {
            const ONSET: u64 = 9_600;
            let mut hit = event(1.0);
            hit.onset_frame = ONSET;
            hit.generation = 1;
            let mut backend = ScalarBackend::prepared(48_000, 2).expect("backend");
            assert!(backend.try_note_prepared(hit.into_prepared()));
            let mut output = vec![0.0; 7_200 * 2];
            backend.process_block(&mut output, 7_200);
            backend.hand_over_from(2, ONSET + 2, 7_200);
            assert!(!backend.kept_onset_stands_for(ONSET, 2, true));
            assert!(!backend.kept_onset_stands_for(ONSET + 1, 2, true));
            assert!(!backend.kept_onset_stands_for(ONSET + 200, 2, true));
            // Frame zero names no takeover.
            backend.hand_over_from(3, 0, 7_200);
            assert!(!backend.kept_onset_stands_for(ONSET, 3, true));
        }

        /// Only an SBD graph starts before its onset. A voice of another kind
        /// that started one frame before the takeover frame is not marked.
        #[test]
        fn a_started_voice_of_another_kind_one_frame_before_the_takeover_is_not_marked() {
            const ONSET: u64 = 9_600;
            let mut hit = OnsetEvent::new(ONSET, 220.0, 0.5, 1.0);
            hit.generation = 1;
            let mut backend = ScalarBackend::prepared(48_000, 2).expect("backend");
            assert!(backend.try_note_prepared(hit.into_prepared()));
            let mut output = vec![0.0; 9_700 * 2];
            backend.process_block(&mut output, 9_700);
            backend.hand_over_from(2, ONSET + 1, 9_700);
            assert_eq!(backend.voices.len(), 1, "the voice has started");
            assert!(!backend.kept_onset_stands_for(ONSET + 1, 2, false));
        }

        /// A roll of hits 25 ms apart, moved 300 frames by the takeover. Each
        /// started graph takes the copy of its own hit. The copy of a hit that
        /// was still pending finds no voice and sounds.
        #[test]
        fn each_started_sbd_graph_of_a_roll_stands_in_for_its_own_hit() {
            const HITS: [u64; 4] = [9_600, 10_800, 12_000, 13_200];
            let mut backend = ScalarBackend::prepared(48_000, 4).expect("backend");
            for onset in HITS {
                let mut hit = event(1.0);
                hit.onset_frame = onset;
                hit.generation = 1;
                assert!(backend.try_note_prepared(hit.into_prepared()));
            }
            let mut output = vec![0.0; 7_300 * 2];
            backend.process_block(&mut output, 7_300);
            backend.hand_over_from(2, 7_400, 7_300);
            assert_eq!(backend.voices.len(), 3, "the last graph has not started");
            assert_eq!(backend.pending.len(), 0);
            for (index, onset) in HITS.into_iter().enumerate() {
                let stands_in = backend.kept_onset_stands_for(onset - 300, 2, true);
                assert_eq!(stands_in, index < 3, "hit {index}");
                let waiting = backend
                    .voices
                    .iter()
                    .filter(|voice| voice.replayed_by.is_some())
                    .map(|voice| voice.start_frame);
                assert!(waiting.eq(HITS[(index + 1).min(3)..3].iter().copied()));
            }
        }

        #[test]
        fn zero_input_waveshaper_interpolates_the_precomputed_curve() {
            // The curve has `sampleRate` entries (not sampleRate + 1), so input
            // zero falls between two unequal entries and does not map to zero.
            for (sample_rate, expected) in [
                (44_100, -0.000_045_351_473_f32),
                (48_000, -0.000_041_666_666_f32),
            ] {
                let curve = super::sbd_saturation_curve(sample_rate);
                let actual = super::sbd_saturation(0.0, &curve);
                assert!(
                    (actual - expected).abs() < 2e-10,
                    "{sample_rate} Hz: expected {expected}, got {actual}"
                );
                // At the score's default 0.8 gain this becomes exactly the one
                // negative 16-bit step measured in the live device capture.
                assert_eq!((actual * 0.8 * 32_768.0).round() as i32, -1);
            }
        }

        #[test]
        fn tonal_body_keeps_the_pinned_signal_path() {
            const SAMPLE_RATE: u32 = 48_000;
            const CHECKPOINTS: [usize; 11] =
                [0, 1, 31, 95, 127, 128, 511, 1_199, 2_400, 9_600, 22_000];
            // Independently captured oscillator -> WaveShaper -> gain -> mix
            // checkpoints. Noise was omitted at the source, so it can be removed
            // from the generated signal below.
            const EXPECTED_BODY: [f32; 11] = [
                -0.000_041_666_666,
                0.058_261_58,
                0.943_600_9,
                -0.836_568_83,
                -0.824_552,
                -0.808_099_2,
                -0.932_327_7,
                -0.390_854_63,
                0.532_205_4,
                0.042_446_423,
                -0.002_196_277_7,
            ];

            let output = render(SAMPLE_RATE, 1.0, CHECKPOINTS[CHECKPOINTS.len() - 1] + 1);
            let mut noise = super::NoiseGen::at_onset(SAMPLE_RATE, 0.0);
            let mut checkpoint = 0;
            for (frame, sample) in output.into_iter().enumerate() {
                let t = frame as f32 / SAMPLE_RATE as f32;
                let tonal = sample - noise.next(2, 0.0) * super::sbd_noise_gain(t);
                if frame == CHECKPOINTS[checkpoint] {
                    let expected = EXPECTED_BODY[checkpoint];
                    assert!(
                        (tonal - expected).abs() < 6e-5,
                        "frame {frame}: expected {expected}, got {tonal}"
                    );
                    checkpoint += 1;
                    if checkpoint == CHECKPOINTS.len() {
                        break;
                    }
                }
            }
        }
    }

    mod begin_frame_tests {
        /// A resampled buffer lands `begin` between samples; the read must
        /// resolve to a whole frame. Frame counts below are the real ones: the
        /// bundled 48 kHz bd is
        /// 12000 frames, and tr909's open hat is 34094 frames at 44.1 kHz, which
        /// becomes 37109.116 at 48 k.
        #[test]
        fn a_fractional_begin_resolves_to_a_whole_frame() {
            // 48 kHz source: already exact, and must not move.
            assert_eq!(super::begin_frame(0.2, 12_000.0, 1.0), 2_400.0);

            // Resampled source: every one of these was fractional, and the
            // measured corr tracked how far from a whole frame it sat.
            for begin in [0.05_f32, 0.1, 0.2, 0.25, 0.4, 0.5] {
                let landed = super::begin_frame(begin, 37_109.116, 1.0);
                assert_eq!(
                    landed,
                    landed.round(),
                    "begin({begin}) must land on a whole frame, got {landed}"
                );
                let exact = f64::from(begin) * 37_109.116;
                assert!(
                    (landed - exact).abs() <= 0.5,
                    "begin({begin}) moved {} frames, further than rounding",
                    (landed - exact).abs()
                );
            }
        }
    }

    mod wavetable_boundary_tests {
        use super::*;

        #[test]
        fn wavetable_warp_reads_stay_within_the_table_at_cycle_boundaries() {
            // A steep first edge exposes extrapolation immediately. Exercise the
            // real portable oscillator, with two frames and frame crossfading.
            let pcm: Vec<f32> = (0..4096)
                .map(|i| if i % 2 == 0 { -0.75 } else { 0.75 } * if i < 2048 { 1.0 } else { 0.5 })
                .collect();
            let mut bank = SampleBank::empty();
            bank.install(
                crate::SampleId(9),
                Box::new(crate::DecodedSample::from_parts(48000, 1, pcm.clone()).unwrap()),
            )
            .unwrap();
            let mut controls = crate::WavetableControls {
                table: crate::SampleId(9),
                frame_len: 2048,
                voices: 1.0,
                lfo_shape: 0,
                phaserand: 0.0,
                freqspread: 0.0,
                panspread: 0.7,
                position: 0.0,
                pos_env_amount: 0.0,
                pos_attack: 0.0,
                pos_decay: 0.5,
                pos_sustain: 0.0,
                pos_release: 0.1,
                lfo_depth: 0.0,
                lfo_rate: 1.0,
                lfo_skew: 0.5,
                lfo_dc: 0.0,
                warp: 0.0,
                warp_mode: 0,
                warp_env_amount: 0.0,
                warp_attack: 0.0,
                warp_decay: 0.5,
                warp_sustain: 0.0,
                warp_release: 0.1,
                warp_lfo_depth: 0.0,
                warp_lfo_rate: 1.0,
                warp_lfo_skew: 0.5,
                warp_lfo_dc: 0.0,
                warp_lfo_shape: 0,
            };
            for mode in 0..=21 {
                controls.warp_mode = mode;
                let mode = crate::warp::WarpMode::from_index(i32::from(mode));
                for amount in [0.0, 0.25, 0.5, 0.75, 1.0] {
                    controls.warp = amount;
                    for position in [0.0, 0.37, 1.0] {
                        controls.position = position;
                        for step in 0..=2048 {
                            let phase = step as f32 / 2048.0;
                            let warped = crate::warp::warp_phase(phase, amount, mode);
                            // Independent circular interpolation: wrap the entire
                            // position before selecting either sample or fraction.
                            let pos = (f64::from(warped) * 2048.0).rem_euclid(2048.0);
                            let index = pos.floor() as usize;
                            let fraction = (pos - pos.floor()) as f32;
                            let mut expected =
                                pcm[index] * (1.0 - fraction) + pcm[(index + 1) % 2048] * fraction;
                            expected *= 1.0 - position * 0.5;
                            if mode.flips_sample(phase, amount) {
                                expected = -expected;
                            }
                            expected *= 0.5_f32.sqrt();
                            let mut phases = [f64::from(phase); MAX_UNISON];
                            let (left, right) = ScalarBackend::wavetable_sample(
                                &bank,
                                &controls,
                                &mut phases,
                                &mut None,
                                &mut None,
                                0.0,
                                440.0,
                                0.0,
                                1.0,
                                48000.0,
                                WavetableAdds::default(),
                                None,
                            );
                            assert!(
                                (left - expected).abs() < 1e-5 && (right - expected).abs() < 1e-5,
                                "mode={mode:?} amount={amount} position={position} phase={phase}: {left} expected {expected}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn saw_step_matches_the_euclidean_remainder_bit_for_bit() {
        for x in [
            0.0f32,
            -0.0,
            1e-30,
            0.25,
            0.999_999_9,
            1.0,
            1.5,
            -0.25,
            f32::INFINITY,
            f32::NAN,
        ] {
            assert_eq!(saw_step(x).to_bits(), x.rem_euclid(1.0).to_bits(), "{x}");
        }
    }
}
