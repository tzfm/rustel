use super::*;

impl LiveCallbackMeters {
    pub(super) fn new(sample_rate: u32, publisher: &RealtimeLoadPublisher) -> Self {
        Self {
            master: crate::meter::MasterMeter::new(sample_rate),
            master_gain: None,
            load: RealtimeLoadMeter::new(sample_rate, publisher),
            pressure: RealtimePressureMeter::default(),
            sample_rate,
            limiter: crate::limiter::live::LiveLimiter::new(sample_rate),
        }
    }
}

impl Drop for CallbackErrorGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl CallbackGate {
    pub(super) fn new() -> Self {
        Self {
            state: AtomicU8::new(CALLBACK_PENDING),
            errors_in_flight: AtomicUsize::new(0),
        }
    }

    pub(super) fn is_active(&self) -> bool {
        self.state.load(Ordering::Acquire) == CALLBACK_ACTIVE
    }

    pub(super) fn retire(&self) {
        // Retire before shared status is reused. Error writers never wait.
        self.state.store(CALLBACK_RETIRED, Ordering::SeqCst);
        while self.errors_in_flight.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
    }

    pub(super) fn begin_error(&self) -> CallbackErrorGuard<'_> {
        // Retirement sees this writer, or its state check sees retirement.
        self.errors_in_flight.fetch_add(1, Ordering::SeqCst);
        CallbackErrorGuard(&self.errors_in_flight)
    }

    pub(super) fn activate(&self) -> Result<(), DevicePlaybackError> {
        self.state
            .compare_exchange(
                CALLBACK_PENDING,
                CALLBACK_ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| {
                DevicePlaybackError::Unavailable(
                    "live output failed before activation or was already activated".into(),
                )
            })
    }

    pub(super) fn render_or_silence<T>(&self, output: &mut [T], render: impl FnOnce(&mut [T]))
    where
        T: Sample + FromSample<f32>,
    {
        tripwire::audio_scope(|| {
            if self.is_active() {
                render(output);
            } else {
                output.fill(T::from_sample(0.0));
            }
        });
    }

    pub(super) fn report_error(&self, failed: &AtomicBool, errors: &AtomicU64) {
        let _writer = self.begin_error();
        // An error before activation belongs only to this candidate. The CAS
        // also closes the race between an error and the activation release.
        if self.state.compare_exchange(
            CALLBACK_PENDING,
            CALLBACK_FAILED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) == Err(CALLBACK_ACTIVE)
        {
            errors.fetch_add(1, Ordering::Relaxed);
            failed.store(true, Ordering::Release);
        }
    }

    fn report_xrun(&self, xruns: &AtomicU64) {
        let _writer = self.begin_error();
        if self.state.load(Ordering::SeqCst) == CALLBACK_ACTIVE {
            xruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(feature = "test-support")]
impl ManualLiveOutput {
    pub fn new(sample_rate: u32, initial_generation: u64) -> Result<Self, DevicePlaybackError> {
        Self::new_with_samples(sample_rate, initial_generation, &[])
    }

    pub fn new_with_samples(
        sample_rate: u32,
        initial_generation: u64,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<Self, DevicePlaybackError> {
        Self::new_with_samples_and_options(
            sample_rate,
            initial_generation,
            samples,
            LiveOutputOptions::default(),
        )
    }

    pub fn new_with_samples_and_options(
        sample_rate: u32,
        initial_generation: u64,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
        options: LiveOutputOptions,
    ) -> Result<Self, DevicePlaybackError> {
        let buffer_frames = options
            .buffer_preference()
            .resolve(SILENT_BUFFER_FRAMES, AudioBufferRange::Unknown)
            .map_err(buffer_preference_error)?;
        let shared = LiveShared::new(initial_generation);
        let backend = PreparedLiveOutput::backend(&shared, sample_rate, options, samples)?;
        let meters = LiveCallbackMeters::new(sample_rate, &shared.realtime_load);
        let gate = Arc::new(CallbackGate::new());
        let prepared = PreparedLiveOutput {
            handle: OutputHandle::Silent(SilentHandle {
                close: Arc::new(AtomicBool::new(false)),
                join: None,
                clock_hold: Arc::new(std::sync::atomic::AtomicU8::new(SILENT_CLOCK_RUNNING)),
            }),
            facts: AudioOutputFacts::new(
                AudioHost::Silent,
                SILENT_OUTPUT_NAME,
                sample_rate,
                2,
                None,
                buffer_frames,
                Some(buffer_frames),
            )
            .with_buffer_preference(options.buffer_preference()),
            gate: Arc::clone(&gate),
            tripwire_baseline: tripwire::Violations::capture(),
            backend_bytes: backend.prepared_bytes(),
        };
        let device = LiveScalarDevice::start_prepared(shared, options, prepared)?;
        Ok(Self {
            device,
            backend,
            meters,
            gate,
            underruns: PlaybackUnderruns::new(buffer_frames, sample_rate),
            _same_thread: std::marker::PhantomData,
        })
    }

    pub fn device(&self) -> &LiveScalarDevice {
        &self.device
    }

    /// Render interleaved stereo through the ordinary gated callback body.
    pub fn render(&mut self, output: &mut [f32]) {
        let shared = &self.device.shared;
        self.gate.render_or_silence(output, |output| {
            write_live_output(&mut self.backend, &mut self.meters, output, 2, shared);
        });
    }

    /// Render as an output on Windows does when its playback clock gives
    /// `playback_nanos` for this buffer, from the first buffer on, and the
    /// buffer plays `latency_nanos` after its callback. The output counts its
    /// underruns from the clock.
    pub fn render_at(&mut self, output: &mut [f32], playback_nanos: u64, latency_nanos: u64) {
        let shared = &self.device.shared;
        self.gate.render_or_silence(output, |output| {
            shared
                .buffer_playback_nanos
                .store(playback_nanos, Ordering::Release);
            self.underruns
                .observe(playback_nanos, latency_nanos, output.len() / 2, shared);
            write_live_output(&mut self.backend, &mut self.meters, output, 2, shared);
        });
    }
}

impl LiveScalarDevice {
    /// Verify that allocator events cross this device's reporting path.
    ///
    /// Opening a stream is not enough: a missing global allocator and a report
    /// accidentally hard-coded to zero both look like a clean callback. The
    /// canary must be exactly one allocation plus its release, the report must
    /// expose that exact delta, and no earlier violation may exist.
    pub fn arm_callback_tripwire(&mut self) -> bool {
        self.arm_callback_tripwire_checked().is_ok()
    }

    /// Diagnose a failed arm so callers can distinguish an absent allocator
    /// from a callback violation on an output that should be retired.
    pub fn arm_callback_tripwire_checked(&mut self) -> Result<(), CallbackTripwireArmError> {
        let before_report = self.report();
        let before_raw = tripwire::Violations::capture();
        let allocator_armed = tripwire::allocator_is_armed();
        let after_raw = tripwire::Violations::capture();
        let after_report = self.report();
        let raw_canary = after_raw.since(before_raw);
        let reported_canary = tripwire::Violations {
            allocs: after_report
                .callback_allocations
                .wrapping_sub(before_report.callback_allocations),
            frees: after_report
                .callback_frees
                .wrapping_sub(before_report.callback_frees),
        };
        // The positive control is not callback work. Exclude only its own
        // allocation and free; rebasing to `after_raw` would also erase a
        // previous or concurrent real-time violation.
        if allocator_armed {
            self.tripwire_canaries.allocs = self.tripwire_canaries.allocs.wrapping_add(1);
            self.tripwire_canaries.frees = self.tripwire_canaries.frees.wrapping_add(1);
        }
        if !allocator_armed {
            return Err(CallbackTripwireArmError::MissingAllocator);
        }
        if before_report.callback_allocations != 0
            || before_report.callback_frees != 0
            || before_report.callback_scope_misses != 0
        {
            return Err(CallbackTripwireArmError::PriorViolation {
                allocations: before_report.callback_allocations,
                frees: before_report.callback_frees,
                scope_misses: before_report.callback_scope_misses,
            });
        }
        if raw_canary
            != (tripwire::Violations {
                allocs: 1,
                frees: 1,
            })
            || reported_canary != raw_canary
        {
            return Err(CallbackTripwireArmError::CanaryMismatch {
                raw: raw_canary,
                reported: reported_canary,
            });
        }
        Ok(())
    }

    pub fn report(&self) -> LiveDeviceReport {
        let mut report = self.shared.report(self.stream_id);
        report.playhead_nanos = self.clock_nanos();
        report.plugin_workers = self.ready_plugin_workers();
        let violations = tripwire::Violations::capture()
            .since(self.tripwire_baseline)
            .since(self.tripwire_canaries);
        apply_tripwire_report(report, violations)
    }

    /// What this output holds in memory. Call it from the producer: it
    /// reads counters the producer moves, the caches it keeps and what was
    /// measured when the backend was prepared, and never the callback's own
    /// state, which it cannot see. A handful of loads and two short locks.
    pub fn memory(&self) -> LiveDeviceMemory {
        use std::mem::{size_of, size_of_val};
        let ring = |ring: &Ring| {
            ring.pushed().min(ring.capacity())
                * size_of::<crate::QueuedAudioEvent<crate::CompactKabelControls>>()
        };
        let orbit_reverbs: usize = self
            .reverb_cache
            .lock()
            .expect("producer reverb cache")
            .iter()
            .flatten()
            .map(|(_, bytes)| bytes)
            .sum();
        // The producer cache sees pending installs but forgets older
        // fingerprints after eight; the callback reports its full resident
        // pool, including reverbs leased to voices. Use the larger view.
        let pending_stage_reverbs = self
            .fx_reverb_cache
            .lock()
            .expect("producer fx reverb cache")
            .iter()
            .fold(0usize, |held, (_, bytes, _)| {
                if *bytes <= crate::scalar::MAX_FX_REVERB_BYTES.saturating_sub(held) {
                    held + bytes
                } else {
                    held
                }
            });
        let stage_reverbs = pending_stage_reverbs.max(
            self.shared
                .samples
                .fx_reverb_resident_bytes
                .load(Ordering::Acquire),
        );
        let master = &self.shared.analysis;
        let visual_taps = self
            .shared
            .visual_analysis
            .iter()
            .filter(|tap| tap.get().is_some())
            .count();
        LiveDeviceMemory {
            event_rings: ring(&self.shared.ring) + ring(&self.shared.immediate),
            input: self.shared.input.touched_bytes(),
            record: self.shared.record.touched_bytes(),
            reverbs: orbit_reverbs + stage_reverbs,
            backend: self.backend_bytes
                + self
                    .kabel_cache
                    .lock()
                    .expect("producer KabelSalat cache")
                    .iter()
                    .map(|entry| entry.bytes)
                    .sum::<usize>()
                    .max(
                        self.shared
                            .samples
                            .kabel_resident_bytes
                            .load(Ordering::Acquire),
                    )
                + (self
                    .program_cache
                    .lock()
                    .expect("producer KabelSalat program cache")
                    .len()
                    * size_of::<crate::assets::PreparedKabelProgram>())
                .max(
                    self.shared
                        .samples
                        .program_resident_bytes
                        .load(Ordering::Acquire),
                ),
            analysis: size_of_val(&**master)
                + master.sides.as_deref().map_or(0, size_of_val)
                + visual_taps * size_of::<LiveAnalysisTap>(),
        }
    }

    /// Cumulative voices the callback had to refuse.
    ///
    /// Cheap enough to poll every live iteration, unlike [`Self::report`],
    /// which also captures tripwire violations. The live loop watches this for
    /// CHANGE so it can surface a recoverable `live_error` without ending the
    /// set.
    pub fn refused_voices(&self) -> u64 {
        self.shared.refused_voices.load(Ordering::Acquire)
    }
}

fn apply_tripwire_report(
    mut report: LiveDeviceReport,
    violations: tripwire::Violations,
) -> LiveDeviceReport {
    report.callback_allocations = violations.allocs;
    report.callback_frees = violations.frees;
    report
}

/// Whether a stream error fails the stream. An xrun is a glitch the host
/// recovers from; any other kind, a rerouted default device included, has
/// the owner open the stream again or report the failure.
fn fails_stream(error: &cpal::Error) -> bool {
    error.kind() != cpal::ErrorKind::Xrun
}

/// The error callback of a stream whose owner watches `failed`.
pub(super) fn flag_stream_failure(
    failed: Arc<AtomicBool>,
) -> impl FnMut(cpal::Error) + Send + 'static {
    move |error| {
        if fails_stream(&error) {
            failed.store(true, Ordering::Release);
        }
    }
}

/// The live stream's error callback. Active xruns are counted without failing
/// the stream. Other errors go to its [`CallbackGate`].
pub(super) fn live_error_callback(
    gate: Arc<CallbackGate>,
    failed: Arc<AtomicBool>,
    errors: Arc<AtomicU64>,
    xruns: Arc<AtomicU64>,
) -> impl FnMut(cpal::Error) + Send + 'static {
    move |error| {
        if fails_stream(&error) {
            gate.report_error(&failed, &errors);
        } else {
            gate.report_xrun(&xruns);
        }
    }
}

// SAFETY: see the type's documentation. Under cpal =0.18.2's WASAPI host the
// handle is created, used and reverted on the `cpal_wasapi_out` render
// thread, and only the empty value is sent there; a cpal upgrade or another
// Windows host (ASIO) must recheck that ownership.
#[cfg(windows)]
unsafe impl Send for CallbackProAudio {}

#[cfg(windows)]
impl CallbackProAudio {
    /// Register the calling thread on the first call only. A method rather
    /// than a field access in the closure: the closure must capture the
    /// whole `Send` wrapper, not the field inside it.
    pub(super) fn attach_once(&mut self) {
        if self.0.is_none() {
            self.0 = Some(crate::mmcss::ProAudioThread::attach_critical());
        }
    }
}

#[cfg(target_os = "linux")]
impl CallbackRealTime {
    /// The class for a stream of `frames` at `rate`.
    pub(super) fn new(frames: u32, rate: u32) -> Self {
        Self {
            frames,
            rate,
            asked: false,
            promoted: None,
        }
    }

    /// Promotes the calling thread at the first call that finds
    /// `workers_ready` set. A worker with the class proves that the system
    /// gives the class to the process, so the call meets no refusal, whose
    /// error the crate allocates. Until then the thread keeps its priority.
    pub(super) fn promote_once(&mut self, workers_ready: &AtomicBool) {
        if !self.asked && workers_ready.load(Ordering::Acquire) {
            self.asked = true;
            self.promoted =
                audio_thread_priority::promote_current_thread_to_real_time(self.frames, self.rate)
                    .ok()
                    .map(|handle| (handle, std::thread::current().id()));
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for CallbackRealTime {
    fn drop(&mut self) {
        if let Some((handle, thread)) = self.promoted.take()
            && thread == std::thread::current().id()
        {
            let _ = audio_thread_priority::demote_current_thread_from_real_time(handle);
        }
    }
}

#[cfg(any(windows, feature = "test-support"))]
impl PlaybackUnderruns {
    /// The count of a stream of `buffer_frames` at `sample_rate`.
    pub(super) fn new(buffer_frames: u32, sample_rate: u32) -> Self {
        let sample_rate = u64::from(sample_rate.max(1));
        Self {
            sample_rate,
            buffer_nanos: (u64::from(buffer_frames) * 1_000_000_000 / sample_rate) as i64,
            submitted: 0,
            ahead: None,
            base: 0,
            rising: Some(0),
            latency: 0,
        }
    }

    /// Reads the clock at a buffer of `frames` that plays `playback_nanos`
    /// after the first buffer and `latency_nanos` after its callback. An
    /// underrun adds 1 to the xruns of `shared`. A stream that reported an
    /// error counts none.
    pub(super) fn observe(
        &mut self,
        playback_nanos: u64,
        latency_nanos: u64,
        frames: usize,
        shared: &LiveShared,
    ) {
        // At a change of the default device, Windows moves the stream to the
        // new device. cpal then reports an error. The gap of that move is no
        // underrun.
        if shared.failed.load(Ordering::Acquire) {
            return;
        }
        let submitted_nanos =
            (u128::from(self.submitted) * 1_000_000_000 / u128::from(self.sample_rate)) as i64;
        self.submitted += frames as u64;
        let ahead = (playback_nanos as i64).saturating_sub(submitted_nanos);
        // The first buffer has no rise to read.
        let rise = self
            .ahead
            .map_or(i64::MAX, |last| ahead.saturating_sub(last));
        self.ahead = Some(ahead);
        self.latency = self.latency.max(latency_nanos as i64);
        // WASAPI calls back at its device period, also with a larger buffer.
        let frames_nanos = (frames as u64 * 1_000_000_000 / self.sample_rate) as i64;
        if let Some(from) = self.rising {
            // The clock holds still once it moves a quarter of the time of
            // these frames or less. A clock that moves ahead for longer than
            // the latency and 2 buffers plays silence.
            if rise <= frames_nanos / 4 {
                self.base = ahead;
                self.rising = None;
            } else if submitted_nanos - from <= self.latency.saturating_add(2 * self.buffer_nanos) {
                self.base = ahead;
            } else {
                self.rising = None;
            }
        } else if ahead > self.base.saturating_add(2 * self.buffer_nanos) {
            shared.callback_xruns.fetch_add(1, Ordering::Relaxed);
            self.rising = Some(submitted_nanos);
        } else {
            // 1/1024 of the time of these frames: about 1 ms a second.
            self.base = ahead.min(self.base.saturating_add(frames_nanos / 1024));
        }
    }
}

pub(super) fn write_live_output<T>(
    backend: &mut LiveScalarBackend,
    meters: &mut LiveCallbackMeters,
    output: &mut [T],
    channels: usize,
    shared: &LiveShared,
) where
    T: Sample + FromSample<f32>,
{
    write_live_output_inner(backend, meters, output, channels, shared, true, true);
}

#[cfg_attr(test, inline(never))]
fn write_live_output_inner<T>(
    backend: &mut LiveScalarBackend,
    meters: &mut LiveCallbackMeters,
    output: &mut [T],
    channels: usize,
    shared: &LiveShared,
    measure_load: bool,
    measure_pressure: bool,
) where
    T: Sample + FromSample<f32>,
{
    const BLOCK: usize = 128;
    let entered_nanos = shared.clock_base.elapsed().as_nanos() as u64;
    if !tripwire::in_audio_scope() {
        shared.callback_scope_misses.fetch_add(1, Ordering::Relaxed);
    }
    let silence = T::from_sample(0.0);
    output.fill(silence);
    if channels == 0 {
        return;
    }
    let frame_count = output.len() / channels;
    // Monotonic callback frames, not CPAL's playback timestamp. See
    // `LiveScalarDevice::clock_nanos`.
    let start = shared.frames.load(Ordering::Acquire);
    let generation_at_entry = shared.generation.load(Ordering::Acquire);
    // Pointer writes only: adopt freshly decoded samples into the bank and
    // hand displaced ones back for the producer to free.
    backend.drain_sample_installs(&shared.samples);
    backend.retire_kabel_programs(&shared.samples, &shared.ring, &shared.immediate);
    backend.drain_reverb_installs(&shared.samples);
    // The producer names a chain before it ships the first insert of the
    // chain, so the names go first.
    take_named_insert_chains(backend, shared);
    backend.drain_insert_installs(&shared.samples);
    for update in shared.live_controls.drain_available() {
        backend.set_live_control(update);
    }
    for value in shared.insert_values.drain_available() {
        backend.set_insert_value(value);
    }
    let mut scratch = [0.0f32; BLOCK * 2];
    let mut rendered = 0usize;
    let mut stop_state = OUTPUT_RUNNING;
    let mut callback_late = 0u64;
    let mut callback_refused = 0u64;
    // Routing set by the producer reaches the backend at a block edge.
    let mut pairs = [0u8; crate::scalar::MAX_ORBITS];
    for (slot, pair) in pairs.iter_mut().zip(shared.orbit_pairs.iter()) {
        *slot = pair.load(Ordering::Relaxed);
    }
    backend.set_orbit_pairs(pairs);
    let pairs_available = channels / 2;
    // Asked for between blocks: silence what is sounding before this one
    // is filled, so a swapped audition stops rather than ringing on under
    // the next.
    if shared.cut_sounding.swap(false, Ordering::AcqRel) {
        backend.cut_sounding_voices(start);
    }
    while rendered < frame_count {
        let count = (frame_count - rendered).min(BLOCK);
        let end_frame = start
            .saturating_add(rendered as u64)
            .saturating_add(count as u64);
        // Publish only the chunk about to render, not the entire host
        // buffer: process_block rechecks generation at every chunk edge.
        // The completed/certified frontier below remains untouched.
        shared
            .rendering_until_frame
            .store(end_frame, Ordering::Release);
        backend.set_max_polyphony(shared.max_polyphony.load(Ordering::Relaxed));
        let visual_analysis_mask = shared.visual_analysis_mask.load(Ordering::Acquire);
        let visual_generation_floor = shared
            .visual_analysis_generation_floor
            .load(Ordering::Acquire);
        backend.set_ui_visual_capture_mask(visual_analysis_mask);
        backend.set_ui_visual_capture_generation_floor(visual_generation_floor);
        // The mixer's orbit faders, read once a block: sixteen loads.
        let mut orbit_gains = [1.0f32; crate::scalar::MAX_ORBITS];
        for (gain, slot) in orbit_gains.iter_mut().zip(shared.orbit_gains.iter()) {
            *gain = f32::from_bits(slot.load(Ordering::Relaxed));
        }
        backend.set_orbit_gains(&orbit_gains);
        // A list the producer wrote during this callback goes before the
        // inserts this block takes.
        take_named_insert_chains(backend, shared);
        // Bound admission before reading Stop. Events published after a
        // concurrent stop/resume belong to the next running block.
        let ring_end = shared.ring.published_end();
        let immediate_end = shared.immediate.published_end();
        stop_state = shared.stop.0.load(Ordering::Acquire);
        for _ in shared
            .ring
            .drain_queued_before(shared.stop_ring_end.load(Ordering::Acquire))
        {}
        for _ in shared
            .immediate
            .drain_queued_before(shared.stop_immediate_end.load(Ordering::Acquire))
        {}
        let immediate = if stop_state == OUTPUT_RUNNING {
            backend.admit_immediate(
                &shared.immediate,
                start.saturating_add(rendered as u64),
                count.min(shared.immediate.len_before(immediate_end)),
            )
        } else {
            crate::LiveBlockReport::default()
        };
        let mut report = backend.process_block_with_events(
            &mut scratch[..count * 2],
            count,
            start.saturating_add(rendered as u64),
            shared.ring.drain_queued_before(ring_end),
            shared.flip_atomics(),
            stop_state != OUTPUT_RUNNING,
            Some(&shared.samples),
        );
        if backend.intake_is_running() {
            shared
                .completed_intake_frame
                .store(end_frame, Ordering::Release);
        }
        report.accepted += immediate.accepted;
        report.refused += immediate.refused;
        // Visuals follow the score independently of listening volume and
        // the output limiter.
        shared
            .analysis
            .write_stereo(&scratch[..count * 2], end_frame);
        // Metering and the device see the same post-fader signal.
        let target_gain = shared.meter.gain();
        let orbit_peaks = backend.take_orbit_peaks();
        // Silent startup callbacks can precede the saved volume. Adopt that
        // volume when the first signal reaches the main pair or a routed orbit.
        if meters.master_gain.is_none()
            && (scratch[..count * 2].iter().any(|sample| *sample != 0.0)
                || orbit_peaks.iter().any(|peak| *peak != 0.0))
        {
            meters.master_gain = Some(crate::live_control::Ramp::new(0, target_gain));
        }
        if let Some(gain_ramp) = &mut meters.master_gain
            && target_gain != gain_ramp.target()
        {
            gain_ramp.set_target(
                target_gain,
                (0.010 * meters.sample_rate as f32).round() as u32,
            );
        }
        // One 10 ms ramp serves every output pair and survives callback boundaries.
        let mut frame_gains = [0.0_f32; BLOCK];
        let mut peak_gain = 0.0_f32;
        for (frame, gain) in scratch[..count * 2]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(&mut frame_gains[..count])
        {
            *gain = meters
                .master_gain
                .as_mut()
                .map_or(target_gain, crate::live_control::Ramp::next);
            peak_gain = peak_gain.max(*gain);
            frame[0] *= *gain;
            frame[1] *= *gain;
        }
        // What the meter would have said before the limiter touched it. CLIP
        // means "this score is asking for more than full scale", and a ceiling
        // below full scale would otherwise make it permanently dark and turn
        // the indicator into a lie.
        shared.meter.observe_unlimited(&scratch[..count * 2]);
        // The safety limiter, between the fader and every consumer of the
        // output buffer: the meter and the recorder tap both see
        // what the device is about to be handed.
        let reduction = meters.limiter.process_stereo(
            &mut scratch[..count * 2],
            shared.meter.limiter_settings(),
            shared.meter.limiter_makeup(),
        );
        shared.meter.publish_reduction(reduction);
        meters
            .master
            .observe_stereo(&scratch[..count * 2], &shared.meter);
        for (slot, peak) in shared.orbit_peaks.iter().zip(orbit_peaks) {
            shared
                .score_peak
                .fetch_max(peak.to_bits(), Ordering::Relaxed);
            // Non-negative floats order as their bits do.
            slot.fetch_max((peak * peak_gain).to_bits(), Ordering::Relaxed);
        }
        shared.record.write_stereo(&scratch[..count * 2]);
        let visual_generation_current = backend.generation()
            == shared.generation.load(Ordering::Acquire)
            && visual_generation_floor
                == shared
                    .visual_analysis_generation_floor
                    .load(Ordering::Acquire);
        let current_visual_mask = shared.visual_analysis_mask.load(Ordering::Acquire);
        let mut slots = visual_analysis_mask;
        while slots != 0 {
            let slot = slots.trailing_zeros() as usize;
            if visual_generation_current
                && current_visual_mask & (1_u64 << slot) != 0
                && let Some(tap) = shared.visual_analysis_tap(slot)
            {
                tap.write_stereo(backend.ui_visual_mix(slot, count), end_frame);
            }
            slots &= slots - 1;
        }
        shared
            .refused_voices
            .fetch_add(report.refused as u64, Ordering::Relaxed);
        shared
            .accepted_events
            .fetch_add(report.accepted as u64, Ordering::Relaxed);
        shared
            .stale_events
            .fetch_add(report.stale as u64, Ordering::Relaxed);
        shared
            .late_events
            .fetch_add(report.late as u64, Ordering::Relaxed);
        callback_late = callback_late.saturating_add(report.late as u64);
        callback_refused = callback_refused.saturating_add(report.refused as u64);
        // Route whenever any orbit is sent off the main pair; a pair the
        // device does not have falls back to the main pair rather than
        // vanishing (which it did when this was gated on pairs_available).
        let routed = backend.routing_active();
        for frame in 0..count {
            let output_frame =
                &mut output[(rendered + frame) * channels..(rendered + frame + 1) * channels];
            let left = scratch[frame * 2];
            let right = scratch[frame * 2 + 1];
            if channels == 1 {
                output_frame[0] = T::from_sample((left + right) * 0.5);
            } else if routed {
                // Orbits go to their pairs; a pair nobody is routed to is
                // silent, and the main pair carries the rest. Summed in f32
                // first, on the stack: the callback allocates nothing.
                let mut mixed = [0.0f32; MAX_OUTPUT_CHANNELS];
                mixed[0] = left;
                mixed[1] = right;
                for (orbit, pair) in pairs.iter().enumerate() {
                    let pair = usize::from(*pair);
                    if pair == 0 {
                        continue;
                    }
                    // A pair the device does not have is the main pair, so
                    // the orbit is heard rather than dropped.
                    let dest = if pair >= pairs_available || pair * 2 + 1 >= MAX_OUTPUT_CHANNELS {
                        0
                    } else {
                        pair
                    };
                    let mix = backend.orbit_mix(orbit, count);
                    if mix.len() < (frame + 1) * 2 {
                        continue;
                    }
                    mixed[dest * 2] += mix[frame * 2] * frame_gains[frame];
                    mixed[dest * 2 + 1] += mix[frame * 2 + 1] * frame_gains[frame];
                }
                for (channel, sample) in output_frame.iter_mut().enumerate() {
                    *sample = T::from_sample(mixed.get(channel).copied().unwrap_or(0.0));
                }
            } else {
                output_frame[0] = T::from_sample(left);
                output_frame[1] = T::from_sample(right);
                let center = T::from_sample((left + right) * 0.5);
                for sample in &mut output_frame[2..] {
                    *sample = center;
                }
            }
        }
        // DSP is not consumption: only this completed host-slice copy can
        // certify the actual generation selected for the sub-block.
        backend.confirmation_copied(start.saturating_add(rendered as u64), end_frame);
        rendered += count;
        // The gate of the plugin workers opens when the time so far passes
        // 80% of the time of the frames so far. So a heavy start posts its
        // jobs from its next sub-block.
        backend.end_pool_sub_block(rendered, || {
            (shared.clock_base.elapsed().as_nanos() as u64).saturating_sub(entered_nanos)
        });
    }
    shared
        .frames
        .fetch_add(frame_count as u64, Ordering::Release);
    shared.callbacks.fetch_add(1, Ordering::Release);
    shared
        .score_sources_active
        .store(backend.score_sources_active(), Ordering::Release);
    // Callback timing: the largest gap between completions and the longest
    // elapsed callback time. Both include scheduling effects. Reading the
    // clock and publishing atomics allocate nothing inside the callback.
    let now_nanos = shared.clock_base.elapsed().as_nanos() as u64;
    let busy_nanos = now_nanos.saturating_sub(entered_nanos);
    // The plugin workers park until the next buffer, the load of this one
    // moves their gate, and the figures of its jobs go to the report.
    let plugins = backend.end_pool_buffer(frame_count, busy_nanos);
    if let Some(plugins) = plugins {
        shared
            .plugin_jobs_inline
            .fetch_add(plugins.inline_jobs, Ordering::Relaxed);
        shared
            .plugin_join_wait_nanos
            .fetch_add(plugins.join_wait_nanos, Ordering::Relaxed);
        shared
            .plugin_late_jobs
            .fetch_add(plugins.late_jobs, Ordering::Relaxed);
    }
    if measure_load {
        meters.load.observe(
            frame_count,
            busy_nanos,
            plugins.unwrap_or_default(),
            meters.sample_rate,
            &shared.realtime_load,
        );
    }
    if measure_pressure {
        meters.pressure.observe(
            backend.pressure_observation(),
            callback_late,
            callback_refused,
            &shared.realtime_pressure,
        );
    }
    shared
        .max_callback_busy_nanos
        .fetch_max(busy_nanos, Ordering::AcqRel);
    let previous = shared.last_callback_nanos.swap(now_nanos, Ordering::AcqRel);
    if previous != 0 {
        let gap = now_nanos.saturating_sub(previous);
        shared
            .max_callback_gap_nanos
            .fetch_max(gap, Ordering::AcqRel);
    }
    if shared.generation.load(Ordering::Acquire) != generation_at_entry {
        shared.cutover_race_blocks.fetch_add(1, Ordering::Relaxed);
    }
    if backend.stop_ramp_complete() {
        shared.stop.complete(stop_state);
    }
}

/// Takes a new list of named chains, when the producer does not write it
/// now: see [`LiveScalarDevice::set_named_insert_chains`].
fn take_named_insert_chains(backend: &mut LiveScalarBackend, shared: &LiveShared) {
    let list = shared.insert_named_list.load(Ordering::Acquire);
    if list == backend.named_insert_list() || !list.is_multiple_of(2) {
        return;
    }
    let mut named = [u64::MAX; crate::insert::MAX_INSERT_BUSES];
    for (name, slot) in named.iter_mut().zip(shared.insert_named.iter()) {
        *name = slot.load(Ordering::Acquire);
    }
    if shared.insert_named_list.load(Ordering::Acquire) == list {
        backend.set_named_insert_chains(list, &named);
    }
}

pub(super) fn write_output<T>(
    backend: &mut ScalarBackend,
    output: &mut [T],
    channels: usize,
    total_frames: u64,
    written: &AtomicU64,
) where
    T: Sample + FromSample<f32>,
{
    const BLOCK: usize = 128;
    let silence = T::from_sample(0.0);
    output.fill(silence);
    if channels == 0 {
        return;
    }

    let start = written.load(Ordering::Relaxed);
    let available = total_frames.saturating_sub(start) as usize;
    let frames = (output.len() / channels).min(available);
    let mut scratch = [0.0f32; BLOCK * 2];
    let mut rendered = 0usize;
    while rendered < frames {
        let count = (frames - rendered).min(BLOCK);
        backend.notify_pending_inserts_early(count);
        backend.process_block(&mut scratch[..count * 2], count);
        for frame in 0..count {
            let output_frame =
                &mut output[(rendered + frame) * channels..(rendered + frame + 1) * channels];
            let left = scratch[frame * 2];
            let right = scratch[frame * 2 + 1];
            if channels == 1 {
                output_frame[0] = T::from_sample((left + right) * 0.5);
            } else {
                output_frame[0] = T::from_sample(left);
                output_frame[1] = T::from_sample(right);
                let center = T::from_sample((left + right) * 0.5);
                for sample in &mut output_frame[2..] {
                    *sample = center;
                }
            }
        }
        rendered += count;
    }
    written.fetch_add(frames as u64, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;
    mod callback_gates {
        use super::*;

        const GATE_BENCH_CHUNK_CALLBACKS: usize = 8;

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum ReceiptBenchMode {
            Disabled,
            Empty,
            Window,
        }

        impl ReceiptBenchMode {
            fn label(self) -> &'static str {
                match self {
                    Self::Disabled => "no-consumer",
                    Self::Empty => "armed-empty-ledger",
                    Self::Window => "zero-event-window-per-chunk",
                }
            }
        }

        struct GateBenchState {
            shared: LiveShared,
            backend: LiveScalarBackend,
            meters: LiveCallbackMeters,
            gate: CallbackGate,
        }

        impl GateBenchState {
            fn new(frames: usize, callbacks: usize) -> Self {
                Self::with_capacity(frames, callbacks, 4)
            }

            fn with_capacity(frames: usize, callbacks: usize, voice_capacity: usize) -> Self {
                let shared = LiveShared::new(1);
                // Prepare only the fixture's capacity, not the full device's
                // thousands of voice/FX states.
                let mut backend = LiveScalarBackend::with_dispatch(
                    48_000,
                    voice_capacity,
                    DspDispatch::portable(),
                )
                .expect("portable backend");
                backend.set_input(Some(Arc::clone(&shared.input)));
                assert!(backend.dispatch().is_forced_portable());
                let duration_secs =
                    frames.checked_mul(callbacks).expect("frame count") as f32 / 48_000.0 + 1.0;
                assert!(shared.ring.push(AudioEvent {
                    onset_id: 1,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.5,
                    duration_secs,
                    ui_visuals: 0,
                    controls: crate::OscillatorControls {
                        envelope: crate::Envelope {
                            attack_secs: 0.001,
                            decay_secs: 0.0,
                            sustain: 1.0,
                            release_secs: 0.01,
                        },
                        ..Default::default()
                    },
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
                let meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
                let gate = CallbackGate::new();
                gate.activate().expect("activate before timing");
                Self {
                    shared,
                    backend,
                    meters,
                    gate,
                }
            }

            fn arm_receipts(&mut self, mode: ReceiptBenchMode) {
                if mode != ReceiptBenchMode::Disabled {
                    self.backend
                        .set_confirmations(self.shared.confirmations.clone());
                }
            }

            fn offer_receipt_chunk(
                &self,
                mode: ReceiptBenchMode,
                chunk: usize,
                frames: usize,
            ) -> Option<crate::confirmation::WindowOffer> {
                if mode != ReceiptBenchMode::Window {
                    return None;
                }
                let start = self.shared.frames.load(Ordering::Acquire);
                let end = start
                    .checked_add(
                        u64::try_from(
                            frames
                                .checked_mul(GATE_BENCH_CHUNK_CALLBACKS)
                                .expect("chunk frames"),
                        )
                        .expect("bounded frame count"),
                    )
                    .expect("window end");
                let token = u64::try_from(chunk)
                    .expect("chunk token")
                    .checked_add(1)
                    .expect("next token");
                let mut offer =
                    confirmation_offer(&self.shared.confirmations, token, 1, start, end, 0);
                offer.takeover_frame = 0;
                assert!(self.shared.confirmations.publish(offer));
                Some(offer)
            }

            fn collect_receipt_chunk(
                &self,
                offered: Option<crate::confirmation::WindowOffer>,
                frames: usize,
            ) {
                if let Some(offer) = offered {
                    let terminal = self
                        .shared
                        .confirmations
                        .pop_terminal()
                        .expect("offered chunk copied");
                    assert_eq!(terminal.key, offer.key);
                    assert_eq!(terminal.generation, offer.generation);
                    assert_eq!(terminal.takeover_frame, offer.takeover_frame);
                    assert_eq!(
                        terminal.outcome,
                        crate::confirmation::WindowOutcome::Confirmed
                    );
                    assert_eq!(terminal.copied_start_frame, offer.start_frame);
                    assert_eq!(
                        terminal.copied_end_frame,
                        offer.start_frame + frames.min(128) as u64
                    );
                }
                assert!(self.shared.confirmations.pop_terminal().is_none());
            }

            fn time_chunk(&mut self, output: &mut [f32], frames: usize, gated: bool) -> u128 {
                // Choose the arm outside timing. Both enter exactly one audio scope
                // and run the same metered callback body and host-buffer copy.
                let gate = std::hint::black_box(&self.gate);
                if gated {
                    let started = Instant::now();
                    for output in output.chunks_exact_mut(frames * 2) {
                        gate.render_or_silence(output, |output| {
                            write_live_output(
                                &mut self.backend,
                                &mut self.meters,
                                output,
                                2,
                                &self.shared,
                            );
                        });
                    }
                    started.elapsed().as_nanos()
                } else {
                    let started = Instant::now();
                    for output in output.chunks_exact_mut(frames * 2) {
                        tripwire::audio_scope(|| {
                            write_live_output(
                                &mut self.backend,
                                &mut self.meters,
                                output,
                                2,
                                &self.shared,
                            );
                        });
                    }
                    started.elapsed().as_nanos()
                }
            }

            fn check(&self, frames: usize, callbacks: usize) {
                self.check_counts(frames, callbacks, 1);
                assert_eq!(self.backend.pressure_observation().active_voices, 1);
            }

            fn check_counts(&self, frames: usize, callbacks: usize, accepted_events: usize) {
                let report = self.shared.report(1);
                assert_eq!(report.callbacks, callbacks as u64);
                assert_eq!(report.submitted_frames, (frames * callbacks) as u64);
                assert_eq!(report.accepted_events, accepted_events as u64);
                assert_eq!(report.ring_depth, 0);
                assert_eq!(report.ring_role_conflicts, 0);
                assert_eq!(report.ring_refusals, 0);
                assert_eq!(report.refused_voices, 0);
                assert_eq!(report.stale_events_filtered, 0);
                assert_eq!(report.late_events, 0);
                assert_eq!(report.callback_errors, 0);
                assert_eq!(report.callback_scope_misses, 0);
                assert!(!self.shared.failed.load(Ordering::Acquire));
                assert!(!self.shared.stop.is_stopped());
                assert_eq!(self.backend.pressure_observation().voice_ceiling_drops, 0);
            }
        }

        fn validate_callback_chunk(
            a_output: &[f32],
            b_output: &[f32],
            chunk: usize,
            mut hash: Option<&mut u64>,
        ) -> f32 {
            assert_eq!(a_output.len(), b_output.len());
            let mut peak = 0.0f32;
            for (a, b) in a_output.iter().zip(b_output.iter()) {
                assert!(a.is_finite() && b.is_finite());
                assert_eq!(a.to_bits(), b.to_bits(), "PCM mismatch in chunk {chunk}");
                peak = peak.max(a.abs());
                if let Some(hash) = hash.as_deref_mut() {
                    for byte in a.to_bits().to_le_bytes() {
                        *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            assert!(peak > 1e-6, "silent chunk {chunk}");
            peak
        }

        fn callback_gate_pair(
            frames: usize,
            gated_b: bool,
            reverse: bool,
            warmup_chunks: usize,
            measured_chunks: usize,
        ) -> ([u128; 2], u64, f32, u64) {
            callback_gate_receipt_pair(
                frames,
                [false, gated_b],
                [ReceiptBenchMode::Disabled; 2],
                reverse,
                warmup_chunks,
                measured_chunks,
            )
        }

        fn callback_gate_receipt_pair(
            frames: usize,
            gated: [bool; 2],
            receipts: [ReceiptBenchMode; 2],
            reverse: bool,
            warmup_chunks: usize,
            measured_chunks: usize,
        ) -> ([u128; 2], u64, f32, u64) {
            assert!(matches!(frames, 128 | 513));
            assert!(measured_chunks > 0);
            let chunks = warmup_chunks
                .checked_add(measured_chunks)
                .expect("chunk count");
            let callbacks = chunks
                .checked_mul(GATE_BENCH_CHUNK_CALLBACKS)
                .expect("callback count");
            let mut a = GateBenchState::new(frames, callbacks);
            let mut b = GateBenchState::new(frames, callbacks);
            a.arm_receipts(receipts[0]);
            b.arm_receipts(receipts[1]);
            // Only two prepared states and 65,664 bytes of PCM scratch coexist.
            // Validation is outside each eight-callback timer, not retained history.
            let mut buffers = [[0.0f32; GATE_BENCH_CHUNK_CALLBACKS * 513 * 2]; 2];
            let [a_output, b_output] = &mut buffers;
            let samples = GATE_BENCH_CHUNK_CALLBACKS * frames * 2;
            let (a_output, b_output) = (&mut a_output[..samples], &mut b_output[..samples]);
            let before = tripwire::Violations::capture();
            let scopes = tripwire::scope_entries();
            let mut elapsed = [0u128; 2];
            let mut hash = 0xcbf29ce484222325u64;
            let mut peak = 0.0f32;
            for chunk in 0..chunks {
                // Producer publication and collection never enter either timer.
                let a_offer = a.offer_receipt_chunk(receipts[0], chunk, frames);
                let b_offer = b.offer_receipt_chunk(receipts[1], chunk, frames);
                let (a_nanos, b_nanos) = if reverse {
                    let b_nanos = b.time_chunk(b_output, frames, gated[1]);
                    (a.time_chunk(a_output, frames, gated[0]), b_nanos)
                } else {
                    let a_nanos = a.time_chunk(a_output, frames, gated[0]);
                    (a_nanos, b.time_chunk(b_output, frames, gated[1]))
                };
                if receipts[0] != ReceiptBenchMode::Disabled {
                    a.collect_receipt_chunk(a_offer, frames);
                }
                if receipts[1] != ReceiptBenchMode::Disabled {
                    b.collect_receipt_chunk(b_offer, frames);
                }
                let chunk_peak = validate_callback_chunk(
                    a_output,
                    b_output,
                    chunk,
                    if chunk >= warmup_chunks {
                        Some(&mut hash)
                    } else {
                        None
                    },
                );
                if chunk >= warmup_chunks {
                    elapsed[0] = elapsed[0].checked_add(a_nanos).expect("elapsed time");
                    elapsed[1] = elapsed[1].checked_add(b_nanos).expect("elapsed time");
                    peak = peak.max(chunk_peak);
                }
            }
            assert!(tripwire::Violations::capture().since(before).clean());
            let observed_scopes = tripwire::scope_entries().wrapping_sub(scopes);
            // Other ordinary tests can enter this process-global scope counter.
            assert!(observed_scopes >= (callbacks * 2) as u64);
            assert!(elapsed.iter().all(|nanos| *nanos > 0));
            a.check(frames, callbacks);
            b.check(frames, callbacks);
            (elapsed, hash, peak, observed_scopes)
        }

        #[test]
        fn active_callback_gate_preserves_complete_pcm_and_allocation_contract() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            for frames in [128, 513] {
                callback_gate_pair(frames, true, false, 1, 2);
            }
        }

        #[test]
        fn callback_receipts_preserve_complete_pcm_and_allocation_contract() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            for frames in [128, 513] {
                for mode in [ReceiptBenchMode::Empty, ReceiptBenchMode::Window] {
                    callback_gate_receipt_pair(
                        frames,
                        [true; 2],
                        [ReceiptBenchMode::Disabled, mode],
                        false,
                        1,
                        2,
                    );
                }
            }
        }

        const DENSE_RECEIPT_WINDOWS: usize = 4;
        const DENSE_ONSETS_PER_CALLBACK: usize = 16;
        const DENSE_VOICE_CAPACITY: usize = 256;

        fn callback_dense_receipt_pair(
            frames: usize,
            converted_per_window: usize,
            tracked_b: bool,
            reverse: bool,
            warmup_chunks: usize,
        ) -> ([u128; 2], u64, f32, u64) {
            assert!(matches!(frames, 128 | 513));
            assert!(
                (1..=crate::confirmation::MAX_CONFIRMATION_ORDINALS)
                    .contains(&converted_per_window)
            );
            let onsets_per_chunk = DENSE_ONSETS_PER_CALLBACK * GATE_BENCH_CHUNK_CALLBACKS;
            let measured_onsets = DENSE_RECEIPT_WINDOWS * converted_per_window;
            assert!(measured_onsets.is_multiple_of(onsets_per_chunk));
            let measured_chunks = measured_onsets / onsets_per_chunk;
            let chunks = warmup_chunks
                .checked_add(measured_chunks)
                .expect("dense chunks");
            let callbacks = chunks * GATE_BENCH_CHUNK_CALLBACKS;
            let mut a = GateBenchState::with_capacity(frames, callbacks, DENSE_VOICE_CAPACITY);
            let mut b = GateBenchState::with_capacity(frames, callbacks, DENSE_VOICE_CAPACITY);
            if tracked_b {
                b.arm_receipts(ReceiptBenchMode::Empty);
            }
            let start = (warmup_chunks * GATE_BENCH_CHUNK_CALLBACKS * frames) as u64;
            let end = (callbacks * frames) as u64;
            let offers: [_; DENSE_RECEIPT_WINDOWS] = std::array::from_fn(|window| {
                let mut offer = confirmation_offer(
                    &b.shared.confirmations,
                    window as u64 + 1,
                    1,
                    start,
                    end,
                    converted_per_window as u32,
                );
                offer.takeover_frame = 0;
                offer
            });
            let mut buffers = [[0.0f32; GATE_BENCH_CHUNK_CALLBACKS * 513 * 2]; 2];
            let [a_output, b_output] = &mut buffers;
            let samples = GATE_BENCH_CHUNK_CALLBACKS * frames * 2;
            let (a_output, b_output) = (&mut a_output[..samples], &mut b_output[..samples]);
            let before = tripwire::Violations::capture();
            let scopes = tripwire::scope_entries();
            let mut elapsed = [0u128; 2];
            let mut hash = 0xcbf29ce484222325u64;
            let mut peak = 0.0f32;
            let mut confirmed = [false; DENSE_RECEIPT_WINDOWS];
            for chunk in 0..chunks {
                let measured = chunk >= warmup_chunks;
                if chunk == warmup_chunks && tracked_b {
                    for offer in offers {
                        assert!(b.shared.confirmations.publish(offer));
                    }
                }
                // One bounded producer refill, outside timing. Both arms receive
                // exactly the same onset order; only B's envelope can carry tags.
                // Short native voices keep active overlap below semantic polyphony,
                // and 128 queued onsets plus their tails fit the prepared 256 slots.
                for local in 0..onsets_per_chunk {
                    let global = chunk * onsets_per_chunk + local;
                    let within_window =
                        chunk.saturating_sub(warmup_chunks) * onsets_per_chunk + local;
                    let target = ((chunk * GATE_BENCH_CHUNK_CALLBACKS
                        + local / DENSE_ONSETS_PER_CALLBACK)
                        * frames
                        + (local % DENSE_ONSETS_PER_CALLBACK) * frames / DENSE_ONSETS_PER_CALLBACK)
                        as u64;
                    let mut queued = confirmation_event(
                        offers[within_window % DENSE_RECEIPT_WINDOWS],
                        (within_window / DENSE_RECEIPT_WINDOWS) as u32,
                        target,
                    );
                    queued.event.onset_id = global as u64 + 2;
                    queued.event.freq_hz = 220.0 + (global % 5) as f32 * 55.0;
                    queued.event.gain = 0.01;
                    queued.event.duration_secs = 0.0005;
                    queued.event.controls.envelope = crate::Envelope {
                        attack_secs: 0.0001,
                        decay_secs: 0.0,
                        sustain: 1.0,
                        release_secs: 0.0001,
                    };
                    assert!(a.shared.ring.push(queued.event.clone()));
                    if !tracked_b || !measured {
                        queued.confirmation = None;
                    }
                    assert!(b.shared.ring.push_queued(queued));
                }
                let (a_nanos, b_nanos) = if reverse {
                    let b_nanos = b.time_chunk(b_output, frames, true);
                    (a.time_chunk(a_output, frames, true), b_nanos)
                } else {
                    let a_nanos = a.time_chunk(a_output, frames, true);
                    (a_nanos, b.time_chunk(b_output, frames, true))
                };
                let chunk_peak = validate_callback_chunk(
                    a_output,
                    b_output,
                    chunk,
                    if measured { Some(&mut hash) } else { None },
                );
                if measured {
                    elapsed[0] = elapsed[0].checked_add(a_nanos).expect("dense elapsed");
                    elapsed[1] = elapsed[1].checked_add(b_nanos).expect("dense elapsed");
                    peak = peak.max(chunk_peak);
                }
                for _ in 0..DENSE_RECEIPT_WINDOWS {
                    let Some(terminal) = b.shared.confirmations.pop_terminal() else {
                        break;
                    };
                    assert!(tracked_b);
                    assert_eq!(
                        chunk + 1,
                        chunks,
                        "a complete dense window cannot finish early"
                    );
                    let slot = offers
                        .iter()
                        .position(|offer| offer.key == terminal.key)
                        .expect("offered key");
                    assert!(!confirmed[slot], "duplicate dense terminal");
                    confirmed[slot] = true;
                    assert_eq!(terminal.generation, 1);
                    assert_eq!(terminal.takeover_frame, 0);
                    assert_eq!(
                        terminal.outcome,
                        crate::confirmation::WindowOutcome::Confirmed
                    );
                    assert!(
                        terminal.copied_start_frame >= start && terminal.copied_end_frame <= end
                    );
                    assert!(terminal.copied_start_frame < terminal.copied_end_frame);
                }
                assert!(b.shared.confirmations.pop_terminal().is_none());
            }
            assert_eq!(confirmed, [tracked_b; DENSE_RECEIPT_WINDOWS]);
            assert!(tripwire::Violations::capture().since(before).clean());
            let observed_scopes = tripwire::scope_entries().wrapping_sub(scopes);
            assert!(observed_scopes >= (callbacks * 2) as u64);
            assert!(elapsed.iter().all(|nanos| *nanos > 0));
            for state in [&a, &b] {
                state.check_counts(frames, callbacks, callbacks * DENSE_ONSETS_PER_CALLBACK + 1);
                assert_eq!(
                    state
                        .backend
                        .pressure_observation()
                        .semantic_polyphony_fades,
                    0
                );
            }
            assert_eq!(
                a.backend.pressure_observation().active_voices,
                b.backend.pressure_observation().active_voices
            );
            (elapsed, hash, peak, observed_scopes)
        }

        #[test]
        fn callback_dense_receipts_preserve_pcm_and_confirm_every_onset() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            for frames in [128, 513] {
                callback_dense_receipt_pair(frames, 32, true, false, 1);
            }
        }

        /// Four full finite ledgers with actual interleaved onset admission,
        /// activation and host copy, compared with identical untracked audio.
        /// Run alone with `--release --exact --ignored --test-threads=1`.
        #[test]
        #[ignore = "manual release benchmark"]
        #[allow(
            clippy::assertions_on_constants,
            reason = "reject debug invocation at runtime, not compilation"
        )]
        fn callback_dense_consumer_confirmation_overhead_report() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(
                !cfg!(debug_assertions),
                "run this manual benchmark with --release"
            );
            assert!(tripwire::allocator_is_armed());
            const CONVERTED_PER_WINDOW: usize = 4096;
            const WARMUP_CHUNKS: usize = 4;
            const PAIRS: usize = 4;
            let callbacks =
                DENSE_RECEIPT_WINDOWS * CONVERTED_PER_WINDOW / DENSE_ONSETS_PER_CALLBACK;
            for frames in [128, 513] {
                let mut reference_hash = None;
                for tracked_b in [false, true] {
                    for repetition in 0..PAIRS {
                        let reverse = repetition % 2 == 1;
                        let (nanos, hash, peak, observed_scopes) = callback_dense_receipt_pair(
                            frames,
                            CONVERTED_PER_WINDOW,
                            tracked_b,
                            reverse,
                            WARMUP_CHUNKS,
                        );
                        assert_eq!(
                            observed_scopes,
                            ((callbacks + WARMUP_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS) * 2) as u64
                        );
                        assert_eq!(*reference_hash.get_or_insert(hash), hash);
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1, "benchmark": "callback-dense-consumer-confirmation",
                                "measurement": "correctness-metadata-overhead",
                                "timing_scope": "eight-callback-chunks", "callback_path": "active-gate-full-callback",
                                "dispatch": "portable-engine-kernels", "sample_rate": 48_000,
                                "frames_per_callback": frames, "callbacks_per_arm": callbacks,
                                "warmup_callbacks": WARMUP_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS,
                                "voice_capacity": DENSE_VOICE_CAPACITY, "sustained_bed_voices": 1,
                                "dense_onsets_per_callback": DENSE_ONSETS_PER_CALLBACK,
                                "measured_onsets_per_arm": callbacks * DENSE_ONSETS_PER_CALLBACK,
                                "validated_samples_per_arm": callbacks * frames * 2,
                                "window_count": if tracked_b { DENSE_RECEIPT_WINDOWS } else { 0 },
                                "converted_per_window": CONVERTED_PER_WINDOW,
                                "confirmed_receipts_per_b": if tracked_b { DENSE_RECEIPT_WINDOWS } else { 0 },
                                "comparison": if tracked_b { "untracked-tagged" } else { "untracked-untracked" },
                                "producer_publication_and_collection_timed": false,
                                "repetition": repetition, "block": repetition / 2, "order": if reverse { "ba" } else { "ab" },
                                "a_nanos": nanos[0], "b_nanos": nanos[1],
                                "a_nanos_per_callback": nanos[0] as f64 / callbacks as f64,
                                "b_nanos_per_callback": nanos[1] as f64 / callbacks as f64,
                                "validation_hash": format!("{hash:016x}"), "validation_peak": peak,
                            })
                        );
                    }
                }
            }
        }

        /// Correctness-metadata overhead for the complete gated callback, not a
        /// throughput improvement or device-pacing measurement. Publication,
        /// terminal collection, validation, construction and teardown are untimed.
        /// Run alone with `--release --exact --ignored --test-threads=1`.
        #[test]
        #[ignore = "manual release benchmark"]
        #[allow(
            clippy::assertions_on_constants,
            reason = "reject debug invocation at runtime, not compilation"
        )]
        fn callback_consumer_confirmation_overhead_report() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(
                !cfg!(debug_assertions),
                "run this manual benchmark with --release"
            );
            assert!(tripwire::allocator_is_armed());
            // Match the existing whole-callback gate baseline's musical interval
            // and repetition schedule, so pre-receipt builds remain comparable.
            const WARMUP_CHUNKS: usize = 64;
            const MEASURED_CHUNKS: usize = 512;
            const PAIRS: usize = 12;
            let callbacks = MEASURED_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS;
            for frames in [128, 513] {
                let mut reference_hash = None;
                for mode in [
                    ReceiptBenchMode::Disabled,
                    ReceiptBenchMode::Empty,
                    ReceiptBenchMode::Window,
                ] {
                    for repetition in 0..PAIRS {
                        // Consecutive fresh AB and BA pairs form balanced ABBA;
                        // Disabled/Disabled supplies the same-callback A/A control.
                        let reverse = repetition % 2 == 1;
                        let (nanos, hash, peak, observed_scopes) = callback_gate_receipt_pair(
                            frames,
                            [true; 2],
                            [ReceiptBenchMode::Disabled, mode],
                            reverse,
                            WARMUP_CHUNKS,
                            MEASURED_CHUNKS,
                        );
                        assert_eq!(
                            observed_scopes,
                            ((WARMUP_CHUNKS + MEASURED_CHUNKS) * GATE_BENCH_CHUNK_CALLBACKS * 2)
                                as u64
                        );
                        assert_eq!(
                            *reference_hash.get_or_insert(hash),
                            hash,
                            "receipt modes must render the same measured window"
                        );
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1,
                                "benchmark": "callback-consumer-confirmation",
                                "measurement": "correctness-metadata-overhead",
                                "timing_scope": "eight-callback-chunks",
                                "callback_path": "active-gate-full-callback",
                                "dispatch": "portable-engine-kernels",
                                "voices": 1, "voice_capacity": 4, "sample_rate": 48_000,
                                "frames_per_callback": frames,
                                "callbacks_per_arm": callbacks,
                                "warmup_callbacks": WARMUP_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS,
                                "validated_samples_per_arm": callbacks * frames * 2,
                                "a_mode": ReceiptBenchMode::Disabled.label(), "b_mode": mode.label(),
                                "measured_offers_per_b": if mode == ReceiptBenchMode::Window { MEASURED_CHUNKS } else { 0 },
                                "total_confirmed_receipts_per_b": if mode == ReceiptBenchMode::Window { WARMUP_CHUNKS + MEASURED_CHUNKS } else { 0 },
                                "receipt_publication_and_collection_timed": false,
                                "repetition": repetition, "block": repetition / 2,
                                "order": if reverse { "ba" } else { "ab" },
                                "a_nanos": nanos[0], "b_nanos": nanos[1],
                                "a_nanos_per_callback": nanos[0] as f64 / callbacks as f64,
                                "b_nanos_per_callback": nanos[1] as f64 / callbacks as f64,
                                "validation_hash": format!("{hash:016x}"), "validation_peak": peak,
                            })
                        );
                    }
                }
            }
        }

        /// Active gate plus callback-body service time, not device pacing or
        /// per-callback percentiles. Both arms share the eight-callback validation
        /// cadence. Construction, activation and teardown are untimed.
        /// Run this test alone with `--exact --ignored --test-threads=1`.
        #[test]
        #[ignore = "manual release benchmark"]
        fn callback_active_gate_overhead_report() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            const WARMUP_CHUNKS: usize = 64;
            const MEASURED_CHUNKS: usize = 512;
            const PAIRS: usize = 12;
            let callbacks = MEASURED_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS;
            for frames in [128, 513] {
                let mut reference_hash = None;
                for gated_b in [false, true] {
                    for repetition in 0..PAIRS {
                        // Adjacent A/B and B/A pairs form a balanced ABBA block.
                        let reverse = repetition % 2 == 1;
                        let (nanos, hash, peak, observed_scopes) = callback_gate_pair(
                            frames,
                            gated_b,
                            reverse,
                            WARMUP_CHUNKS,
                            MEASURED_CHUNKS,
                        );
                        assert_eq!(
                            observed_scopes,
                            ((WARMUP_CHUNKS + MEASURED_CHUNKS) * GATE_BENCH_CHUNK_CALLBACKS * 2)
                                as u64
                        );
                        assert_eq!(
                            *reference_hash.get_or_insert(hash),
                            hash,
                            "fresh pairs must render the same measured window"
                        );
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1, "benchmark": "callback-active-gate",
                                "timing_scope": "eight-callback-chunks", "dispatch": "portable-engine-kernels",
                                "voices": 1, "voice_capacity": 4, "sample_rate": 48_000,
                                "frames_per_callback": frames, "callbacks_per_arm": callbacks,
                                "warmup_callbacks": WARMUP_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS,
                                "validated_samples_per_arm": callbacks * frames * 2,
                                "comparison": if gated_b { "direct-gated" } else { "direct-direct" },
                                "repetition": repetition, "block": repetition / 2,
                                "order": if reverse { "ba" } else { "ab" },
                                "a_nanos": nanos[0], "b_nanos": nanos[1],
                                "a_nanos_per_callback": nanos[0] as f64 / callbacks as f64,
                                "b_nanos_per_callback": nanos[1] as f64 / callbacks as f64,
                                "validation_hash": format!("{hash:016x}"), "validation_peak": peak,
                            })
                        );
                    }
                }
            }
        }

        fn time_callback_telemetry_arm(
            measure_load: bool,
            measure_pressure: bool,
            callbacks: usize,
        ) -> u128 {
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 128 * 2];
            for _ in 0..512 {
                tripwire::audio_scope(|| {
                    write_live_output_inner(
                        &mut backend,
                        &mut meters,
                        &mut output,
                        2,
                        &shared,
                        std::hint::black_box(measure_load),
                        std::hint::black_box(measure_pressure),
                    )
                });
            }
            let started = Instant::now();
            for _ in 0..callbacks {
                tripwire::audio_scope(|| {
                    write_live_output_inner(
                        &mut backend,
                        &mut meters,
                        &mut output,
                        2,
                        &shared,
                        std::hint::black_box(measure_load),
                        std::hint::black_box(measure_pressure),
                    )
                });
            }
            std::hint::black_box(output);
            started.elapsed().as_nanos()
        }

        #[test]
        #[ignore = "manual release benchmark"]
        fn callback_load_meter_overhead_report() {
            const CALLBACKS: usize = 20_000;
            for repetition in 0..15 {
                let unmetered_a = time_callback_telemetry_arm(false, false, CALLBACKS);
                let metered = time_callback_telemetry_arm(true, false, CALLBACKS);
                let unmetered_b = time_callback_telemetry_arm(false, false, CALLBACKS);
                eprintln!(
                    "{{\"benchmark\":\"callback-load-meter\",\"repetition\":{repetition},\"callbacks\":{CALLBACKS},\"unmetered_a_nanos\":{unmetered_a},\"metered_nanos\":{metered},\"unmetered_b_nanos\":{unmetered_b}}}"
                );
            }
        }

        #[test]
        #[ignore = "manual release benchmark"]
        fn callback_pressure_meter_overhead_report() {
            const CALLBACKS: usize = 20_000;
            for repetition in 0..15 {
                let load_only_a = time_callback_telemetry_arm(true, false, CALLBACKS);
                let with_pressure = time_callback_telemetry_arm(true, true, CALLBACKS);
                let load_only_b = time_callback_telemetry_arm(true, false, CALLBACKS);
                eprintln!(
                    "{{\"benchmark\":\"callback-pressure-meter\",\"repetition\":{repetition},\"callbacks\":{CALLBACKS},\"load_only_a_nanos\":{load_only_a},\"with_pressure_nanos\":{with_pressure},\"load_only_b_nanos\":{load_only_b}}}"
                );
            }
        }
    }
    mod callback_output {
        use super::*;

        #[test]
        fn callback_load_publication_is_calibrated_and_allocation_free() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 128 * 2];
            assert!(tripwire::allocator_is_armed());
            let before = tripwire::Violations::capture();
            for _ in 0..REALTIME_LOAD_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            let violations = tripwire::Violations::capture().since(before);
            assert!(
                violations.clean(),
                "load telemetry violated callback allocation contract: {violations:?}"
            );

            let load = shared.report(1).realtime_load;
            assert_eq!(load.sample_rate_hz, 48_000);
            assert_eq!(load.last_callback_frames, 128);
            assert_eq!(load.last_callback_period_nanos, 2_666_666);
            assert_eq!(load.total_callbacks, REALTIME_LOAD_PUBLISH_INTERVAL);
            assert_eq!(load.total_period_nanos, 42_666_656);
            assert!(load.max_callback_busy_nanos >= load.last_callback_busy_nanos);
            let pressure = shared.report(1).realtime_pressure;
            assert_eq!(pressure.total_callbacks, REALTIME_LOAD_PUBLISH_INTERVAL);
            assert_eq!(pressure.window_callbacks, REALTIME_LOAD_PUBLISH_INTERVAL);
        }

        #[test]
        fn stopping_an_active_pooled_voice_does_not_free_in_the_callback() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            let controls = crate::OscillatorControls {
                stretch: Some(2.0),
                ..Default::default()
            };
            let event = AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls,
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            assert!(shared.ring.push(event.clone()));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut meters = test_meter();
            let mut output = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            let pressure = backend.pressure_observation();
            assert_eq!(pressure.active_voices, 1);
            assert_eq!(shared.completed_intake_frame.load(Ordering::Acquire), 128);
            assert_eq!(
                pressure.active_pool_leases[crate::pressure::RealtimePool::Stretch.index()],
                1
            );

            assert!(tripwire::allocator_is_armed());
            let before = tripwire::Violations::capture();
            shared.request_stop();
            shared.stop.resume();
            assert!(shared.ring.push(AudioEvent {
                onset_id: 2,
                target_frame: 128,
                ..event
            }));
            // The 10 ms ramp spans four 128-frame callbacks. The last one
            // resets the voices and returns the lease.
            for _ in 0..4 {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert_eq!(shared.completed_intake_frame.load(Ordering::Acquire), 128);
                assert_eq!(shared.ring.len(), 1);
            }
            let violations = tripwire::Violations::capture().since(before);
            assert!(
                violations.clean(),
                "stop freed a pooled voice inside the callback: {violations:?}"
            );
            assert!(backend.stop_ramp_complete());
            let pressure = backend.pressure_observation();
            assert_eq!(pressure.active_voices, 0);
            assert_eq!(
                pressure.active_pool_leases[crate::pressure::RealtimePool::Stretch.index()],
                0
            );
            assert_eq!(shared.clock_frames(), 640);
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert_eq!(shared.completed_intake_frame.load(Ordering::Acquire), 768);
            assert_eq!(shared.ring.len(), 0);
            assert_eq!(backend.pressure_observation().active_voices, 1);
            assert!(tripwire::Violations::capture().since(before).clean());
        }

        #[test]
        fn callback_pressure_reports_semantic_fades_without_allocating() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            for onset_id in 0..130 {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.1,
                    duration_secs: 1.0,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
            }
            let mut backend = LiveScalarBackend::new(48_000, 256).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 128 * 2];
            let before = tripwire::Violations::capture();
            for _ in 0..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            assert!(
                tripwire::Violations::capture().since(before).clean(),
                "pressure telemetry allocated in the callback"
            );
            let pressure = shared.report(1).realtime_pressure;
            assert_eq!(pressure.active_voices, 130);
            assert_eq!(pressure.peak_active_voices, 130);
            assert_eq!(pressure.active_orbits, 1);
            assert_eq!(pressure.semantic_polyphony_fades, 2);
            assert_eq!(pressure.window_semantic_polyphony_fades, 2);
            assert_eq!(pressure.voice_ceiling_drops, 0);
        }

        #[test]
        fn callback_applies_polyphony_changes_without_allocating_or_cutting_voices() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            shared.max_polyphony.store(256, Ordering::Relaxed);
            for onset_id in 0..160 {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.001,
                    duration_secs: 2.0,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
            }
            let mut backend = LiveScalarBackend::new(48_000, 256).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 256];
            let before = tripwire::Violations::capture();
            for _ in 0..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            let raised = shared.report(1).realtime_pressure;
            assert_eq!(raised.max_polyphony, 256);
            assert_eq!(raised.active_voices, 160);
            assert_eq!(raised.semantic_polyphony_fades, 0);
            shared.max_polyphony.store(64, Ordering::Relaxed);
            for _ in 0..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            assert!(
                tripwire::Violations::capture().since(before).clean(),
                "changing polyphony must not allocate in the callback"
            );
            let lowered = shared.report(1).realtime_pressure;
            assert_eq!(lowered.max_polyphony, 64);
            assert_eq!(
                lowered.active_voices, 160,
                "retiring voices must keep their fade"
            );
            assert_eq!(lowered.semantic_polyphony_fades, 96);
            assert_eq!(lowered.voice_ceiling_drops, 0);
            assert_eq!(
                LiveShared::new(1).max_polyphony.load(Ordering::Relaxed),
                crate::MAX_POLYPHONY
            );
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn device_polyphony_is_bounded_and_prepared_outputs_keep_it() {
            let mut output = ManualLiveOutput::new(48_000, 1).expect("manual output");
            assert_eq!(output.device().max_polyphony(), crate::MAX_POLYPHONY);
            output.device().set_max_polyphony(0);
            assert_eq!(output.device().max_polyphony(), 1);
            output.device().set_max_polyphony(usize::MAX);
            assert_eq!(
                output.device().max_polyphony(),
                crate::MAX_CONFIGURABLE_POLYPHONY
            );
            output.render(&mut [0.0; 256]);
            assert_eq!(
                output.backend.max_polyphony(),
                crate::MAX_CONFIGURABLE_POLYPHONY
            );
            let replacement = PreparedLiveOutput::backend(
                &output.device.shared,
                48_000,
                LiveOutputOptions::default().with_dispatch(DspDispatch::portable()),
                &[],
            )
            .expect("replacement backend");
            assert_eq!(
                replacement.max_polyphony(),
                crate::MAX_CONFIGURABLE_POLYPHONY
            );
        }

        #[test]
        fn callback_pressure_reports_exact_degradation_and_pool_misses() {
            let shared = LiveShared::new(1);
            let controls = crate::OscillatorControls {
                reverb: Some(crate::ReverbControls {
                    wet: 0.5,
                    size_secs: 0.1,
                    fade_secs: 0.1,
                    lp_start_hz: 15_000.0,
                    lp_end_hz: 1_000.0,
                    ir: None,
                }),
                ..crate::OscillatorControls::default()
            };
            for onset_id in 0..9 {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.1,
                    duration_secs: 1.0,
                    ui_visuals: 0,
                    controls: controls.clone(),
                    sample: None,
                    synth: Some(crate::SynthSource::ZzFx {
                        params: crate::zzfx::ZzfxParams {
                            delay: 0.1,
                            sustain: 0.1,
                            release: 0.2,
                            ..crate::zzfx::ZzfxParams::default()
                        },
                    }),
                    wavetable: None,
                    cut: None,
                }));
            }
            let mut backend = LiveScalarBackend::new(48_000, 16).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 128 * 2];
            for _ in 0..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            let pressure = shared.report(1).realtime_pressure;
            assert_eq!(
                pressure.active_pool_leases(crate::RealtimePool::ZzfxDelay),
                8
            );
            assert_eq!(pressure.pool_misses(crate::RealtimePool::ZzfxDelay), 1);
            assert_eq!(
                pressure.window_pool_misses(crate::RealtimePool::ZzfxDelay),
                1
            );
            assert_eq!(pressure.orbit_reverb_misses, 9);
            assert_eq!(pressure.window_orbit_reverb_misses, 9);

            let refused_shared = LiveShared::new(1);
            let event = |onset_id, target_frame| AudioEvent {
                onset_id,
                generation: 1,
                target_frame,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.1,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            assert!(refused_shared.ring.push(event(0, 0)));
            let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &refused_shared.realtime_load);
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &refused_shared)
            });
            assert!(refused_shared.ring.push(event(1, 128)));
            for _ in 1..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &refused_shared)
                });
            }
            let pressure = refused_shared.report(1).realtime_pressure;
            assert_eq!(pressure.voice_ceiling_drops, 1);
            assert_eq!(pressure.window_voice_ceiling_drops, 1);
            assert_eq!(pressure.refused_voices, 1);
            assert_eq!(pressure.window_refused_voices, 1);
        }

        #[test]
        fn live_voice_uses_the_installed_shared_orbit_reverb() {
            let shared = LiveShared::new(1);
            let installed = crate::reverb::ReverbParams {
                ir: None,
                size_secs: 0.1,
                fade_secs: 0.1,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
            };
            let channel = crate::assets::SampleChannel::new();
            let reverb = Box::into_raw(Box::new(crate::reverb::OrbitReverb::generate(
                48_000, installed,
            )));
            if let Err(rejected) = channel
                .reverb_installs
                .push(crate::assets::ReverbInstall { orbit: 0, reverb })
            {
                // SAFETY: the empty test ring refused before ownership moved.
                drop(unsafe { Box::from_raw(rejected.reverb) });
                panic!("reverb install was refused");
            }

            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            backend.drain_reverb_installs(&channel);
            let controls = crate::OscillatorControls {
                orbit: 0,
                reverb: Some(crate::ReverbControls {
                    wet: 1.0,
                    size_secs: 0.2,
                    fade_secs: 0.1,
                    lp_start_hz: 15_000.0,
                    lp_end_hz: 1_000.0,
                    ir: None,
                }),
                dry: Some(0.0),
                ..Default::default()
            };
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 0.1,
                ui_visuals: 0,
                controls,
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));

            let mut meters = test_meter();
            let mut output = [0.0f32; 128 * 2];
            let mut peak = 0.0f32;
            for _ in 0..8 {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared);
                peak = output
                    .iter()
                    .fold(peak, |current, sample| current.max(sample.abs()));
            }
            let pressure = backend.pressure_observation();
            assert_eq!(pressure.active_orbit_reverbs, 1);
            assert_eq!(pressure.orbit_reverb_misses, 0);
            assert!(peak > 0.0, "the shared reverb bus stayed silent");
        }

        #[test]
        fn callback_writer_is_non_silent_and_stops_at_the_exact_frame() {
            let mut backend = ScalarBackend::new();
            backend.init(48_000).unwrap();
            backend.note(OnsetEvent::new(0, 440.0, 0.8, 0.1));
            let written = AtomicU64::new(0);
            let mut first = [0.0f32; 128 * 2];
            write_output(&mut backend, &mut first, 2, 130, &written);
            assert_eq!(written.load(Ordering::Acquire), 128);
            assert!(first.iter().any(|sample| sample.abs() > 1e-6));

            let mut last = [1.0f32; 128 * 2];
            write_output(&mut backend, &mut last, 2, 130, &written);
            assert_eq!(written.load(Ordering::Acquire), 130);
            assert!(last[4..].iter().all(|sample| *sample == 0.0));
        }

        #[test]
        fn live_callback_report_counts_delivery_filtering_and_ring_pressure() {
            let shared = LiveShared::new(2);
            for event in [
                AudioEvent {
                    onset_id: 1,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.5,
                    duration_secs: 0.1,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                },
                AudioEvent {
                    onset_id: 2,
                    generation: 2,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 440.0,
                    gain: 0.5,
                    duration_secs: 0.1,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                },
            ] {
                assert!(shared.ring.push(event));
            }
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            shared
                .buffer_playback_nanos
                .store(2_000_000, Ordering::Release);
            shared
                .playback_latency_nanos
                .store(3_000_000, Ordering::Release);
            shared
                .max_playback_latency_nanos
                .store(4_000_000, Ordering::Release);
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });

            let report = shared.report(77);
            assert_eq!(report.stream_id, 77);
            assert_eq!(report.submitted_frames, 128);
            assert_eq!(report.playhead_nanos, 0);
            assert_eq!(report.buffer_playback_nanos, 2_000_000);
            assert_eq!(report.playback_latency_nanos, 3_000_000);
            assert_eq!(report.max_playback_latency_nanos, 4_000_000);
            assert_eq!(report.generation, 2);
            assert_eq!(report.callbacks, 1);
            assert_eq!(report.accepted_events, 1);
            assert_eq!(report.stale_events_filtered, 1);
            assert_eq!(report.refused_voices, 0);
            assert_eq!(report.callback_errors, 0);
            assert_eq!(report.callback_scope_misses, 0);
            assert_eq!(report.callback_allocations, 0);
            assert_eq!(report.callback_frees, 0);
            assert!(!report.stop_acknowledged);
            assert_eq!(report.ring_refusals, 0);
            assert_eq!(report.ring_role_conflicts, 0);
            assert_eq!(report.ring_depth, 0);
            assert_eq!(report.ring_capacity, LIVE_RING_CAPACITY as u64);
            assert_eq!(report.ring_peak_depth, 2);
            assert_eq!(report.asset_queues.capacity, 64);
            assert_eq!(
                report.asset_queues,
                AssetQueuePressureSnapshot {
                    capacity: 64,
                    ..AssetQueuePressureSnapshot::default()
                }
            );
            assert_eq!(report.cutover_race_blocks, 0);
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
        }

        #[test]
        fn live_dsp_writer_records_missing_callback_scope() {
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
            let mut output = [0.0f32; 2];

            write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared);
            assert_eq!(
                shared.report(1).callback_scope_misses,
                1,
                "DSP work outside audio_scope was not exposed"
            );

            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            assert_eq!(
                shared.report(1).callback_scope_misses,
                1,
                "a correctly scoped DSP call was misclassified"
            );
        }

        #[test]
        fn live_report_does_not_erase_tripwire_violations() {
            let report = apply_tripwire_report(
                LiveShared::new(1).report(1),
                tripwire::Violations {
                    allocs: 3,
                    frees: 2,
                },
            );
            assert_eq!(report.callback_allocations, 3);
            assert_eq!(report.callback_frees, 2);
        }

        /// A full voice pool refuses excess onsets and reports the refusal count.
        /// Exactly one event fits in this one-slot pool.
        #[test]
        fn refused_voices_are_counted_and_are_not_fatal() {
            // Generation must match the events, or they are filtered as stale
            // and nothing ever reaches the voice pool.
            let shared = LiveShared::new(1);
            for onset_id in 0..8u64 {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.1,
                    duration_secs: 4.0,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
            }
            // Capacity 1: the first onset takes the slot, the rest are refused.
            let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 1, &shared)
            });

            let report = shared.report(1);
            assert!(
                report.refused_voices > 0,
                "a pool of 1 took all 8 simultaneous onsets: {report:?}"
            );
            assert_eq!(
                report.accepted_events, 1,
                "exactly one onset should have fit: {report:?}"
            );
        }

        /// An output on Windows counts an underrun when its playback clock
        /// runs more than 2 buffers ahead of the frames it submitted. A gap
        /// counts once, also when a device with a long latency shows it over
        /// 10 buffers. The start, also on such a device and with a buffer of
        /// 2048 frames, a stop, a parked output in a long silence, the start
        /// of a new stream and a failed stream count none. The gaps of a set
        /// over its budget add up and count, also from the start of a stream
        /// and with a buffer longer than the device period. The clocks copy
        /// the ones a WASAPI output gives.
        #[cfg(feature = "test-support")]
        #[test]
        fn a_gap_in_the_playback_clock_counts_one_underrun() {
            /// Renders a buffer of 10 ms for each step, with the clock the
            /// step's microseconds ahead of the frames, and gives the count.
            /// The device plays each buffer `latency` microseconds after its
            /// callback.
            fn play(
                manual: &mut ManualLiveOutput,
                latency: u64,
                ahead: impl IntoIterator<Item = u64>,
            ) -> u64 {
                let frames = u64::from(manual.device().sample_rate()) / 100;
                let mut output = vec![0.0f32; frames as usize * 2];
                for ahead in ahead {
                    let buffers = manual.device().clock_frames() / frames;
                    manual.render_at(
                        &mut output,
                        buffers * 10_000_000 + ahead * 1_000,
                        latency * 1_000,
                    );
                }
                manual.device().report().callback_xruns
            }
            let options = LiveOutputOptions::default()
                .with_buffer_preference(crate::AudioBufferPreference::Frames(480));
            let mut manual =
                ManualLiveOutput::new_with_samples_and_options(48_000, 7, &[], options)
                    .expect("manual output");
            // The device plays each buffer 32 ms after its callback, and it
            // starts to play 25 ms after the first buffer.
            assert_eq!(
                play(&mut manual, 32_000, [0, 15_200, 25_200, 25_300, 25_400]),
                0
            );
            // A gap of 29 ms shows over 3 buffers. It counts once, also while
            // the clock stays ahead.
            assert_eq!(play(&mut manual, 32_000, [35_300, 43_800, 54_300]), 1);
            assert_eq!(play(&mut manual, 32_000, [54_300; 100]), 1);
            // A gap of 19 ms counts none.
            assert_eq!(play(&mut manual, 32_000, [73_300; 100]), 1);
            // After a long gap the device takes 2 buffers to play again.
            assert_eq!(
                play(&mut manual, 32_000, [133_300, 143_800, 154_300, 154_300]),
                2
            );
            // A set over its budget leaves a gap of 2 ms in each buffer. The
            // gaps add up and count.
            assert_eq!(
                play(
                    &mut manual,
                    32_000,
                    (1..=20).map(|step| 154_300 + step * 2_000)
                ),
                3
            );
            // A stop parks the output, and 100 s of silence follow. The clock
            // of the device runs 400 ppm slower than the system clock: 40 ms
            // in all.
            manual.device().stop();
            assert_eq!(
                play(
                    &mut manual,
                    32_000,
                    (0..10_000).map(|step| 194_300 + step * 4)
                ),
                3
            );
            assert!(manual.device().report().stop_acknowledged);
            manual.device().resume();
            // The base follows a clock that falls back, so a gap of 25 ms from
            // there counts.
            assert_eq!(play(&mut manual, 32_000, [224_300; 4]), 3);
            assert_eq!(play(&mut manual, 32_000, [249_300]), 4);
            // A recycle gives a new stream with a new callback. The output
            // keeps its count. The new callback reads the clock from its own
            // first buffer, so its start adds none.
            manual.underruns = PlaybackUnderruns::new(480, 48_000);
            assert_eq!(
                play(&mut manual, 32_000, [0, 15_200, 25_200, 25_300, 25_400]),
                4
            );
            assert_eq!(play(&mut manual, 32_000, [54_300]), 5);
            // A set far over its budget leaves a gap of 5 ms in each buffer,
            // so the clock never holds still. The base holds after the latency
            // and 2 buffers, and the gaps add up and count.
            assert_eq!(
                play(
                    &mut manual,
                    32_000,
                    (1..=100).map(|step| 54_300 + step * 5_000)
                ),
                15
            );
            // A stall of 40 ms that shows over 3 buffers counts once.
            assert_eq!(play(&mut manual, 32_000, [554_300; 10]), 15);
            assert_eq!(
                play(&mut manual, 32_000, [569_300, 584_300, 594_300, 594_300]),
                16
            );
            // A stream that starts in such a set counts too. The base follows
            // its clock for the latency and 2 buffers first.
            manual.underruns = PlaybackUnderruns::new(480, 48_000);
            assert_eq!(
                play(&mut manual, 32_000, (0..11).map(|step| step * 5_000)),
                17
            );
            // WASAPI calls back each 480 frames, also with a buffer of 2048
            // frames. A gap of 3 ms each second adds up and counts once in a
            // minute.
            manual.underruns = PlaybackUnderruns::new(2048, 48_000);
            assert_eq!(
                play(
                    &mut manual,
                    32_000,
                    (0..6_000).map(|step| step / 100 * 3_000)
                ),
                18
            );
            // A Bluetooth headset on a buffer of 2048 frames: the callback
            // still brings 480 frames, and the clock runs 10 ms further ahead
            // in each of its first 15 buffers. Its start counts none.
            manual.underruns = PlaybackUnderruns::new(2048, 48_000);
            assert_eq!(
                play(
                    &mut manual,
                    170_000,
                    (0..20).map(|step| step.min(15) * 10_000)
                ),
                18
            );
            // A Bluetooth headset plays each buffer 170 ms after its callback.
            // Its clock runs 10 ms further ahead in each of its first 15
            // buffers, and its start counts none. A gap of 100 ms shows over
            // 10 buffers there and counts once.
            manual.underruns = PlaybackUnderruns::new(480, 48_000);
            assert_eq!(
                play(
                    &mut manual,
                    170_000,
                    (0..20).map(|step| step.min(15) * 10_000)
                ),
                18
            );
            assert_eq!(
                play(
                    &mut manual,
                    170_000,
                    (1..=12).map(|step| 150_000 + step.min(10) * 10_000)
                ),
                19
            );
            // A change of the default device fails the stream. Windows then
            // moves the stream to the new device. The gap of that move counts
            // none.
            assert_eq!(play(&mut manual, 170_000, [250_000; 4]), 19);
            manual.device().fail_output_stream_for_test();
            assert_eq!(play(&mut manual, 170_000, [350_000; 4]), 19);
        }
    }
    mod confirmation {
        use super::*;

        #[test]
        fn confirmation_mixed_conversion_waits_for_every_due_host_copy() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(7);
            let channel = &shared.confirmations;
            let mut offer = confirmation_offer(channel, 1, 7, 0, 256, 2);
            offer.intended = 5;
            offer.refused = 1;
            offer.skipped_loading = 1;
            offer.external = 1;
            assert!(channel.publish(offer));
            assert!(shared.ring.push_queued(confirmation_event(offer, 0, 0)));
            assert!(shared.ring.push_queued(confirmation_event(offer, 1, 128)));
            let mut backend = confirmation_backend(&shared, 4);
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            assert!(
                channel.pop_terminal().is_none(),
                "pending intake is not host consumption"
            );
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            let terminal = channel.pop_terminal().expect("both due blocks copied");
            assert_eq!(terminal.key, offer.key);
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::Confirmed
            );
            assert!(channel.pop_terminal().is_none());
        }

        /// A rewind's drop horizons refuse tracked onsets through the real
        /// callback, and the confirmation ledger hears it, all inside the
        /// callback scope with nothing allocated: an outgoing onset refused by a
        /// fired line arm, and one refused by an immediate rewind's own cut
        /// horizon (its takeover set past the onset, so only the cut can refuse
        /// it), each end their window Superseded.
        #[test]
        fn rewind_drop_horizons_supersede_tracked_onsets_without_allocating() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(7);
            let channel = &shared.confirmations;
            let mut backend = confirmation_backend(&shared, 4);
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            let mut callback = |backend: &mut LiveScalarBackend| {
                let before = tripwire::Violations::capture();
                tripwire::audio_scope(|| {
                    write_live_output(backend, &mut meters, &mut output, 2, &shared)
                });
                let delta = tripwire::Violations::capture().since(before);
                assert!(delta.clean(), "a rewind callback allocated: {delta:?}");
            };

            // The line arm at 256 with its drop bit, fired by the third callback.
            shared.line_arm.store((256 << 2) | 0b11, Ordering::Release);
            callback(&mut backend); // [0, 128)
            callback(&mut backend); // [128, 256)
            let ghost = confirmation_offer(channel, 1, 7, 512, 1_024, 1);
            assert!(channel.publish(ghost));
            assert!(shared.ring.push_queued(confirmation_event(ghost, 0, 512)));
            callback(&mut backend); // [256, 384): fires, refuses the ghost
            let terminal = channel.pop_terminal().expect("the refused ghost's window");
            assert_eq!(terminal.key, ghost.key);
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::Superseded
            );

            // An immediate rewind flips 7 -> 8 at 384 with its takeover far
            // ahead: the old onset at 1_024 is behind the takeover, ahead of the
            // cut.
            let old = confirmation_offer(channel, 2, 7, 1_024, 1_536, 1);
            assert!(channel.publish(old));
            shared.takeover_frame.store(8_192, Ordering::Release);
            shared
                .takeover_cut
                .store(TakeoverCut::AtFlip as u64, Ordering::Release);
            shared.line_arm.store(0, Ordering::Release);
            shared.generation.store(8, Ordering::Release);
            assert!(shared.ring.push_queued(confirmation_event(old, 0, 1_024)));
            callback(&mut backend); // [384, 512)
            let terminal = channel.pop_terminal().expect("the cut's refused onset");
            assert_eq!(terminal.key, old.key);
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::Superseded
            );
            assert!(channel.pop_terminal().is_none());
        }

        #[test]
        fn confirmation_pending_capacity_refusal_cannot_certify_partial_audio() {
            let shared = LiveShared::new(7);
            let channel = &shared.confirmations;
            let offer = confirmation_offer(channel, 1, 7, 0, 128, 2);
            assert!(channel.publish(offer));
            assert!(shared.ring.push_queued(confirmation_event(offer, 0, 0)));
            assert!(shared.ring.push_queued(confirmation_event(offer, 1, 0)));
            let mut backend = confirmation_backend(&shared, 1);
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
            assert_eq!(
                channel.pop_terminal().expect("refused window").outcome,
                crate::confirmation::WindowOutcome::AdmissionRefused
            );
        }

        #[test]
        fn confirmation_expired_onset_cannot_certify_a_late_silent_copy() {
            let shared = LiveShared::new(7);
            shared.frames.store(48_000, Ordering::Release);
            let channel = &shared.confirmations;
            let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
            let mut queued = confirmation_event(offer, 0, 0);
            queued.event.duration_secs = 0.001;
            assert!(channel.publish(offer));
            assert!(shared.ring.push_queued(queued.clone()));
            let mut backend = confirmation_backend(&shared, 2);
            let mut meters = test_meter();
            let mut output = [0.5f32; 256];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert_eq!(shared.accepted_events.load(Ordering::Relaxed), 1);
            assert!(output.iter().all(|sample| *sample == 0.0));
            let terminal = channel.pop_terminal().expect("expired finite window");
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::CopyMissed
            );
            assert_eq!(terminal.copied_start_frame, 48_000);
        }

        #[test]
        fn confirmation_missing_sample_is_not_repaired_by_a_later_install() {
            let shared = LiveShared::new(7);
            let channel = &shared.confirmations;
            let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
            let mut queued = confirmation_event(offer, 0, 0);
            queued.event.sample = Some(crate::SampleControls {
                sample: crate::SampleId(1),
                playback_rate: 1.0,
                begin: 0.0,
                end: 1.0,
                hold: crate::SampleHold::Hap,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            });
            assert!(channel.publish(offer));
            assert!(shared.ring.push_queued(queued.clone()));
            let mut backend = confirmation_backend(&shared, 2);
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            // Leave the terminal undrained while the asset becomes ready: later
            // installation must not revise the actual missing-at-activation result.
            let sample =
                crate::DecodedSample::from_parts(48_000, 1, vec![0.5; 256]).expect("sample");
            assert!(
                shared
                    .samples
                    .installs
                    .push(crate::assets::SampleInstall {
                        id: crate::SampleId(1),
                        sample: Box::into_raw(Box::new(sample)),
                    })
                    .is_ok()
            );
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert_eq!(
                channel
                    .pop_terminal()
                    .expect("missing asset receipt")
                    .outcome,
                crate::confirmation::WindowOutcome::MissingSample
            );
            assert!(channel.pop_terminal().is_none());
        }

        #[test]
        fn confirmation_requires_the_expected_body_when_a_sample_id_is_replaced() {
            for expected_known in [false, true] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let old = crate::DecodedSample::from_parts(48_000, 1, vec![0.25; 1024])
                    .expect("old body");
                let replacement =
                    crate::DecodedSample::from_parts(48_000, 1, vec![0.5; 1024]).expect("new body");
                let replacement_identity = replacement.identity();
                assert!(
                    shared
                        .samples
                        .installs
                        .push(crate::assets::SampleInstall {
                            id: crate::SampleId(1),
                            sample: Box::into_raw(Box::new(old)),
                        })
                        .is_ok()
                );
                let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
                let mut queued = confirmation_event(offer, 0, 0);
                queued.event.sample = Some(crate::SampleControls {
                    sample: crate::SampleId(1),
                    playback_rate: 1.0,
                    begin: 0.0,
                    end: 1.0,
                    hold: crate::SampleHold::Hap,
                    muted: false,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    reversed: false,
                    nudge_secs: 0.0,
                    cut: None,
                });
                queued.expected_sample_identity = expected_known.then_some(replacement_identity);
                assert!(channel.publish(offer));
                assert!(shared.ring.push_queued(queued.clone()));
                let mut backend = confirmation_backend(&shared, 4);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert!(
                    output.iter().any(|sample| sample.abs() > 1e-6),
                    "identity refusal must not change playback policy"
                );
                // The newer publication/install can arrive after this callback's
                // entry drain. Its readiness cannot revise the old activation.
                assert!(
                    shared
                        .samples
                        .installs
                        .push(crate::assets::SampleInstall {
                            id: crate::SampleId(1),
                            sample: Box::into_raw(Box::new(replacement)),
                        })
                        .is_ok()
                );
                let next = confirmation_offer(channel, 2, 7, 128, 256, 1);
                let mut next_event = confirmation_event(next, 0, 128);
                next_event.event.sample = queued.event.sample;
                next_event.expected_sample_identity = Some(replacement_identity);
                assert!(channel.publish(next));
                assert!(shared.ring.push_queued(next_event));
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert_eq!(
                    channel
                        .pop_terminal()
                        .expect("old-body failure retained")
                        .outcome,
                    crate::confirmation::WindowOutcome::MissingSample
                );
                assert_eq!(
                    channel
                        .pop_terminal()
                        .expect("required body installed at activation")
                        .outcome,
                    crate::confirmation::WindowOutcome::Confirmed
                );
                assert!(channel.pop_terminal().is_none());
                shared.samples.reclaim();
            }
        }

        #[test]
        fn confirmation_wavetable_requires_its_expected_installed_body() {
            for installed in [false, true] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let table =
                    crate::DecodedSample::from_parts(48_000, 1, vec![0.5; 256]).expect("table");
                let expected_identity = table.identity();
                if installed {
                    assert!(
                        shared
                            .samples
                            .installs
                            .push(crate::assets::SampleInstall {
                                id: crate::SampleId(1),
                                sample: Box::into_raw(Box::new(table)),
                            })
                            .is_ok()
                    );
                }
                let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
                let mut queued = confirmation_event(offer, 0, 0);
                queued.expected_sample_identity = Some(expected_identity);
                queued.event.wavetable = Some(crate::WavetableControls {
                    table: crate::SampleId(1),
                    frame_len: 256,
                    voices: 1.0,
                    lfo_shape: 0,
                    phaserand: 0.0,
                    freqspread: 0.0,
                    panspread: 0.0,
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
                });
                assert!(channel.publish(offer));
                assert!(shared.ring.push_queued(queued.clone()));
                let mut backend = confirmation_backend(&shared, 2);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert_eq!(
                    channel
                        .pop_terminal()
                        .expect("wavetable activation evidence")
                        .outcome,
                    if installed {
                        crate::confirmation::WindowOutcome::Confirmed
                    } else {
                        crate::confirmation::WindowOutcome::MissingSample
                    }
                );
            }
        }

        #[test]
        fn confirmation_superseded_pending_and_stop_do_not_certify_silence() {
            for stop in [false, true] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let offer = confirmation_offer(channel, 1, 7, 0, 256, 1);
                assert!(channel.publish(offer));
                assert!(shared.ring.push_queued(confirmation_event(offer, 0, 128)));
                let mut backend = confirmation_backend(&shared, 2);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert!(channel.pop_terminal().is_none());
                if stop {
                    shared.request_stop();
                } else {
                    shared.takeover_frame.store(128, Ordering::Release);
                    shared.generation.store(8, Ordering::Release);
                }
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                let terminal = channel
                    .pop_terminal()
                    .expect("uncopied pending onset ended");
                assert_eq!(
                    terminal.outcome,
                    if stop {
                        crate::confirmation::WindowOutcome::Cancelled
                    } else {
                        crate::confirmation::WindowOutcome::Superseded
                    }
                );
            }
        }

        #[test]
        fn confirmation_copied_audio_and_valid_silence_survive_a_later_generation() {
            for converted in [0, 1] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let offer = confirmation_offer(channel, 1, 7, 0, 128, converted);
                assert!(channel.publish(offer));
                if converted != 0 {
                    assert!(shared.ring.push_queued(confirmation_event(offer, 0, 0)));
                }
                let mut backend = confirmation_backend(&shared, 2);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert_eq!(
                    output.iter().any(|sample| sample.abs() > 1e-6),
                    converted != 0
                );
                // The producer handles a later request before it drains B's
                // terminal. The already copied B remains valid rollback evidence.
                shared.takeover_frame.store(128, Ordering::Release);
                shared.generation.store(8, Ordering::Release);
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                let terminal = channel.pop_terminal().expect("delayed B completion");
                assert_eq!(terminal.key, offer.key);
                assert_eq!(terminal.generation, 7);
                assert_eq!(
                    terminal.outcome,
                    crate::confirmation::WindowOutcome::Confirmed
                );
                assert!(channel.pop_terminal().is_none());
            }
        }

        #[test]
        fn confirmation_intentional_mute_and_permitted_dry_fallback_qualify() {
            for muted in [false, true] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
                let mut queued = confirmation_event(offer, 0, 0);
                if muted {
                    queued.expected_sample_identity = Some(1);
                    queued.event.sample = Some(crate::SampleControls {
                        sample: crate::BUNDLED_BD_SAMPLE_ID,
                        playback_rate: 1.0,
                        begin: 0.0,
                        end: 1.0,
                        hold: crate::SampleHold::Hap,
                        muted: true,
                        loop_secs: None,
                        envelope_peak: 1.0,
                        reversed: false,
                        nudge_secs: 0.0,
                        cut: None,
                    });
                } else {
                    queued.event.controls.reverb = Some(crate::ReverbControls {
                        wet: 0.5,
                        size_secs: 0.1,
                        fade_secs: 0.1,
                        lp_start_hz: 15_000.0,
                        lp_end_hz: 1_000.0,
                        ir: None,
                    });
                }
                assert!(channel.publish(offer));
                assert!(shared.ring.push_queued(queued.clone()));
                let mut backend = confirmation_backend(&shared, 2);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert_eq!(output.iter().any(|sample| sample.abs() > 1e-6), !muted);
                assert_eq!(
                    channel
                        .pop_terminal()
                        .expect("permitted disposition copied")
                        .outcome,
                    crate::confirmation::WindowOutcome::Confirmed
                );
            }
        }
    }
    mod immediate_piano {
        use super::*;

        #[test]
        fn immediate_piano_bypasses_score_lookahead_and_preserves_score_pcm() {
            assert_immediate_piano_mix(false);
        }

        #[test]
        fn immediate_piano_shares_a_sample_with_the_score_without_sharing_voices() {
            assert_immediate_piano_mix(true);
        }

        #[test]
        fn immediate_piano_is_independent_of_score_orbit_controls() {
            let _tripwire_guard = tripwire_assertion_guard();
            for channels in [1, 2] {
                for control in ["mute", "duck", "djf", "channels", "output pair"] {
                    let shared = LiveShared::new(1);
                    let expected_shared = LiveShared::new(1);
                    // Keep both devices in explicit multi-pair mode, even when
                    // the score itself stays on the main pair.
                    shared.orbit_pairs[2].store(1, Ordering::Relaxed);
                    expected_shared.orbit_pairs[2].store(1, Ordering::Relaxed);
                    let mut backend = LiveScalarBackend::new(48_000, 8).unwrap();
                    let mut expected_backend = LiveScalarBackend::new(48_000, 8).unwrap();
                    for target in [&mut backend, &mut expected_backend] {
                        let pcm = crate::DecodedSample::from_parts(
                            48_000,
                            channels,
                            vec![0.25; 48_000 * usize::from(channels)],
                        )
                        .unwrap();
                        assert!(
                            target
                                .install_prepared_sample(crate::SampleId(1), Box::new(pcm))
                                .is_ok()
                        );
                    }
                    let mut score = AudioEvent {
                        onset_id: 1,
                        generation: 1,
                        target_frame: 0,
                        onset_lead: 0.0,
                        freq_hz: 220.0,
                        gain: 0.0,
                        duration_secs: 1.0,
                        ui_visuals: 0,
                        controls: crate::OscillatorControls {
                            orbit: 1,
                            ..Default::default()
                        },
                        sample: Some(crate::SampleControls {
                            sample: crate::SampleId(1),
                            playback_rate: 1.0,
                            begin: 0.0,
                            end: 1.0,
                            hold: crate::SampleHold::Slice,
                            muted: false,
                            loop_secs: None,
                            envelope_peak: 1.0,
                            reversed: false,
                            nudge_secs: 0.0,
                            cut: None,
                        }),
                        synth: None,
                        wavetable: None,
                        cut: None,
                    };
                    assert!(expected_shared.ring.push(score.clone()));
                    let mut piano = score.clone();
                    piano.controls.piano = true;
                    piano.gain = 0.1;
                    match control {
                        "mute" => shared.orbit_gains[1].store(0.0f32.to_bits(), Ordering::Relaxed),
                        "duck" => {
                            score.controls.duck = Some(crate::DuckControls {
                                targets: [
                                    Some(crate::DuckTarget {
                                        orbit: 1,
                                        onset_secs: 0.0,
                                        attack_secs: 1.0,
                                        depth: 1.0,
                                    }),
                                    None,
                                    None,
                                    None,
                                ],
                            });
                        }
                        "djf" => score.controls.djf = Some(0.9),
                        "channels" => score.controls.channels = Some([2, 0]),
                        "output pair" => shared.orbit_pairs[1].store(1, Ordering::Relaxed),
                        _ => unreachable!(),
                    }
                    assert!(shared.ring.push(score.clone()));
                    let mut output = [0.0f32; 512];
                    let mut expected = [0.0f32; 512];
                    let mut meter = test_meter();
                    let mut expected_meter = test_meter();
                    write_live_output(&mut backend, &mut meter, &mut output, 4, &shared);
                    write_live_output(
                        &mut expected_backend,
                        &mut expected_meter,
                        &mut expected,
                        4,
                        &expected_shared,
                    );
                    assert!(shared.immediate.push(piano.clone()));
                    assert!(expected_shared.immediate.push(piano));
                    let before = tripwire::Violations::capture();
                    tripwire::audio_scope(|| {
                        write_live_output(&mut backend, &mut meter, &mut output, 4, &shared);
                        write_live_output(
                            &mut expected_backend,
                            &mut expected_meter,
                            &mut expected,
                            4,
                            &expected_shared,
                        );
                    });
                    assert!(tripwire::Violations::capture().since(before).clean());
                    assert!(expected.iter().any(|sample| sample.abs() > 0.001));
                    assert_eq!(
                        output, expected,
                        "{channels}-channel piano inherited score {control}"
                    );
                }
            }
        }

        fn assert_immediate_piano_mix(sampled: bool) {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            let expected_shared = LiveShared::new(1);
            let event = |frequency, cut, piano| AudioEvent {
                onset_id: u64::MAX,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: frequency,
                gain: 0.1,
                duration_secs: f32::INFINITY,
                ui_visuals: 0,
                controls: crate::OscillatorControls {
                    piano,
                    ..Default::default()
                },
                sample: sampled.then_some(crate::SampleControls {
                    sample: crate::SampleId(1),
                    playback_rate: 1.0,
                    begin: 0.0,
                    end: 1.0,
                    hold: crate::SampleHold::Slice,
                    muted: false,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    reversed: false,
                    nudge_secs: 0.0,
                    cut: None,
                }),
                synth: None,
                wavetable: None,
                cut,
            };
            let score = event(220.0, None, false);
            assert!(shared.ring.push(score.clone()));
            assert!(expected_shared.ring.push(score.clone()));
            let mut future = event(110.0, None, false);
            future.target_frame = 480_000;
            assert!(shared.ring.push(future));
            let mut backend = LiveScalarBackend::new(48_000, 8).unwrap();
            let mut expected_backend = LiveScalarBackend::new(48_000, 8).unwrap();
            if sampled {
                for target in [&mut backend, &mut expected_backend] {
                    let pcm =
                        crate::DecodedSample::from_parts(48_000, 1, vec![0.25; 48_000]).unwrap();
                    assert!(
                        target
                            .install_prepared_sample(crate::SampleId(1), Box::new(pcm))
                            .is_ok()
                    );
                }
            }
            let mut output = [0.0f32; 256];
            let mut expected = [0.0f32; 256];
            let mut meter = test_meter();
            let mut expected_meter = test_meter();
            // Put a far-future score event in the consumer's held slot first.
            write_live_output(&mut backend, &mut meter, &mut output, 2, &shared);
            write_live_output(
                &mut expected_backend,
                &mut expected_meter,
                &mut expected,
                2,
                &expected_shared,
            );
            let mut first = event(440.0, Some(1.0), true);
            first.target_frame = u64::MAX; // direct admission must ignore this
            assert!(shared.immediate.push(first));
            assert!(shared.immediate.push(event(660.0, Some(2.0), true)));
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meter, &mut output, 2, &shared);
                write_live_output(
                    &mut expected_backend,
                    &mut expected_meter,
                    &mut expected,
                    2,
                    &expected_shared,
                );
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            assert_ne!(output, expected, "piano sounds on the very next callback");
            assert_eq!(backend.pressure_observation().active_voices, 3);
            let mut release = event(440.0, Some(1.0), true);
            release.controls.choke_only = true;
            assert!(shared.immediate.push(release.clone()));
            for _ in 0..16 {
                write_live_output(&mut backend, &mut meter, &mut output, 2, &shared);
                write_live_output(
                    &mut expected_backend,
                    &mut expected_meter,
                    &mut expected,
                    2,
                    &expected_shared,
                );
            }
            assert_eq!(backend.pressure_observation().active_voices, 2);
            release.cut = Some(2.0);
            assert!(shared.immediate.push(release.clone()));
            for _ in 0..16 {
                write_live_output(&mut backend, &mut meter, &mut output, 2, &shared);
                write_live_output(
                    &mut expected_backend,
                    &mut expected_meter,
                    &mut expected,
                    2,
                    &expected_shared,
                );
            }
            assert_eq!(
                output, expected,
                "releasing piano leaves score PCM unchanged"
            );
            assert_eq!(backend.pressure_observation().active_voices, 1);
            assert!(output.iter().any(|sample| sample.abs() > 0.001));
        }
    }

    /// A turn of the producer names a chain, then ships its first copy. The
    /// callback takes the names first, so the copy that finds no free bus
    /// starts the fade of a chain with no name. The bus of that chain is
    /// free after the fade, and its copy goes back.
    #[test]
    fn a_copy_finds_the_name_its_turn_wrote_before_it() {
        struct Probe;
        impl crate::insert::OrbitInsert for Probe {
            fn key(&self) -> crate::insert::InsertKey {
                crate::insert::InsertKey::default()
            }
            fn set_param(&mut self, _: crate::insert::InsertParam, _: u32) {}
            fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
        }
        let install = |chain: u64| crate::assets::InsertInstall {
            slot: crate::insert::effect_slot(chain, 0),
            sample_rate: 48_000,
            insert: Box::into_raw(Box::new(Probe) as Box<dyn crate::insert::OrbitInsert>),
        };
        let shared = LiveShared::new(1);
        let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
        let mut meters = test_meter();
        let mut output = [0.0f32; 128 * 2];
        let mut render = |backend: &mut LiveScalarBackend| {
            tripwire::audio_scope(|| {
                write_live_output(backend, &mut meters, &mut output, 2, &shared)
            });
        };
        // The copies of 64 chains hold each bus.
        for chain in 100..164 {
            assert!(shared.samples.insert_installs.push(install(chain)).is_ok());
        }
        render(&mut backend);
        // The list of the turn, as the producer writes it, then the copy.
        shared.insert_named_list.fetch_add(1, Ordering::AcqRel);
        for (at, slot) in shared.insert_named.iter().enumerate() {
            slot.store(if at == 0 { 9000 } else { u64::MAX }, Ordering::Release);
        }
        shared.insert_named_list.fetch_add(1, Ordering::AcqRel);
        assert!(shared.samples.insert_installs.push(install(9000)).is_ok());
        render(&mut backend);
        assert_eq!(
            shared.samples.insert_returns.len(),
            1,
            "the copy with no bus"
        );
        // The fade lasts 480 frames.
        for _ in 0..5 {
            render(&mut backend);
        }
        assert_eq!(
            shared.samples.insert_returns.len(),
            2,
            "the copy of the chain that faded"
        );
    }
}
