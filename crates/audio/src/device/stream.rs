use super::*;

/// Translate an absolute frame coordinate without moving its point in time.
///
/// Round upward so a recycle never makes the producer clock run backwards;
/// the bounded forward adjustment is less than one frame at the new rate.
pub(super) fn rebase_frame_rate(frame: u64, old_sample_rate: u32, new_sample_rate: u32) -> u64 {
    let old_sample_rate = u128::from(old_sample_rate.max(1));
    let scaled = u128::from(frame).saturating_mul(u128::from(new_sample_rate.max(1)));
    let rebased = scaled.saturating_add(old_sample_rate - 1) / old_sample_rate;
    u64::try_from(rebased).unwrap_or(u64::MAX)
}

/// The delay before retrying an audio input after an open or stream failure.
/// Studio and the live command use the same delay. Consecutive failures
/// increase the delay until a healthy stream is observed.
pub fn input_retry_delay(attempts: u64) -> Duration {
    let shift = attempts.saturating_sub(1).min(INPUT_RETRY_MAX_SHIFT) as u32;
    INPUT_RETRY_BASE.saturating_mul(1_u32 << shift)
}

impl LiveScalarDevice {
    pub fn start_default(initial_generation: u64) -> Result<Self, DevicePlaybackError> {
        Self::start_output(None, initial_generation)
    }

    /// Open live output on a named device, or the host default when
    /// `selector` is `None`.
    pub fn start_output(
        selector: Option<&str>,
        initial_generation: u64,
    ) -> Result<Self, DevicePlaybackError> {
        Self::start_output_with_options(selector, initial_generation, LiveOutputOptions::default())
    }

    /// Open live output with engine-owned kernels selected before preparation.
    /// The same selection is used for producer-prepared assets and recycled
    /// streams. FFT planning retains its independent automatic policy.
    /// Sample interpolation is set before playback and retained on replacement.
    /// Invalid buffer requests are rejected before probing a device or
    /// allocating the backend; `Auto` retains the platform/environment policy.
    pub fn start_output_with_options(
        selector: Option<&str>,
        initial_generation: u64,
        options: LiveOutputOptions,
    ) -> Result<Self, DevicePlaybackError> {
        options
            .buffer_preference()
            .validate()
            .map_err(buffer_preference_error)?;
        if selector == Some(SILENT_OUTPUT_NAME) {
            return Self::start_silent_with_options(
                SILENT_SAMPLE_RATE,
                initial_generation,
                options,
            );
        }
        let opened = ScalarDevice::open_output(selector)?;
        let shared = LiveShared::new(initial_generation);
        let prepared = PreparedLiveOutput::native(opened, shared.clone(), options, &[])?;
        Self::start_prepared(shared, options, prepared)
    }

    /// Live output that makes no sound: see [`SILENT_OUTPUT_NAME`].
    pub fn start_silent(
        sample_rate: u32,
        initial_generation: u64,
    ) -> Result<Self, DevicePlaybackError> {
        Self::start_silent_with_options(
            sample_rate,
            initial_generation,
            LiveOutputOptions::default(),
        )
    }

    /// Open silent output with the same kernel selection as an explicit
    /// offline or device-backed renderer and the requested callback period.
    /// Sample interpolation uses the mode supplied in `options`.
    /// Automatic silent output retains its fixed default buffer size.
    pub fn start_silent_with_options(
        sample_rate: u32,
        initial_generation: u64,
        options: LiveOutputOptions,
    ) -> Result<Self, DevicePlaybackError> {
        options
            .buffer_preference()
            .validate()
            .map_err(buffer_preference_error)?;
        let shared = LiveShared::new(initial_generation);
        let prepared = PreparedLiveOutput::silent(shared.clone(), sample_rate, options, &[])?;
        Self::start_prepared(shared, options, prepared)
    }

    pub(super) fn start_prepared(
        shared: LiveShared,
        options: LiveOutputOptions,
        prepared: PreparedLiveOutput,
    ) -> Result<Self, DevicePlaybackError> {
        prepared.start_gated()?;
        let facts = &prepared.facts;
        let mut device = Self {
            stream: None,
            callback_gate: None,
            silent: None,
            input: None,
            reverb_cache: std::sync::Mutex::new(vec![None; crate::scalar::MAX_ORBITS]),
            fx_reverb_cache: std::sync::Mutex::new(Vec::with_capacity(8)),
            kabel_cache: std::sync::Mutex::new(Vec::with_capacity(crate::MAX_KABEL_GRAPHS)),
            program_cache: std::sync::Mutex::new(Vec::new()),
            insert_cache: std::sync::Mutex::new(Vec::with_capacity(
                crate::insert::INSERT_SLOTS * 3,
            )),
            insert_waiting: std::sync::Mutex::new(Vec::with_capacity(crate::insert::INSERT_SLOTS)),
            insert_released: std::sync::Mutex::new(Vec::with_capacity(
                crate::insert::INSERT_SLOTS * 3,
            )),
            insert_provider: std::sync::Mutex::new(None),
            insert_names: std::sync::Mutex::new(None),
            insert_values: std::sync::Mutex::new(None),
            plugin_pool: std::sync::Mutex::new(None),
            plugin_pool_shipped: AtomicBool::new(false),
            shared,
            sample_rate: facts.sample_rate_hz(),
            channels: facts.channels(),
            host_id: facts.host().id().map(Into::into),
            output_sample_format: facts.sample_format(),
            name: facts.device_id().to_owned(),
            stream_id: NEXT_LIVE_STREAM_ID.fetch_add(1, Ordering::Relaxed),
            options,
            requested_buffer_frames: facts.requested_buffer_frames(),
            reported_buffer_frames: facts.reported_buffer_frames(),
            device_period_frames: facts.device_period_frames(),
            tripwire_baseline: prepared.tripwire_baseline,
            tripwire_canaries: tripwire::Violations::default(),
            backend_bytes: 0,
        };
        device.activate_prepared(prepared)?;
        Ok(device)
    }

    fn activate_prepared(
        &mut self,
        prepared: PreparedLiveOutput,
    ) -> Result<(), DevicePlaybackError> {
        let backend_bytes = prepared.backend_bytes;
        let callback_gate = Arc::clone(&prepared.gate);
        match prepared.activate()? {
            OutputHandle::Native(stream) => self.stream = Some(stream),
            OutputHandle::Silent(silent) => self.silent = Some(silent),
        }
        self.callback_gate = Some(callback_gate);
        self.backend_bytes = backend_bytes;
        Ok(())
    }

    pub fn is_silent(&self) -> bool {
        self.silent.is_some()
    }

    /// Whether an output handle is still owned. Unlike health, this separates
    /// an untouched but unhealthy stream from failed replacement after teardown.
    pub fn has_output(&self) -> bool {
        self.stream.is_some() || self.silent.is_some()
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Open the audio input - the host default, or a named device - so
    /// `s("in")` voices have something to read. The input runs on its own
    /// stream beside the output (or beside the silent output); its frames
    /// go into a ring the backend reads a little behind.
    pub fn open_input(&mut self, selector: Option<&str>) -> Result<String, DevicePlaybackError> {
        self.input = None;
        let input = AudioInput::open(
            selector,
            Arc::clone(&self.shared.input),
            Some(self.sample_rate),
        )?;
        let name = input.name().to_owned();
        self.input = Some(input);
        Ok(name)
    }

    /// Close the audio input; `s("in")` voices read silence.
    pub fn close_input(&mut self) {
        self.input = None;
        self.shared.input.set_channels(0);
    }

    pub fn input_name(&self) -> Option<&str> {
        self.input.as_ref().map(AudioInput::name)
    }

    /// The ring the open input writes into - what `s("in")` reads, and
    /// what a recorded sample is drained from. `None` with no input open.
    pub fn input_ring(&self) -> Option<Arc<crate::input::InputRing>> {
        self.input.as_ref().map(|_| Arc::clone(&self.shared.input))
    }

    /// Whether the open input's stream has reported an error - a device
    /// unplugged, a driver gone - so the owner can open it again.
    pub fn input_failed(&self) -> bool {
        self.input.as_ref().is_some_and(AudioInput::failed)
    }

    /// Channels the open input delivers; zero without one.
    pub fn input_channels(&self) -> usize {
        self.shared.input.channels()
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    pub fn requested_buffer_frames(&self) -> u32 {
        self.requested_buffer_frames
    }

    /// Selection retained by this device and its producer-prepared assets.
    pub const fn dispatch(&self) -> DspDispatch {
        self.options.dispatch()
    }

    /// Original requests retained across stream recycling.
    pub const fn output_options(&self) -> LiveOutputOptions {
        self.options
    }

    /// Adopt a buffer preference for this and every later stream open,
    /// including the recycle path. The current callback keeps its size
    /// until the next open; the caller recycles to apply it at once.
    pub fn set_buffer_preference(&mut self, preference: AudioBufferPreference) {
        self.options = self.options.with_buffer_preference(preference);
    }

    /// Original preference, retained across automatic stream recycling.
    pub const fn buffer_preference(&self) -> AudioBufferPreference {
        self.options.buffer_preference()
    }

    /// Backend's best estimate of frames per callback, when supported.
    pub fn reported_buffer_frames(&self) -> Option<u32> {
        self.reported_buffer_frames
    }

    /// Stable facts used by every diagnostics projection.
    pub fn audio_facts(&self) -> AudioStreamFacts {
        let host = self
            .host_id
            .as_ref()
            .map_or(AudioHost::Silent, |id| AudioHost::cpal(id.clone()));
        let output = AudioOutputFacts::new(
            host,
            self.name.clone(),
            self.sample_rate,
            self.channels,
            self.output_sample_format,
            self.requested_buffer_frames,
            self.reported_buffer_frames,
        )
        .with_buffer_preference(self.buffer_preference())
        .with_device_period_frames(self.device_period_frames);
        AudioStreamFacts::new(
            output,
            self.input.as_ref().map(|input| input.facts().clone()),
        )
    }
}

impl Drop for LiveScalarDevice {
    fn drop(&mut self) {
        self.shared.request_stop();
        if let Some(gate) = self.callback_gate.take() {
            gate.retire();
        }
        if let Some(mut silent) = self.silent.take() {
            silent.stop();
        }
        // Finish the output owner before publishing retirement to any
        // producer endpoint retained beyond this device's lifetime.
        self.stream = None;
        // No callback runs now, so no job is out. The plugin workers end
        // here, on the producer.
        drop(
            self.plugin_pool
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        self.shared.confirmations.retire_after_join();
    }
}

impl PreparedLiveOutput {
    pub(super) fn backend(
        shared: &LiveShared,
        sample_rate: u32,
        options: LiveOutputOptions,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<LiveScalarBackend, DevicePlaybackError> {
        validate_output_samples(samples)?;
        let mut backend =
            LiveScalarBackend::with_dispatch(sample_rate, MAX_LIVE_VOICES, options.dispatch())
                .map_err(|message| {
                    DevicePlaybackError::Unavailable(format!(
                        "cannot initialise live scalar DSP: {message}"
                    ))
                })?;
        backend.set_sample_resampling_mode(options.sample_resampling_mode());
        backend.set_max_polyphony(shared.max_polyphony.load(Ordering::Relaxed));
        backend.set_input(Some(shared.input.clone()));
        backend.set_confirmations(shared.confirmations.clone());
        for (id, sample) in samples {
            let displaced = backend
                .install_prepared_sample(*id, Box::new(sample.clone()))
                .map_err(|_| {
                    DevicePlaybackError::ResourceLimit("sample id exceeds output bank".into())
                })?;
            drop(displaced);
        }
        Ok(backend)
    }

    fn native(
        opened: ScalarDevice,
        shared: LiveShared,
        options: LiveOutputOptions,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<Self, DevicePlaybackError> {
        let sample_rate = opened.sample_rate();
        let native_format = opened.config.sample_format();
        let format = sample_format_from_cpal(native_format).ok_or_else(|| {
            DevicePlaybackError::Unavailable(format!(
                "audio output sample format {native_format} is not supported by live scalar playback"
            ))
        })?;
        let requested =
            select_live_buffer_frames(opened.config.buffer_size(), options.buffer_preference())?;
        let backend = Self::backend(&shared, sample_rate, options, samples)?;
        let backend_bytes = backend.prepared_bytes();
        let gate = Arc::new(CallbackGate::new());
        // Include callbacks invoked synchronously while CPAL builds a stream.
        let tripwire_baseline = tripwire::Violations::capture();
        let stream = build_live_stream(
            &opened.device,
            &opened.config,
            requested,
            backend,
            shared,
            Arc::clone(&gate),
        )?;
        let reported = stream.buffer_size().ok();
        let facts = AudioOutputFacts::new(
            AudioHost::cpal(opened.host_id),
            opened.name,
            sample_rate,
            opened.config.channels(),
            Some(format),
            requested,
            reported,
        )
        .with_buffer_preference(options.buffer_preference())
        .with_device_period_frames(fixed_device_period(opened.config.buffer_size()));
        Ok(Self {
            handle: OutputHandle::Native(stream),
            facts,
            gate,
            tripwire_baseline,
            backend_bytes,
        })
    }

    fn silent(
        shared: LiveShared,
        sample_rate: u32,
        options: LiveOutputOptions,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<Self, DevicePlaybackError> {
        let buffer_frames = options
            .buffer_preference()
            .resolve(SILENT_BUFFER_FRAMES, AudioBufferRange::Unknown)
            .map_err(buffer_preference_error)?;
        let backend = Self::backend(&shared, sample_rate, options, samples)?;
        let backend_bytes = backend.prepared_bytes();
        let gate = Arc::new(CallbackGate::new());
        let tripwire_baseline = tripwire::Violations::capture();
        let silent = spawn_silent_output(
            shared,
            backend,
            sample_rate,
            buffer_frames,
            Arc::clone(&gate),
        )?;
        Ok(Self {
            handle: OutputHandle::Silent(silent),
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
            gate,
            tripwire_baseline,
            backend_bytes,
        })
    }

    fn start_gated(&self) -> Result<(), DevicePlaybackError> {
        if let OutputHandle::Native(stream) = &self.handle {
            stream.play().map_err(|error| {
                DevicePlaybackError::Unavailable(format!(
                    "cannot start live audio output {}: {error}",
                    self.facts.device_id(),
                ))
            })?;
        }
        Ok(())
    }

    fn activate(self) -> Result<OutputHandle, DevicePlaybackError> {
        self.gate.activate()?;
        Ok(self.handle)
    }
}

impl SilentHandle {
    /// Stop the thread and wait for it to leave `LiveShared`, so a
    /// replacement stream never runs concurrently with it.
    pub(super) fn stop(&mut self) {
        self.close.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(feature = "test-support")]
impl LiveScalarDevice {
    /// Freeze (or release) the silent output's clock, for tests that must
    /// observe a moment before a line without racing wall time.
    ///
    /// Holding returns only once the silent thread has stopped between two
    /// blocks, so the render frontier read afterwards stays put until the
    /// release. Released, the clock resumes from where it stopped. Returns
    /// false, doing nothing, when this device is not a running silent output.
    #[doc(hidden)]
    pub fn hold_silent_clock_for_test(&self, hold: bool) -> bool {
        let Some(silent) = self.silent.as_ref().filter(|silent| silent.join.is_some()) else {
            return false;
        };
        if !hold {
            silent
                .clock_hold
                .store(SILENT_CLOCK_RUNNING, Ordering::Release);
            return true;
        }
        let _ = silent.clock_hold.compare_exchange(
            SILENT_CLOCK_RUNNING,
            SILENT_CLOCK_HOLD_REQUESTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while silent.clock_hold.load(Ordering::Acquire) != SILENT_CLOCK_HELD {
            assert!(
                Instant::now() < deadline,
                "the silent output never paused its clock"
            );
            std::thread::sleep(Duration::from_micros(200));
        }
        true
    }
}

impl Drop for SilentHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn silent_elapsed(
    started: &mut Option<Instant>,
    gate: &CallbackGate,
    clock: impl FnOnce() -> Instant,
) -> Option<Duration> {
    if !gate.is_active() {
        return None;
    }
    // A timestamp captured before admission could include preparation time
    // if activation occurs while this thread is descheduled.
    let now = clock();
    Some(now.duration_since(*started.get_or_insert(now)))
}

fn spawn_silent_output(
    shared: LiveShared,
    mut backend: LiveScalarBackend,
    sample_rate: u32,
    buffer_frames: u32,
    gate: Arc<CallbackGate>,
) -> Result<SilentHandle, DevicePlaybackError> {
    let close = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&close);
    #[cfg(feature = "test-support")]
    let clock_hold = Arc::new(std::sync::atomic::AtomicU8::new(SILENT_CLOCK_RUNNING));
    #[cfg(feature = "test-support")]
    let hold = Arc::clone(&clock_hold);
    let mut meters = LiveCallbackMeters::new(sample_rate, &shared.realtime_load);
    let mut output = vec![0.0f32; buffer_frames as usize * 2];
    let join = std::thread::Builder::new()
        .name("silent-output".into())
        .spawn(move || {
            let mut started: Option<Instant> = None;
            let mut delivered = 0u64;
            #[cfg(feature = "test-support")]
            let mut held_since: Option<Instant> = None;
            while !flag.load(Ordering::Acquire) {
                // A held clock delivers nothing, so a test can look at a
                // moment that wall time would otherwise carry away. On
                // release the origin moves forward by the time spent held,
                // so the clock resumes where it stopped (partial progress
                // towards the next block included) instead of rendering the
                // held span in one catch-up burst.
                #[cfg(feature = "test-support")]
                {
                    if hold.load(Ordering::Acquire) != SILENT_CLOCK_RUNNING {
                        held_since.get_or_insert_with(Instant::now);
                        // Acknowledge only a standing request: a release
                        // that lands between the load and here must win.
                        let _ = hold.compare_exchange(
                            SILENT_CLOCK_HOLD_REQUESTED,
                            SILENT_CLOCK_HELD,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                        std::thread::sleep(Duration::from_micros(500));
                        continue;
                    }
                    if let Some(since) = held_since.take()
                        && let Some(origin) = started.as_mut()
                    {
                        *origin += since.elapsed();
                    }
                }
                let first = started.is_none();
                let Some(elapsed) = silent_elapsed(&mut started, &gate, Instant::now) else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                if first {
                    shared.playback_origin_nanos.store(0, Ordering::Release);
                }
                let due = (elapsed.as_secs_f64() * f64::from(sample_rate)) as u64;
                while delivered + u64::from(buffer_frames) <= due && !flag.load(Ordering::Acquire) {
                    gate.render_or_silence(&mut output, |output| {
                        shared.buffer_playback_nanos.store(
                            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
                            Ordering::Release,
                        );
                        shared.playback_latency_nanos.store(0, Ordering::Release);
                        write_live_output(&mut backend, &mut meters, output, 2, &shared);
                    });
                    delivered += u64::from(buffer_frames);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        })
        .map_err(|error| {
            DevicePlaybackError::Unavailable(format!("cannot start the silent output: {error}"))
        })?;
    Ok(SilentHandle {
        close,
        join: Some(join),
        #[cfg(feature = "test-support")]
        clock_hold,
    })
}

impl LiveScalarDevice {
    /// Whether the output stream itself has reported an error: the device
    /// went away under the music.
    ///
    /// Told apart from the rest of [`Self::check_health`] because it is
    /// the recoverable one. A Bluetooth headset changing profile, an
    /// interface unplugged, a machine waking from sleep - the stream is
    /// gone and a new one opens in its place, where a run of unbounded
    /// buffering says the machine cannot keep up and reopening would only
    /// do it again.
    pub fn output_failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }

    pub fn check_health(&self) -> Result<(), DevicePlaybackError> {
        if self.shared.failed.load(Ordering::Acquire) {
            return Err(DevicePlaybackError::Unavailable(format!(
                "audio output {} failed while playing live",
                self.name
            )));
        }
        // A refused voice is not fatal. Running out of voices means that
        // some notes did not sound: that is degradation, not failure, and
        // stopping the music is worse than a dropped note. `refused_voices`
        // is cumulative and never decays, so it cannot gate health. The
        // live report carries it as recoverable telemetry and the set
        // plays on.

        // Judge the CURRENT latency, not the high-water mark: a single
        // transient spike must not kill a live set minutes later (the mark
        // never decays; it remains telemetry). Unbounded buffering is still a
        // failure. WSLg's RDP sink can deepen its buffer after jitter, so its
        // default ceiling is higher. RUSTEL_MAX_LIVE_LATENCY_MS overrides the
        // ceiling on every platform.
        let latency = self.shared.playback_latency_nanos.load(Ordering::Acquire);
        let limit = live_latency_limit_nanos();
        if latency > limit {
            return Err(DevicePlaybackError::Unavailable(format!(
                "live output {} buffered {:.1}ms ahead, above the {:.1}ms safety limit",
                self.name,
                latency as f64 / 1_000_000.0,
                limit as f64 / 1_000_000.0
            )));
        }
        Ok(())
    }

    /// Tear the CPAL stream and open a fresh one on the same shared clock.
    ///
    /// Voices in the old callback die (new backend). Generation and clock time
    /// stay put; absolute frame coordinates are rescaled if the replacement
    /// device uses a different rate. The caller must re-query the cleared
    /// scheduling horizon before resuming ordinary producer steps.
    pub fn recycle_output(&mut self) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to(None)
    }

    /// Recycle onto a named output, or the host default when `selector` is
    /// `None`. Playback continues on the replacement stream at the same
    /// generation and clock time.
    ///
    /// A failed device lookup leaves the current output untouched. Querying
    /// its default configuration and opening the replacement happen after the
    /// old stream closes, since some hosts require exclusive device access.
    pub fn recycle_output_to(&mut self, selector: Option<&str>) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to_with(
            selector,
            ScalarDevice::discover_output,
            Self::prepare_recycled_output,
        )
    }

    /// Recycle with cancellation checks before teardown and around candidate
    /// preparation/start. Cancellation after teardown leaves no output; the
    /// caller must not schedule new work and should release the live owner.
    pub fn recycle_output_to_guarded(
        &mut self,
        selector: Option<&str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to_with_cancel(
            selector,
            ScalarDevice::discover_output,
            Self::prepare_recycled_output,
            cancelled,
        )
    }

    /// Replace the output with a complete snapshot of its decoded sample bank.
    /// PCM is shared, not decoded again, and installed before callbacks activate.
    /// Pending sample installs and uninstalls are superseded after the old
    /// callback joins. The caller retains the snapshot for a failed-open retry.
    pub fn recycle_output_to_with_samples(
        &mut self,
        selector: Option<&str>,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
        cancelled: impl Fn() -> bool,
    ) -> Result<(), DevicePlaybackError> {
        validate_output_samples(samples)?;
        self.recycle_output_to_with_cancel(
            selector,
            ScalarDevice::discover_output,
            |device, opened| device.prepare_recycled_output_with_samples(opened, samples),
            cancelled,
        )
    }

    /// Mark the stream as having reported an error, the way a device that
    /// went away under the music does. What a Bluetooth headset changing
    /// profile leaves behind, without needing one.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn fail_output_stream_for_test(&self) {
        self.shared.failed.store(true, Ordering::Release);
    }

    /// Inject a preparation error after closing the old callback and draining
    /// its ring. This exercises the same teardown as an exclusive-device open
    /// failure without depending on a particular host or physical device.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn fail_output_replacement_for_test(&mut self) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to_with(
            Some(SILENT_OUTPUT_NAME),
            |_| unreachable!("silent output does not require discovery"),
            |_, _| {
                Err(DevicePlaybackError::Unavailable(
                    "output preparation failed".into(),
                ))
            },
        )
    }

    pub(super) fn recycle_output_to_with(
        &mut self,
        selector: Option<&str>,
        discover: impl FnOnce(Option<&str>) -> Result<DiscoveredOutput, DevicePlaybackError>,
        prepare: impl FnOnce(
            &Self,
            Option<DiscoveredOutput>,
        ) -> Result<PreparedLiveOutput, DevicePlaybackError>,
    ) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to_with_cancel(selector, discover, prepare, || false)
    }

    fn recycle_output_to_with_cancel(
        &mut self,
        selector: Option<&str>,
        discover: impl FnOnce(Option<&str>) -> Result<DiscoveredOutput, DevicePlaybackError>,
        prepare: impl FnOnce(
            &Self,
            Option<DiscoveredOutput>,
        ) -> Result<PreparedLiveOutput, DevicePlaybackError>,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), DevicePlaybackError> {
        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        self.buffer_preference()
            .validate()
            .map_err(buffer_preference_error)?;
        let opened = if selector == Some(SILENT_OUTPUT_NAME)
            || (selector.is_none() && self.name == SILENT_OUTPUT_NAME)
        {
            None
        } else {
            Some(discover(selector)?)
        };
        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        let old_sample_rate = self.sample_rate;
        self.shared.request_stop();
        if let Some(gate) = self.callback_gate.take() {
            gate.retire();
        }
        if let Some(mut silent) = self.silent.take() {
            silent.stop();
        }
        // Dropping the stream joins the CPAL callback thread, so the consumer
        // role is free from here. Saying so explicitly keeps the ring's
        // single-consumer check able to tell this legitimate handoff from a
        // second consumer running concurrently.
        self.stream = None;
        for install in self.shared.samples.insert_installs.drain_available() {
            // SAFETY: the old callback joined. This unconsumed box has one owner.
            drop(unsafe { Box::from_raw(install.insert) });
        }
        // A release names a copy of the old backend. It holds no box.
        while self.shared.samples.insert_releases.pop().is_some() {}
        // The plugin workers stay. The next turn ships them to the new
        // backend.
        for install in self.shared.samples.pool_installs.drain_available() {
            // SAFETY: the old callback joined. This count of the `Arc` has
            // one owner.
            drop(unsafe { Arc::from_raw(install.0) });
        }
        *self.plugin_pool_shipped.get_mut() = false;
        self.reclaim_assets();
        self.clear_reverb_caches();
        // No output remains if replacement preparation or startup fails.
        self.shared.failed.store(true, Ordering::Release);
        // SAFETY: the stream and the silent callback are gone, so no callback
        // thread pops either ring again.
        unsafe {
            self.shared.ring.release_consumer();
            self.shared.immediate.release_consumer();
        }
        while self.shared.ring.pop().is_some() {}
        while self.shared.immediate.pop().is_some() {}
        // The producer thread above temporarily became the drain consumer.
        // Hand the role back once more so the replacement callback can claim
        // it instead of refusing every pop as a second consumer.
        // SAFETY: this thread's drain has finished, and no replacement
        // callback exists yet.
        unsafe {
            self.shared.ring.release_consumer();
            self.shared.immediate.release_consumer();
        }
        self.shared.reset_analysis();
        self.shared.meter.reset_levels();

        // The old consumer has joined. Keep its completed receipts available
        // to the producer, but never let a replacement certify unfinished
        // work using a new bank or a rebased output clock.
        self.shared
            .confirmations
            .advance_epoch_after_join()
            .map_err(|error| match error {
                crate::confirmation::EpochAdvanceError::Busy => DevicePlaybackError::Unavailable(
                    "output confirmation producer is busy; retry output recovery".to_owned(),
                ),
                crate::confirmation::EpochAdvanceError::Exhausted => {
                    DevicePlaybackError::ResourceLimit(
                        "output confirmation epoch exhausted".to_owned(),
                    )
                }
                crate::confirmation::EpochAdvanceError::Closed => DevicePlaybackError::Unavailable(
                    "output confirmation channel is closed".to_owned(),
                ),
            })?;

        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        let prepared = prepare(self, opened)?;
        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        // A host may invoke callbacks during build or play. They remain
        // gated until both the shared clock and producer facts are committed.
        prepared.start_gated()?;
        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        let facts = &prepared.facts;
        self.shared
            .prepare_recycled_stream(old_sample_rate, facts.sample_rate_hz());
        self.shared.failed.store(false, Ordering::Release);
        self.shared.stop.0.store(OUTPUT_RUNNING, Ordering::Release);
        self.sample_rate = facts.sample_rate_hz();
        self.channels = facts.channels();
        self.host_id = facts.host().id().map(Into::into);
        self.output_sample_format = facts.sample_format();
        self.name = facts.device_id().to_owned();
        self.requested_buffer_frames = facts.requested_buffer_frames();
        self.reported_buffer_frames = facts.reported_buffer_frames();
        self.device_period_frames = facts.device_period_frames();
        if let Err(error) = self.activate_prepared(prepared) {
            self.shared.request_stop();
            self.shared.failed.store(true, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn prepare_recycled_output(
        &self,
        opened: Option<DiscoveredOutput>,
    ) -> Result<PreparedLiveOutput, DevicePlaybackError> {
        self.prepare_output(opened, &[])
    }

    fn prepare_recycled_output_with_samples(
        &self,
        opened: Option<DiscoveredOutput>,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<PreparedLiveOutput, DevicePlaybackError> {
        // The old callback has joined and only this producer owns the channel.
        // Its queued commands predate the authoritative replacement snapshot.
        for install in self.shared.samples.installs.drain_available() {
            if !install.sample.is_null() {
                // SAFETY: this unconsumed producer-owned baton has no other owner.
                drop(unsafe { Box::from_raw(install.sample) });
            }
        }
        self.reclaim_assets();
        self.prepare_output(opened, samples)
    }

    fn prepare_output(
        &self,
        opened: Option<DiscoveredOutput>,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<PreparedLiveOutput, DevicePlaybackError> {
        match opened {
            Some(opened) => PreparedLiveOutput::native(
                ScalarDevice::configure_output(opened)?,
                self.shared.clone(),
                self.options,
                samples,
            ),
            None => PreparedLiveOutput::silent(
                self.shared.clone(),
                SILENT_SAMPLE_RATE,
                self.options,
                samples,
            ),
        }
    }
}

fn validate_output_samples(
    samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
) -> Result<(), DevicePlaybackError> {
    if samples.len() > crate::sample::SAMPLE_BANK_CAPACITY
        || samples
            .iter()
            .any(|(id, _)| id.0 as usize >= crate::sample::SAMPLE_BANK_CAPACITY)
    {
        return Err(DevicePlaybackError::ResourceLimit(
            "sample snapshot exceeds output bank capacity".into(),
        ));
    }
    Ok(())
}

fn live_latency_limit_nanos() -> u64 {
    let default_ms = if std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::env::var_os("WSL_INTEROP").is_some()
    {
        2000
    } else {
        MAX_LIVE_PLAYBACK_LATENCY_NANOS / 1_000_000
    };
    std::env::var(MAX_LIVE_LATENCY_ENV)
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .filter(|ms| (50..=5_000).contains(ms))
        .unwrap_or(default_ms)
        * 1_000_000
}

pub(super) fn buffer_preference_error(error: crate::AudioBufferError) -> DevicePlaybackError {
    DevicePlaybackError::Unavailable(error.to_string())
}

fn select_live_buffer_frames(
    supported: &SupportedBufferSize,
    preference: AudioBufferPreference,
) -> Result<u32, DevicePlaybackError> {
    // Forwarded audio paths (WSLg's RDP sink, which shares one channel with
    // window video) underrun audibly at an interactive size, so the default
    // there is large. A typed-in request overrides either platform default:
    // the env knob is how an arbitrary frame count reaches the automatic
    // path, and the report records what was actually used.
    let platform_default = if std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::env::var_os("WSL_INTEROP").is_some()
    {
        2048
    } else {
        TARGET_LIVE_BUFFER_FRAMES
    };
    let target = std::env::var(LIVE_BUFFER_FRAMES_ENV)
        .ok()
        .and_then(|frames| frames.parse::<u32>().ok())
        .filter(|frames| {
            (AudioBufferPreference::MIN_FRAMES..=AudioBufferPreference::MAX_FRAMES).contains(frames)
        })
        .unwrap_or(platform_default);
    preference
        .resolve(target, live_buffer_range(supported, cfg!(windows)))
        .map_err(buffer_preference_error)
}

/// The range a buffer request is resolved against.
///
/// WASAPI in shared mode advertises its device period as a degenerate range
/// (min == max, 480 frames at 48 kHz), but that period is the callback's
/// cadence, not a limit on the buffer: the request is passed to
/// `IAudioClient::Initialize` as the buffer duration, which accepts any
/// positive length and keeps that much audio queued ahead of the period.
/// A ceiling would clamp every request (`--buffer-frames 2048`, the
/// studio's output-latency row, `RUSTEL_LIVE_BUFFER_FRAMES`) to the same
/// 10 ms, and a larger buffer could not fix underruns on Windows.
/// With `fixed_period_is_floor` such a range is a floor only: smaller
/// requests still get the period, larger ones pass through. Other hosts
/// keep a degenerate range as the hard constraint it is there.
fn live_buffer_range(
    supported: &SupportedBufferSize,
    fixed_period_is_floor: bool,
) -> AudioBufferRange {
    match *supported {
        SupportedBufferSize::Range { min, max } if fixed_period_is_floor && min == max => {
            AudioBufferRange::Range {
                min,
                max: AudioBufferPreference::MAX_FRAMES.max(min),
            }
        }
        SupportedBufferSize::Range { min, max } => AudioBufferRange::Range { min, max },
        SupportedBufferSize::Unknown => AudioBufferRange::Unknown,
    }
}

/// A host's fixed callback period, when it advertises one as a degenerate
/// buffer range: kept beside the request so the stream facts can say both
/// when a shared-mode buffer is larger than its period.
fn fixed_device_period(supported: &SupportedBufferSize) -> Option<u32> {
    match *supported {
        SupportedBufferSize::Range { min, max } if min == max => Some(min),
        _ => None,
    }
}

fn sample_format_from_cpal(format: SampleFormat) -> Option<AudioSampleFormat> {
    match format {
        SampleFormat::I8 => Some(AudioSampleFormat::I8),
        SampleFormat::I16 => Some(AudioSampleFormat::I16),
        SampleFormat::I24 => Some(AudioSampleFormat::I24),
        SampleFormat::I32 => Some(AudioSampleFormat::I32),
        SampleFormat::I64 => Some(AudioSampleFormat::I64),
        SampleFormat::U8 => Some(AudioSampleFormat::U8),
        SampleFormat::U16 => Some(AudioSampleFormat::U16),
        SampleFormat::U24 => Some(AudioSampleFormat::U24),
        SampleFormat::U32 => Some(AudioSampleFormat::U32),
        SampleFormat::U64 => Some(AudioSampleFormat::U64),
        SampleFormat::F32 => Some(AudioSampleFormat::F32),
        SampleFormat::F64 => Some(AudioSampleFormat::F64),
        _ => None,
    }
}

/// The input device whose name is, or contains, `wanted`.
fn find_input_device(host: &cpal::Host, wanted: &str) -> Result<cpal::Device, DevicePlaybackError> {
    let candidates = host
        .input_devices()
        .map_err(|error| DevicePlaybackError::Unavailable(error.to_string()))?
        .filter_map(|device| {
            device.id().ok().map(|id| {
                let friendly = device
                    .description()
                    .ok()
                    .map(|description| description.name().to_owned())
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_default();
                (id.to_string(), friendly, device)
            })
        })
        .collect::<Vec<_>>();
    let matched = match_listed_device(&candidates, wanted).ok_or_else(|| {
        DevicePlaybackError::Unavailable(format!(
            "audio input {wanted:?} not found; available: {}",
            candidates
                .iter()
                .map(|(_, friendly, _)| friendly.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    Ok(candidates
        .into_iter()
        .nth(matched)
        .map(|(_, _, device)| device)
        .expect("matched"))
}

impl AudioInput {
    /// Open the host default, or a named device, onto `ring`.
    pub fn open(
        selector: Option<&str>,
        ring: Arc<crate::input::InputRing>,
        preferred_rate: Option<u32>,
    ) -> Result<Self, DevicePlaybackError> {
        let device = with_preferred_host(|host| match selector {
            None => host.default_input_device().ok_or_else(|| {
                DevicePlaybackError::Unavailable("no default audio input device".into())
            }),
            Some(wanted) => find_input_device(host, wanted),
        })?;
        let name = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "default-input".into());
        let config = device.default_input_config().map_err(|error| {
            DevicePlaybackError::Unavailable(format!(
                "cannot read default input configuration for {name}: {error}"
            ))
        })?;
        // The output's rate when the device offers it, so the ring is read
        // frame for frame; otherwise the device's own, and the reader walks
        // the ring at the ratio.
        let config = match preferred_rate {
            Some(rate) if rate != config.sample_rate() => {
                input_config_at_rate(&device, &config, rate).unwrap_or(config)
            }
            _ => config,
        };
        if config.channels() == 0 {
            return Err(DevicePlaybackError::Unavailable(format!(
                "audio input {name} reports no channels"
            )));
        }
        let native_sample_format = config.sample_format();
        let sample_format = sample_format_from_cpal(native_sample_format).ok_or_else(|| {
            DevicePlaybackError::Unavailable(format!(
                "audio input sample format {native_sample_format} is not supported"
            ))
        })?;
        ring.set_channels(usize::from(config.channels()));
        ring.set_sample_rate(config.sample_rate());
        ring.forget_deliveries();
        let failed = Arc::new(AtomicBool::new(false));
        let stream = build_input_stream(&device, &config, Arc::clone(&ring), Arc::clone(&failed))?;
        stream.play().map_err(|error| {
            DevicePlaybackError::Unavailable(format!("cannot start audio input {name}: {error}"))
        })?;
        Ok(Self {
            _stream: stream,
            facts: AudioInputFacts::new(
                name,
                config.sample_rate(),
                config.channels(),
                sample_format,
            ),
            ring,
            failed,
        })
    }

    pub fn name(&self) -> &str {
        self.facts.device_id()
    }

    /// The ring this input writes into.
    pub fn ring(&self) -> &Arc<crate::input::InputRing> {
        &self.ring
    }

    pub fn facts(&self) -> &AudioInputFacts {
        &self.facts
    }

    pub fn channels(&self) -> usize {
        self.ring.channels()
    }

    /// The loudest sample since the last take, post-fader.
    pub fn take_peak(&self) -> f32 {
        self.ring.take_peak()
    }

    pub fn set_gain(&self, gain: f32) {
        self.ring.set_gain(gain);
    }

    pub fn gain(&self) -> f32 {
        self.ring.gain()
    }

    /// Whether the stream reported an error since it opened.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

/// An input stream feeding the ring, in whatever sample format the device
/// speaks, converted to f32 a chunk at a time on the stack.
/// The device's configuration at `rate` with the default's channels and
/// sample format, when it offers one.
fn input_config_at_rate(
    device: &cpal::Device,
    default: &cpal::SupportedStreamConfig,
    rate: u32,
) -> Option<cpal::SupportedStreamConfig> {
    let ranges: Vec<_> = device.supported_input_configs().ok()?.collect();
    ranges
        .iter()
        .filter(|range| range.min_sample_rate() <= rate && rate <= range.max_sample_rate())
        .find(|range| {
            range.channels() == default.channels()
                && range.sample_format() == default.sample_format()
        })
        .or_else(|| {
            ranges.iter().find(|range| {
                range.min_sample_rate() <= rate
                    && rate <= range.max_sample_rate()
                    && range.channels() == default.channels()
            })
        })
        .map(|range| range.with_sample_rate(rate))
}

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    ring: Arc<crate::input::InputRing>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, DevicePlaybackError> {
    macro_rules! build {
        ($sample:ty) => {
            build_typed_input_stream::<$sample>(device, config.clone().into(), ring, failed)
        };
    }
    match config.sample_format() {
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(U24),
        SampleFormat::U32 => build!(u32),
        format => Err(DevicePlaybackError::Unavailable(format!(
            "audio input sample format {format} is not supported"
        ))),
    }
}

fn build_typed_input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    ring: Arc<crate::input::InputRing>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, DevicePlaybackError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                // The whole delivery, before it is written in pieces: the
                // reader's lag follows the delivery.
                ring.note_delivery((data.len() / channels) as u64);
                let mut chunk = [0.0f32; 4096];
                for frames in data.chunks(chunk.len() / channels * channels) {
                    for (slot, sample) in chunk.iter_mut().zip(frames) {
                        *slot = f32::from_sample(*sample);
                    }
                    ring.write(&chunk[..frames.len()], channels);
                }
            },
            // A device that goes is not silence to be lived with: the
            // owner sees the flag and opens the input again.
            flag_stream_failure(failed),
            Some(Duration::from_secs(3)),
        )
        .map_err(|error| {
            DevicePlaybackError::Unavailable(format!("cannot build audio input stream: {error}"))
        })
}

fn find_output_device(
    host: &cpal::Host,
    wanted: &str,
) -> Result<cpal::Device, DevicePlaybackError> {
    let candidates = host
        .output_devices()
        .map_err(|error| DevicePlaybackError::Unavailable(error.to_string()))?
        .filter_map(|device| {
            device.id().ok().map(|id| {
                let friendly = device
                    .description()
                    .ok()
                    .map(|description| description.name().to_owned())
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_default();
                (id.to_string(), friendly, device)
            })
        })
        .collect::<Vec<_>>();
    let matched = match_listed_device(&candidates, wanted).ok_or_else(|| {
        DevicePlaybackError::Unavailable(format!("no audio output named {wanted:?}"))
    })?;
    Ok(candidates
        .into_iter()
        .nth(matched)
        .expect("matched index is in range")
        .2)
}

/// List output and input devices with their default stream configurations.
///
/// Uses the playback host. Devices with unreadable configurations remain in
/// the list, with no rate or channel count.
pub fn audio_devices() -> Result<(Vec<AudioDeviceInfo>, Vec<AudioDeviceInfo>), DevicePlaybackError>
{
    with_preferred_host(|host| list_audio_devices(host, AudioDeviceDetails::DefaultConfig))
}

/// List output and input device metadata without requesting default stream configurations.
///
/// Uses the playback host and leaves rate and channel count absent. Use this
/// for periodic discovery. A backend can still inspect capabilities during
/// enumeration, but Rustel does not request a default configuration per device.
pub fn audio_device_inventory()
-> Result<(Vec<AudioDeviceInfo>, Vec<AudioDeviceInfo>), DevicePlaybackError> {
    with_preferred_host(|host| list_audio_devices(host, AudioDeviceDetails::Metadata))
}

fn list_audio_devices<H: HostTrait>(
    host: &H,
    details: AudioDeviceDetails,
) -> Result<(Vec<AudioDeviceInfo>, Vec<AudioDeviceInfo>), DevicePlaybackError> {
    let describe = |device: &H::Device, default_name: Option<&str>, input: bool| {
        let id = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "?".into());
        // The backend id is a handle, not a name a musician reads. Prefer
        // the friendly name, falling back to the id when none is known.
        let friendly = device
            .description()
            .ok()
            .map(|description| description.name().to_owned())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        let config = match details {
            AudioDeviceDetails::Metadata => None,
            AudioDeviceDetails::DefaultConfig if input => device.default_input_config().ok(),
            AudioDeviceDetails::DefaultConfig => device.default_output_config().ok(),
        };
        AudioDeviceInfo {
            is_default: default_name == Some(id.as_str()),
            sample_rate: config.as_ref().map(|c| c.sample_rate()),
            channels: config.as_ref().map(|c| c.channels()),
            name: friendly,
            id,
        }
    };
    let default_out = host
        .default_output_device()
        .and_then(|d| d.id().ok().map(|id| id.to_string()));
    let default_in = host
        .default_input_device()
        .and_then(|d| d.id().ok().map(|id| id.to_string()));

    let mut outputs = host
        .output_devices()
        .map_err(|error| DevicePlaybackError::Unavailable(error.to_string()))?
        .map(|device| describe(&device, default_out.as_deref(), false))
        .collect::<Vec<_>>();
    let mut inputs = host
        .input_devices()
        .map_err(|error| DevicePlaybackError::Unavailable(error.to_string()))?
        .map(|device| describe(&device, default_in.as_deref(), true))
        .collect::<Vec<_>>();
    disambiguate_device_names(&mut outputs);
    disambiguate_device_names(&mut inputs);
    Ok((outputs, inputs))
}

/// Match a wanted selector against enumerated `(id, friendly, device)` rows.
///
/// Order is the contract the picker and `rustel devices` rely on: the stable
/// id, then the friendly name, then the disambiguated `name (id)` spelling
/// the list prints for a repeated name, then a case-insensitive substring
/// of the friendly name. Input and output used to disagree about the
/// case-insensitive exact-name step, so `mic` could steal `Microphone`
/// ahead of `Mic` on the input side.
fn match_listed_device<T>(candidates: &[(String, String, T)], wanted: &str) -> Option<usize> {
    candidates
        .iter()
        .position(|(id, _, _)| id == wanted)
        .or_else(|| {
            candidates
                .iter()
                .position(|(id, _, _)| id.eq_ignore_ascii_case(wanted))
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|(_, friendly, _)| friendly == wanted)
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|(_, friendly, _)| friendly.eq_ignore_ascii_case(wanted))
        })
        .or_else(|| {
            candidates.iter().position(|(id, friendly, _)| {
                let disambiguated = format!("{friendly} ({id})");
                disambiguated == wanted || disambiguated.eq_ignore_ascii_case(wanted)
            })
        })
        .or_else(|| {
            let needle = wanted.to_lowercase();
            if needle.is_empty() {
                return None;
            }
            candidates.iter().position(|(_, friendly, _)| {
                !friendly.is_empty() && friendly.to_lowercase().contains(&needle)
            })
        })
}

/// Two devices with the same friendly name must still be tellable apart on
/// the picker; the second and later get their stable id appended, which the
/// device finders match back.
fn disambiguate_device_names(list: &mut [AudioDeviceInfo]) {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for device in list.iter_mut() {
        let count = seen.entry(device.name.clone()).or_insert(0);
        if *count > 0 {
            let id = device.id.clone();
            device.name = format!("{name} ({id})", name = device.name);
        }
        *count += 1;
    }
}

fn prepare_finite_inserts(
    backend: &mut ScalarBackend,
    events: &[OnsetEvent],
    sample_rate: u32,
    provider: Option<&crate::InsertProvider>,
) -> Result<(), DevicePlaybackError> {
    for event in events {
        let chain = event.controls.insert_chain();
        let slots =
            event
                .controls
                .instrument
                .iter()
                .map(|controls| (crate::instrument_slot(chain), controls))
                .chain(event.controls.effects.iter().enumerate().filter_map(
                    |(place, controls)| {
                        controls.as_ref().map(|controls| {
                            (crate::effect_slot(chain, controls.stage(place)), controls)
                        })
                    },
                ));
        for (slot, controls) in slots {
            if let Some(key) = backend.orbit_insert_key(slot) {
                if key == controls.key {
                    continue;
                }
                return Err(DevicePlaybackError::ResourceLimit(
                    "finite playback needs one instrument or effect revision per chain slot".into(),
                ));
            }
            let insert = provider
                .and_then(|build| build(controls.key, sample_rate, slot))
                .filter(|insert| insert.key() == controls.key)
                .ok_or_else(|| {
                    DevicePlaybackError::ResourceLimit(
                        "cannot prepare a finite playback instrument or effect".into(),
                    )
                })?;
            if backend.install_insert(slot, insert).is_some() {
                return Err(DevicePlaybackError::ResourceLimit(
                    "finite playback exceeds the instrument and effect chain limit".into(),
                ));
            }
        }
    }
    Ok(())
}

/// The events a finite playback still has to admit, in onset order. The
/// callback admits them as the output reaches them, the way the live ring
/// does, so native score queues stay bounded however long the list is.
pub(super) struct FiniteAdmission {
    events: Vec<OnsetEvent>,
    next: usize,
    sample_rate: u32,
}

impl FiniteAdmission {
    /// One scheduler lookahead: an onset is admitted before the block that
    /// reaches it, never after.
    pub(super) fn horizon(&self, frame: u64) -> u64 {
        frame.saturating_add((self.sample_rate as u64 / 10).max(1) + 256)
    }

    /// Admit every event due before `horizon`. An event the backend cannot
    /// take yet stays for the next block, exactly as a full live ring does.
    pub(super) fn admit_until(&mut self, backend: &mut ScalarBackend, horizon: u64) {
        while self
            .events
            .get(self.next)
            .is_some_and(|event| event.onset_frame < horizon)
        {
            let compact = self.events[self.next]
                .clone()
                .map_kabel(crate::CompactKabelControls::from);
            if !backend.try_note_prepared(compact) {
                // Admission is refused for now and retried on the next
                // block: arming before this point would queue the same
                // sidechain once per retry and flood the duck queue.
                return;
            }
            // Sidechains arm here, once per admitted event, as the live
            // path arms at ring intake: the queue drains as ducks fire,
            // and arming at the prepare pass would spend its whole
            // capacity on far-future onsets.
            backend.arm_duck_at(
                self.events[self.next].onset_frame,
                self.events[self.next].controls.duck,
            );
            self.next += 1;
        }
    }
}

pub(super) fn queue_finite_events(
    backend: &mut ScalarBackend,
    events: &[OnsetEvent],
    sample_rate: u32,
) -> Result<FiniteAdmission, DevicePlaybackError> {
    let mut queued = events.to_vec();
    queued.sort_by_key(|event| event.onset_frame);
    backend
        .prepare_finite_playback(&queued)
        .map_err(DevicePlaybackError::ResourceLimit)?;
    // Admit the first scheduler window before priming: ahead-rendering
    // inserts are primed over the notes near frame zero, so those notes
    // must already be pending, exactly as a full pre-queue left them.
    let mut admission = FiniteAdmission {
        events: queued,
        next: 0,
        sample_rate,
    };
    admission.admit_until(backend, admission.horizon(0));
    backend.prime_finite_inserts();
    Ok(admission)
}

impl ScalarDevice {
    pub fn open_default() -> Result<Self, DevicePlaybackError> {
        Self::open_output(None)
    }

    /// Open a named output, or the host default when `selector` is `None`.
    ///
    /// The name is matched exactly against the identifiers [`audio_devices`]
    /// reports, then case-insensitively, then as a substring, so a UI can pass
    /// through what a person picked from a list without normalising it.
    pub fn open_output(selector: Option<&str>) -> Result<Self, DevicePlaybackError> {
        Self::configure_output(Self::discover_output(selector)?)
    }

    fn discover_output(selector: Option<&str>) -> Result<DiscoveredOutput, DevicePlaybackError> {
        let (host_id, device) = with_preferred_host(|host| {
            let device = match selector {
                None => host.default_output_device().ok_or_else(|| {
                    DevicePlaybackError::Unavailable("no default audio output device".into())
                })?,
                Some(wanted) => find_output_device(host, wanted)?,
            };
            Ok((host.id().to_string().into_boxed_str(), device))
        })?;
        let name = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "default-output".into());
        Ok(DiscoveredOutput {
            device,
            host_id,
            name,
        })
    }

    fn configure_output(output: DiscoveredOutput) -> Result<Self, DevicePlaybackError> {
        let DiscoveredOutput {
            device,
            host_id,
            name,
        } = output;
        let config = device.default_output_config().map_err(|error| {
            DevicePlaybackError::Unavailable(format!(
                "cannot read default output configuration for {name}: {error}"
            ))
        })?;
        if config.channels() == 0 || config.sample_rate() == 0 {
            return Err(DevicePlaybackError::Unavailable(format!(
                "audio output {name} reported an invalid {}/{} configuration",
                config.sample_rate(),
                config.channels()
            )));
        }
        Ok(Self {
            device,
            config,
            host_id,
            name,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate()
    }

    pub fn channels(&self) -> u16 {
        self.config.channels()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Play a finite prepared event list, observing the caller's independent
    /// Stop flag while the callback runs.
    pub fn play(
        &self,
        events: &[OnsetEvent],
        duration_secs: f64,
        stopped: &AtomicBool,
    ) -> Result<(), DevicePlaybackError> {
        self.play_with_samples(events, duration_secs, stopped, Vec::new())
    }

    /// [`Self::play`] with decoded library samples installed into the fresh
    /// backend first. This is the finite path's equivalent of the live
    /// install ring. Without the samples, the device backend knows only
    /// the bundled `bd`.
    pub fn play_with_samples(
        &self,
        events: &[OnsetEvent],
        duration_secs: f64,
        stopped: &AtomicBool,
        samples: Vec<(crate::sample::SampleId, Box<crate::sample::DecodedSample>)>,
    ) -> Result<(), DevicePlaybackError> {
        self.play_with_samples_and_dispatch(
            events,
            duration_secs,
            stopped,
            samples,
            DspDispatch::automatic(),
        )
    }

    /// Finite playback with one selection shared by voice kernels and prepared
    /// orbit/stage reverbs. No dispatch decision is made in the callback.
    pub fn play_with_samples_and_dispatch(
        &self,
        events: &[OnsetEvent],
        duration_secs: f64,
        stopped: &AtomicBool,
        samples: Vec<(crate::sample::SampleId, Box<crate::sample::DecodedSample>)>,
        dispatch: DspDispatch,
    ) -> Result<(), DevicePlaybackError> {
        self.play_with_samples_dispatch_and_polyphony(
            events,
            duration_secs,
            stopped,
            samples,
            dispatch,
            crate::MAX_POLYPHONY,
        )
    }

    /// Finite playback with the same configurable voice policy as live audio.
    pub fn play_with_samples_dispatch_and_polyphony(
        &self,
        events: &[OnsetEvent],
        duration_secs: f64,
        stopped: &AtomicBool,
        samples: Vec<(crate::sample::SampleId, Box<crate::sample::DecodedSample>)>,
        dispatch: DspDispatch,
        max_polyphony: usize,
    ) -> Result<(), DevicePlaybackError> {
        self.play_with_samples_dispatch_polyphony_and_inserts(
            events,
            duration_secs,
            stopped,
            samples,
            dispatch,
            max_polyphony,
            None,
        )
    }

    /// Finite playback with instruments and effects prepared before streaming.
    #[allow(clippy::too_many_arguments)]
    pub fn play_with_samples_dispatch_polyphony_and_inserts(
        &self,
        events: &[OnsetEvent],
        duration_secs: f64,
        stopped: &AtomicBool,
        samples: Vec<(crate::sample::SampleId, Box<crate::sample::DecodedSample>)>,
        dispatch: DspDispatch,
        max_polyphony: usize,
        provider: Option<Arc<crate::InsertProvider>>,
    ) -> Result<(), DevicePlaybackError> {
        let total_frames = checked_frames(duration_secs, self.sample_rate())?;
        if total_frames == 0 {
            return Ok(());
        }
        if stopped.load(Ordering::Acquire) {
            return Err(DevicePlaybackError::Cancelled);
        }

        let mut backend = ScalarBackend::with_dispatch(dispatch);
        backend.set_max_polyphony(max_polyphony);
        backend.init(self.sample_rate()).map_err(|message| {
            DevicePlaybackError::Unavailable(format!("cannot initialise scalar backend: {message}"))
        })?;
        for (id, decoded) in samples {
            let _ = backend.install_sample(id, decoded);
        }
        prepare_finite_inserts(
            &mut backend,
            events,
            self.sample_rate(),
            provider.as_deref(),
        )?;
        // The event list is complete up front: synthesise every needed orbit
        // reverb HERE, off the callback, so activation never generates.
        let mut stage_reverbs: Vec<crate::reverb::ReverbParams> = Vec::new();
        for event in events {
            if let Some(reverb) = event.controls.reverb {
                let orbit = usize::from(event.controls.orbit).min(crate::scalar::MAX_ORBITS - 1);
                let params = crate::reverb::ReverbParams {
                    ir: None,
                    size_secs: reverb.size_secs,
                    fade_secs: reverb.fade_secs,
                    lp_start_hz: reverb.lp_start_hz,
                    lp_end_hz: reverb.lp_end_hz,
                };
                if backend.orbit_reverb_params(orbit) != Some(params) {
                    let _ = backend.install_reverb(
                        orbit,
                        Box::new(crate::reverb::OrbitReverb::generate_with_dispatch(
                            self.sample_rate(),
                            params,
                            dispatch,
                        )),
                    );
                }
            }
            // An `.FX()` stage's reverb is per VOICE and its impulse response
            // comes from that hap's own roomsize, so it cannot be one of the
            // fixed per-orbit set. Synthesise the distinct ones here, off the
            // callback, for the same reason: 2.7 ms at roomsize 0.5 and 31 ms
            // at 6, against the 2.7 ms a 128-frame callback has.
            for stage in event.controls.fx_stages.iter().flatten() {
                let Some(room) = stage.room else { continue };
                let params = crate::reverb::ReverbParams {
                    ir: room.ir,
                    size_secs: room.size_secs,
                    fade_secs: room.fade_secs,
                    lp_start_hz: room.lp_start_hz,
                    lp_end_hz: room.lp_end_hz,
                };
                if !stage_reverbs.contains(&params) {
                    stage_reverbs.push(params);
                    let _ = backend.install_fx_reverb(Box::new(
                        crate::reverb::OrbitReverb::generate_streaming_with_dispatch(
                            self.sample_rate(),
                            params,
                            dispatch,
                        ),
                    ));
                }
            }
        }
        backend.forbid_inline_reverb();
        let admission = queue_finite_events(&mut backend, events, self.sample_rate())?;

        let written = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = build_stream(
            &self.device,
            &self.config,
            backend,
            total_frames,
            written.clone(),
            failed.clone(),
            admission,
        )?;
        stream.play().map_err(|error| {
            DevicePlaybackError::Unavailable(format!(
                "cannot start audio output {}: {error}",
                self.name
            ))
        })?;

        // PulseAudio may accept the complete timeline into its server buffer
        // much faster than the speaker consumes it. Keep the stream alive
        // until both submission and real elapsed time reach the requested
        // window so queued audio is not truncated.
        let started = Instant::now();
        let audible_window = Duration::from_secs_f64(duration_secs);
        let progress_deadline = started
            .checked_add(audible_window.saturating_add(Duration::from_secs(3)))
            .ok_or_else(|| {
                DevicePlaybackError::Unavailable(
                    "device playback deadline is not representable".into(),
                )
            })?;
        while written.load(Ordering::Acquire) < total_frames || started.elapsed() < audible_window {
            if stopped.load(Ordering::Acquire) {
                return Err(DevicePlaybackError::Cancelled);
            }
            if failed.load(Ordering::Acquire) {
                return Err(DevicePlaybackError::Unavailable(format!(
                    "audio output {} failed while playing",
                    self.name
                )));
            }
            if Instant::now() >= progress_deadline {
                return Err(DevicePlaybackError::Unavailable(format!(
                    "audio output {} stopped requesting frames",
                    self.name
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        // The callback has filled the final host buffer, which is still queued
        // for the device. Keep the stream alive for one conservative block,
        // while still observing cancellation and device failure. This is a
        // smoke path, not a latency measurement.
        let drain_until = Instant::now() + Duration::from_millis(100);
        while Instant::now() < drain_until {
            if stopped.load(Ordering::Acquire) {
                return Err(DevicePlaybackError::Cancelled);
            }
            if failed.load(Ordering::Acquire) {
                return Err(DevicePlaybackError::Unavailable(format!(
                    "audio output {} failed while draining",
                    self.name
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
}

fn checked_frames(duration_secs: f64, sample_rate: u32) -> Result<u64, DevicePlaybackError> {
    let frames = duration_secs * f64::from(sample_rate);
    if !frames.is_finite() || frames < 0.0 || frames > u64::MAX as f64 {
        return Err(DevicePlaybackError::Unavailable(format!(
            "device duration {duration_secs}s is not representable at {sample_rate}Hz"
        )));
    }
    Ok(frames.round() as u64)
}

/// Run `use_host` on the audio host. Prefer PulseAudio when available
/// (WSLg and many desktops have no raw ALSA device), else CPAL's default.
///
/// The PulseAudio host is created once and reused. A dropped connection
/// leaks its reader thread and a 1 MiB buffer, and devices are listed
/// every few seconds. When a call fails, the function requests the device
/// list as a health probe. If the probe also fails, the function drops
/// the host and the next call reconnects. A missing device selector does
/// not discard a working connection.
#[cfg(target_os = "linux")]
fn with_preferred_host<T>(
    use_host: impl FnOnce(&cpal::Host) -> Result<T, DevicePlaybackError>,
) -> Result<T, DevicePlaybackError> {
    static PULSE: std::sync::Mutex<Option<cpal::Host>> = std::sync::Mutex::new(None);
    if cpal::available_hosts().contains(&cpal::HostId::PulseAudio) {
        let mut pulse = PULSE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pulse.is_none() {
            *pulse = cpal::host_from_id(cpal::HostId::PulseAudio).ok();
        }
        if let Some(host) = pulse.as_ref() {
            let result = use_host(host);
            return finish_cached_host_request(&mut pulse, result, |host| host.devices().is_err());
        }
    }
    use_host(&cpal::default_host())
}

#[cfg(any(target_os = "linux", test))]
fn finish_cached_host_request<H, T>(
    host: &mut Option<H>,
    result: Result<T, DevicePlaybackError>,
    disconnected: impl FnOnce(&H) -> bool,
) -> Result<T, DevicePlaybackError> {
    if result.is_err() && host.as_ref().is_some_and(disconnected) {
        *host = None;
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn with_preferred_host<T>(
    use_host: impl FnOnce(&cpal::Host) -> Result<T, DevicePlaybackError>,
) -> Result<T, DevicePlaybackError> {
    use_host(&cpal::default_host())
}

/// Build the cached playback host now, at the calling thread's priority.
///
/// The host's connection hands its work to a fresh thread that carries no
/// name, and a thread with no name inherits its creator's scheduling
/// priority and name. The audio callback later rides that thread, so a
/// host first built by a demoted background worker - a device probe -
/// puts the sound itself below the workers around it for the life of the
/// process. A caller that demotes a thread and then asks for devices must
/// prime the host first, while the thread still stands at its normal
/// priority; the helper thread here also gives the connection a name of
/// its own instead of the probe's.
pub fn prime_playback_host() {
    // A failed priming leaves the host to be built by the next caller at
    // its own priority; the probe still demotes and enumerates either way.
    let primed = std::thread::Builder::new()
        .name("audio-host".into())
        .spawn(|| {
            let _ = with_preferred_host(|_| Ok::<(), DevicePlaybackError>(()));
        });
    if let Ok(handle) = primed {
        let _ = handle.join();
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    backend: ScalarBackend,
    total_frames: u64,
    written: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    admission: FiniteAdmission,
) -> Result<cpal::Stream, DevicePlaybackError> {
    macro_rules! build {
        ($sample:ty) => {
            build_typed_stream::<$sample>(
                device,
                config.clone().into(),
                backend,
                total_frames,
                written,
                failed,
                admission,
            )
        };
    }
    match config.sample_format() {
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(U24),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        format => Err(DevicePlaybackError::Unavailable(format!(
            "audio output sample format {format} is not supported by the scalar smoke backend"
        ))),
    }
}

fn build_typed_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut backend: ScalarBackend,
    total_frames: u64,
    written: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    mut admission: FiniteAdmission,
) -> Result<cpal::Stream, DevicePlaybackError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| {
                write_output(
                    &mut backend,
                    output,
                    channels,
                    total_frames,
                    &written,
                    &mut admission,
                );
            },
            flag_stream_failure(failed),
            Some(Duration::from_secs(3)),
        )
        .map_err(|error| {
            DevicePlaybackError::Unavailable(format!("cannot build audio output stream: {error}"))
        })
}

fn build_live_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    buffer_frames: u32,
    backend: LiveScalarBackend,
    shared: LiveShared,
    gate: Arc<CallbackGate>,
) -> Result<cpal::Stream, DevicePlaybackError> {
    macro_rules! build {
        ($sample:ty) => {
            build_live_typed_stream::<$sample>(
                device,
                config.clone().into(),
                buffer_frames,
                backend,
                shared,
                gate,
            )
        };
    }
    match config.sample_format() {
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(U24),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        format => Err(DevicePlaybackError::Unavailable(format!(
            "audio output sample format {format} is not supported by live scalar playback"
        ))),
    }
}

fn build_live_typed_stream<T>(
    device: &cpal::Device,
    mut config: cpal::StreamConfig,
    buffer_frames: u32,
    mut backend: LiveScalarBackend,
    shared: LiveShared,
    gate: Arc<CallbackGate>,
) -> Result<cpal::Stream, DevicePlaybackError>
where
    T: SizedSample + FromSample<f32>,
{
    // Default PulseAudio buffering accepted tens of seconds of PCM in a few
    // wall-clock seconds on WSLg. That made both edits and the scheduler clock
    // race far ahead of what was audible. A fixed period asks CPAL backends for
    // a bounded live buffer (Pulse uses a two-period end-to-end target).
    config.buffer_size = BufferSize::Fixed(buffer_frames);
    let channels = usize::from(config.channels);
    let on_error = live_error_callback(
        Arc::clone(&gate),
        shared.failed.clone(),
        shared.callback_errors.clone(),
        shared.callback_xruns.clone(),
    );
    let mut playback_origin = None;
    // Callback-owned meter state. It belongs to the stream, not to
    // `LiveShared`, because its filters are tuned to this stream's rate.
    let sample_rate = config.sample_rate;
    let mut meters = LiveCallbackMeters::new(sample_rate, &shared.realtime_load);
    // The callback's thread is CPAL's; the one place to register it with
    // MMCSS is its first call. Held for the stream's life, reverted when
    // the closure drops with the stream.
    #[cfg(windows)]
    let mut pro_audio = CallbackProAudio(None);
    // On Linux the callback takes the class of the plugin workers at its
    // first call after a worker got it, and gives it back when the closure
    // drops with the stream.
    #[cfg(target_os = "linux")]
    let mut real_time = CallbackRealTime::new(buffer_frames, sample_rate);
    // cpal reports no underruns for a WASAPI output, so the callback counts
    // them from the playback clock.
    #[cfg(windows)]
    let mut underruns = PlaybackUnderruns::new(buffer_frames, sample_rate);
    device
        .build_output_stream(
            config,
            move |output: &mut [T], info| {
                #[cfg(windows)]
                pro_audio.attach_once();
                #[cfg(target_os = "linux")]
                real_time.promote_once(&shared.plugin_real_time);
                gate.render_or_silence(output, |output| {
                    let playback = info.timestamp().playback;
                    let origin = *playback_origin.get_or_insert(playback);
                    let origin_nanos = u64::try_from(origin.as_nanos()).unwrap_or(u64::MAX - 1);
                    shared
                        .playback_origin_nanos
                        .store(origin_nanos, Ordering::Release);
                    let elapsed = playback.saturating_duration_since(origin);
                    let elapsed_nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
                    let latency_nanos = u64::try_from(
                        playback
                            .saturating_duration_since(info.timestamp().callback)
                            .as_nanos(),
                    )
                    .unwrap_or(u64::MAX);
                    shared
                        .buffer_playback_nanos
                        .store(elapsed_nanos, Ordering::Release);
                    #[cfg(windows)]
                    underruns.observe(
                        elapsed_nanos,
                        latency_nanos,
                        output.len() / channels,
                        &shared,
                    );
                    shared
                        .playback_latency_nanos
                        .store(latency_nanos, Ordering::Release);
                    shared
                        .max_playback_latency_nanos
                        .fetch_max(latency_nanos, Ordering::Relaxed);
                    write_live_output(&mut backend, &mut meters, output, channels, &shared);
                });
            },
            on_error,
            Some(Duration::from_secs(3)),
        )
        .map_err(|error| {
            DevicePlaybackError::Unavailable(format!(
                "cannot build live audio output stream: {error}"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;

    #[test]
    fn finite_playback_keeps_worker_and_scalar_notes_on_the_original_timeline() {
        struct Insert {
            key: crate::InsertKey,
            ahead: bool,
            frame: u64,
            frontier: u64,
            due: [u64; 3],
        }
        impl crate::OrbitInsert for Insert {
            fn key(&self) -> crate::InsertKey {
                self.key
            }
            fn set_param(&mut self, _: crate::InsertParam, _: u32) {}
            fn renders_ahead(&self) -> bool {
                self.ahead
            }
            fn busy(&self) -> bool {
                true
            }
            fn prime(&mut self, frames: usize) {
                self.frontier = frames as u64;
            }
            fn score(&mut self, id: u64, frames: u32) {
                let due = &mut self.due[id as usize - 1];
                if *due == u64::MAX {
                    // A worker cannot change frames it has rendered ahead.
                    *due = (self.frame + u64::from(frames)).max(self.frontier);
                }
            }
            fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                for (left, right) in left.iter_mut().zip(right) {
                    let sounding = self
                        .due
                        .iter()
                        .any(|due| self.frame >= *due && self.frame.saturating_sub(*due) < 64);
                    *left = if sounding { 0.5 } else { 0.0 };
                    *right = *left;
                    self.frame += 1;
                }
                if self.ahead {
                    self.frontier = self.frontier.max(self.frame + 512);
                }
            }
        }
        let _guard = tripwire_assertion_guard();
        let key = crate::InsertKey {
            plugin: 3,
            preset: 1,
        };
        let mut events: Vec<_> = [0, 128, 2048]
            .into_iter()
            .enumerate()
            .map(|(index, onset)| {
                let mut event = OnsetEvent::new(onset, 440.0, 1.0, 0.001);
                event.controls.insert_chain = Some(1);
                event.controls.instrument =
                    Some(crate::InsertControls::new(key).with_score(index as u64 + 1));
                event
            })
            .collect();
        events.push(OnsetEvent::new(64, 220.0, 0.1, 0.001));
        let mut kabel = OnsetEvent::new(256, 440.0, 0.2, 0.004);
        kabel.controls.kabel = Some(crate::KabelControls {
            program: rustel_kabel::Program::from_json(
                r#"{"nodes":[{"type":"n","value":0.25},{"type":"output","inputs":[0]}]}"#,
            )
            .unwrap()
            .into(),
            start_secs: 256.0 / 48_000.0,
            gate_end_secs: 448.0 / 48_000.0,
            end_secs: 448.0 / 48_000.0,
        });
        events.push(kabel);
        let total = 2304;
        let render = |ahead| {
            let mut backend = ScalarBackend::prepared(48_000, 8).unwrap();
            assert!(
                backend
                    .install_insert(
                        crate::instrument_slot(1),
                        Box::new(Insert {
                            key,
                            ahead,
                            frame: 0,
                            frontier: 0,
                            due: [u64::MAX; 3],
                        })
                    )
                    .is_none()
            );
            let mut admission = queue_finite_events(&mut backend, &events, 48_000).unwrap();
            let written = AtomicU64::new(0);
            let mut output = vec![0.0f32; total as usize * 2];
            let before = crate::tripwire::Violations::capture();
            crate::tripwire::audio_scope(|| {
                super::super::callback::write_output(
                    &mut backend,
                    &mut output,
                    2,
                    total,
                    &written,
                    &mut admission,
                );
            });
            assert!(crate::tripwire::Violations::capture().since(before).clean());
            assert_eq!(written.load(Ordering::Acquire), total);
            output
        };
        let expected = render(false);
        let actual = render(true);
        assert_eq!(actual, expected);
        for onset in [0, 128, 256, 2048] {
            assert!(
                actual[onset * 2..(onset + 64) * 2]
                    .iter()
                    .any(|v| v.abs() > 0.01)
            );
        }
    }

    /// A finite playback longer than the pending-event ceiling still sounds
    /// every event: the callback admits from the timeline as the output
    /// reaches it, so the short events drain and the final tone is
    /// retained. The backend is built the way production builds it, from
    /// `with_dispatch`, whose pools start empty: the producer reserves what
    /// the in-callback admission needs.
    #[test]
    fn finite_playback_admits_events_past_the_pending_ceiling() {
        let _guard = tripwire_assertion_guard();
        let spacing = 64u64;
        let mut events: Vec<_> = (0..crate::scalar::MAX_PENDING_EVENTS as u64)
            .map(|index| OnsetEvent::new(index * spacing, 440.0, 0.05, 0.001))
            .collect();
        let tone_at = crate::scalar::MAX_PENDING_EVENTS as u64 * spacing + 128;
        events.push(OnsetEvent::new(tone_at, 220.0, 0.8, 0.25));
        let total = tone_at + 24_000;

        let mut backend = ScalarBackend::with_dispatch(DspDispatch::portable());
        backend.init(48_000).unwrap();
        let mut admission = queue_finite_events(&mut backend, &events, 48_000).unwrap();
        let written = AtomicU64::new(0);
        let mut output = Vec::new();
        while written.load(Ordering::Acquire) < total {
            let mut block = [0.0f32; 128 * 2];
            let before = crate::tripwire::Violations::capture();
            crate::tripwire::audio_scope(|| {
                super::super::callback::write_output(
                    &mut backend,
                    &mut block,
                    2,
                    total,
                    &written,
                    &mut admission,
                );
            });
            assert!(
                crate::tripwire::Violations::capture().since(before).clean(),
                "in-callback admission allocated"
            );
            output.extend_from_slice(&block);
        }
        let tone = &output[tone_at as usize * 2..];
        assert!(
            tone.iter().any(|sample| sample.abs() > 0.01),
            "the tone after a full pending queue of short events must still sound"
        );
    }

    /// Sidechains arm at admission, as the live path arms at ring intake:
    /// with a list longer than the duck queue's capacity, a trigger past
    /// the old full-queue drop still dips its target orbit.
    #[test]
    fn finite_playback_arms_ducks_past_the_queue_capacity() {
        let _guard = tripwire_assertion_guard();
        let duck = crate::DuckControls {
            targets: [
                Some(crate::DuckTarget {
                    orbit: 2,
                    onset_secs: 0.0,
                    attack_secs: 0.05,
                    depth: 1.0,
                }),
                None,
                None,
                None,
            ],
        };
        let controls = |orbit: u8, duck: Option<crate::DuckControls>| crate::OscillatorControls {
            orbit,
            duck,
            envelope: crate::Envelope {
                attack_secs: 0.001,
                decay_secs: 0.0,
                sustain: 1.0,
                release_secs: 0.01,
            },
            ..Default::default()
        };
        // A sustained tone on orbit 2, and more silent duck triggers than
        // the 4096-entry sidechain queue holds, spaced far enough apart
        // that the gain recovers to unity between dips.
        let tone_duration = (4500u64 * 4800 + 12_000) as f32 / 48_000.0;
        let tone = OnsetEvent::new(0, 220.0, 1.0, tone_duration).with_controls(controls(2, None));
        let count = 4500u64;
        let spacing = 4800u64;
        let last_onset = count * spacing;
        let triggers = (0..count).map(|index| {
            OnsetEvent::new(index * spacing + 2400, 220.0, 0.0, 0.01)
                .with_controls(controls(1, Some(duck)))
        });
        let mut events = vec![tone];
        events.extend(triggers);
        let total = last_onset + 12_000;

        let mut backend = ScalarBackend::with_dispatch(DspDispatch::portable());
        backend.init(48_000).unwrap();
        let mut admission = queue_finite_events(&mut backend, &events, 48_000).unwrap();
        let written = AtomicU64::new(0);
        let mut output = vec![0.0f32; 0];
        while written.load(Ordering::Acquire) < total {
            let mut block = [0.0f32; 128 * 2];
            crate::tripwire::audio_scope(|| {
                super::super::callback::write_output(
                    &mut backend,
                    &mut block,
                    2,
                    total,
                    &written,
                    &mut admission,
                );
            });
            output.extend_from_slice(&block);
        }
        let rms = |from: usize, to: usize| {
            let slice = &output[from * 2..to * 2];
            (slice
                .iter()
                .map(|s| f64::from(*s) * f64::from(*s))
                .sum::<f64>()
                / slice.len() as f64)
                .sqrt()
        };
        let last_trigger = (count - 1) * spacing + 2400;
        let before = rms(
            (last_trigger - 2_000) as usize,
            (last_trigger - 100) as usize,
        );
        let dipped = rms((last_trigger + 50) as usize, (last_trigger + 600) as usize);
        assert!(
            dipped < before * 0.5,
            "the trigger past the queue capacity must still dip the orbit:              before {before:.5}, at the last trigger {dipped:.5}"
        );
    }

    #[test]
    fn finite_inserts_prepare_each_slot_once_and_refuse_unavailable_revisions() {
        struct Insert(crate::InsertKey);
        impl crate::OrbitInsert for Insert {
            fn key(&self) -> crate::InsertKey {
                self.0
            }
            fn set_param(&mut self, _: crate::InsertParam, _: u32) {}
            fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
        }

        let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = built.clone();
        let provider = move |key, rate, _| -> Option<Box<dyn crate::OrbitInsert>> {
            assert_eq!(rate, 48_000);
            count.fetch_add(1, Ordering::Relaxed);
            Some(Box::new(Insert(key)))
        };
        let key = crate::InsertKey {
            plugin: 3,
            preset: 1,
        };
        let mut event = OnsetEvent::new(0, 440.0, 1.0, 0.1);
        event.controls.instrument = Some(crate::InsertControls::new(key).with_score(7));
        // The first effect goes to the slot of its stage, 2.
        let mut effect = crate::InsertControls::new(key);
        effect.set_stage(2);
        event.controls.effects[0] = Some(effect);
        let mut backend = ScalarBackend::new();
        backend.init(48_000).unwrap();
        assert!(matches!(
            prepare_finite_inserts(&mut backend, &[event.clone()], 48_000, None),
            Err(DevicePlaybackError::ResourceLimit(_))
        ));
        prepare_finite_inserts(
            &mut backend,
            &[event.clone(), event.clone()],
            48_000,
            Some(&provider),
        )
        .unwrap();
        assert_eq!(built.load(Ordering::Relaxed), 2);
        assert_eq!(
            backend.orbit_insert_key(crate::instrument_slot(event.controls.insert_chain())),
            Some(key)
        );
        assert_eq!(
            backend.orbit_insert_key(crate::effect_slot(event.controls.insert_chain(), 2)),
            Some(key)
        );
        event.controls.instrument = Some(crate::InsertControls::new(crate::InsertKey {
            preset: 2,
            ..key
        }));
        assert!(matches!(
            prepare_finite_inserts(&mut backend, &[event], 48_000, Some(&provider)),
            Err(DevicePlaybackError::ResourceLimit(_))
        ));
        assert_eq!(built.load(Ordering::Relaxed), 2);
    }

    mod host_cache {
        use super::*;
        use std::rc::Rc;

        #[test]
        fn missing_selector_keeps_the_same_connected_host_across_retries() {
            let connection = Rc::new(());
            let mut cached = Some(Rc::clone(&connection));
            let mut probes = 0;

            for _ in 0..16 {
                let request =
                    Err::<(), _>(DevicePlaybackError::Unavailable("missing input".into()));
                let result = finish_cached_host_request(&mut cached, request, |_| {
                    probes += 1;
                    false
                });

                assert!(
                    matches!(result, Err(DevicePlaybackError::Unavailable(message)) if message == "missing input")
                );
                assert!(Rc::ptr_eq(
                    cached.as_ref().expect("cached host"),
                    &connection
                ));
                assert_eq!(Rc::strong_count(&connection), 2);
            }
            assert_eq!(probes, 16);
        }

        #[test]
        fn failed_connection_is_released_without_replacing_the_request_error() {
            let connection = Rc::new(());
            let mut cached = Some(Rc::clone(&connection));
            let request = Err::<(), _>(DevicePlaybackError::Unavailable(
                "device request failed".into(),
            ));

            let result = finish_cached_host_request(&mut cached, request, |_| true);

            assert!(
                matches!(result, Err(DevicePlaybackError::Unavailable(message)) if message == "device request failed")
            );
            assert!(cached.is_none(), "the next request must create a new host");
            assert_eq!(Rc::strong_count(&connection), 1);
        }

        #[test]
        fn successful_request_keeps_the_host_without_a_health_probe() {
            let connection = Rc::new(());
            let mut cached = Some(Rc::clone(&connection));

            let result = finish_cached_host_request(&mut cached, Ok(42), |_| {
                panic!("a successful request does not need a health probe")
            });

            assert_eq!(result.expect("successful request"), 42);
            assert!(Rc::ptr_eq(
                cached.as_ref().expect("cached host"),
                &connection
            ));
            assert_eq!(Rc::strong_count(&connection), 2);
        }
    }
    mod inventory {
        use super::*;
        use std::cell::Cell;
        use std::hash::{Hash, Hasher};
        use std::rc::Rc;

        #[derive(Debug, Default)]
        struct ConfigProbes {
            input: Cell<usize>,
            output: Cell<usize>,
        }

        #[derive(Clone, Debug)]
        struct ListedDevice {
            id: &'static str,
            name: &'static str,
            input: bool,
            output: bool,
            config_available: bool,
            probes: Rc<ConfigProbes>,
        }

        impl ListedDevice {
            fn new(id: &'static str, name: &'static str, input: bool, output: bool) -> Self {
                Self {
                    id,
                    name,
                    input,
                    output,
                    config_available: true,
                    probes: Rc::default(),
                }
            }

            fn listed_id(&self) -> String {
                self.id().expect("fake device id").to_string()
            }

            fn default_config(
                &self,
                input: bool,
            ) -> Result<cpal::SupportedStreamConfig, cpal::Error> {
                let count = if input {
                    &self.probes.input
                } else {
                    &self.probes.output
                };
                count.set(count.get() + 1);
                if !self.config_available {
                    return Err(cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable));
                }
                Ok(cpal::SupportedStreamConfig::new(
                    if input { 1 } else { 2 },
                    if input { 44_100 } else { 48_000 },
                    SupportedBufferSize::Unknown,
                    SampleFormat::F32,
                ))
            }

            fn assert_probes(&self, input: usize, output: usize) {
                assert_eq!(self.probes.input.get(), input, "{} input probes", self.id);
                assert_eq!(
                    self.probes.output.get(),
                    output,
                    "{} output probes",
                    self.id
                );
            }
        }

        impl PartialEq for ListedDevice {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }

        impl Eq for ListedDevice {}

        impl Hash for ListedDevice {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.id.hash(state);
            }
        }

        impl std::fmt::Display for ListedDevice {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.name)
            }
        }

        impl DeviceTrait for ListedDevice {
            type SupportedInputConfigs = std::vec::IntoIter<cpal::SupportedStreamConfigRange>;
            type SupportedOutputConfigs = std::vec::IntoIter<cpal::SupportedStreamConfigRange>;
            type Stream = cpal::Stream;

            fn description(&self) -> Result<cpal::DeviceDescription, cpal::Error> {
                Ok(cpal::DeviceDescriptionBuilder::new(self.name).build())
            }

            fn id(&self) -> Result<cpal::DeviceId, cpal::Error> {
                Ok(cpal::DeviceId::new(cpal::ALL_HOSTS[0], self.id))
            }

            // ALSA determines direction from device metadata during enumeration.
            fn supports_input(&self) -> bool {
                self.input
            }

            fn supports_output(&self) -> bool {
                self.output
            }

            #[expect(
                clippy::panic_in_result_fn,
                reason = "the stub host does not implement this"
            )]
            fn supported_input_configs(&self) -> Result<Self::SupportedInputConfigs, cpal::Error> {
                panic!("listing must not request input formats from the fake host")
            }

            #[expect(
                clippy::panic_in_result_fn,
                reason = "the stub host does not implement this"
            )]
            fn supported_output_configs(
                &self,
            ) -> Result<Self::SupportedOutputConfigs, cpal::Error> {
                panic!("listing must not request output formats from the fake host")
            }

            fn default_input_config(&self) -> Result<cpal::SupportedStreamConfig, cpal::Error> {
                self.default_config(true)
            }

            fn default_output_config(&self) -> Result<cpal::SupportedStreamConfig, cpal::Error> {
                self.default_config(false)
            }

            #[expect(
                clippy::panic_in_result_fn,
                reason = "the stub host does not implement this"
            )]
            fn build_input_stream_raw<D, E>(
                &self,
                _config: cpal::StreamConfig,
                _sample_format: SampleFormat,
                _data_callback: D,
                _error_callback: E,
                _timeout: Option<Duration>,
            ) -> Result<Self::Stream, cpal::Error>
            where
                D: FnMut(&cpal::Data, &cpal::InputCallbackInfo) + Send + 'static,
                E: FnMut(cpal::Error) + Send + 'static,
            {
                panic!("listing must not open an input stream")
            }

            #[expect(
                clippy::panic_in_result_fn,
                reason = "the stub host does not implement this"
            )]
            fn build_output_stream_raw<D, E>(
                &self,
                _config: cpal::StreamConfig,
                _sample_format: SampleFormat,
                _data_callback: D,
                _error_callback: E,
                _timeout: Option<Duration>,
            ) -> Result<Self::Stream, cpal::Error>
            where
                D: FnMut(&mut cpal::Data, &cpal::OutputCallbackInfo) + Send + 'static,
                E: FnMut(cpal::Error) + Send + 'static,
            {
                panic!("listing must not open an output stream")
            }
        }

        #[derive(Default)]
        struct InventoryHost {
            devices: Vec<ListedDevice>,
            default_input: Option<&'static str>,
            default_output: Option<&'static str>,
            unavailable: bool,
        }

        impl HostTrait for InventoryHost {
            type Devices = std::vec::IntoIter<ListedDevice>;
            type Device = ListedDevice;

            fn is_available() -> bool {
                true
            }

            fn devices(&self) -> Result<Self::Devices, cpal::Error> {
                if self.unavailable {
                    return Err(cpal::Error::new(cpal::ErrorKind::HostUnavailable));
                }
                Ok(self.devices.clone().into_iter())
            }

            fn default_input_device(&self) -> Option<Self::Device> {
                self.devices
                    .iter()
                    .find(|device| Some(device.id) == self.default_input)
                    .cloned()
            }

            fn default_output_device(&self) -> Option<Self::Device> {
                self.devices
                    .iter()
                    .find(|device| Some(device.id) == self.default_output)
                    .cloned()
            }
        }

        #[test]
        fn repeated_inventory_keeps_metadata_without_default_config_probes() {
            let duplex = ListedDevice::new("duplex", "Interface", true, true);
            let output = ListedDevice::new("output", "Interface", false, true);
            let input = ListedDevice::new("input", " ", true, false);
            let host = InventoryHost {
                devices: vec![duplex.clone(), output.clone(), input.clone()],
                default_input: Some(input.id),
                default_output: Some(duplex.id),
                ..Default::default()
            };

            for _ in 0..3 {
                let (outputs, inputs) =
                    list_audio_devices(&host, AudioDeviceDetails::Metadata).unwrap();
                assert_eq!(outputs.len(), 2);
                assert_eq!(inputs.len(), 2);
                assert_eq!(outputs[0].id, duplex.listed_id());
                assert_eq!(outputs[1].id, output.listed_id());
                assert_eq!(outputs[0].name, "Interface");
                assert_eq!(
                    outputs[1].name,
                    format!("Interface ({})", output.listed_id())
                );
                assert!(outputs[0].is_default);
                assert!(!outputs[1].is_default);
                assert_eq!(inputs[0].id, duplex.listed_id());
                assert_eq!(inputs[0].name, "Interface");
                assert!(!inputs[0].is_default);
                assert_eq!(inputs[1].id, input.listed_id());
                assert_eq!(inputs[1].name, input.listed_id());
                assert!(inputs[1].is_default);
                for device in outputs.iter().chain(&inputs) {
                    assert_eq!(device.sample_rate, None);
                    assert_eq!(device.channels, None);
                }
            }
            duplex.assert_probes(0, 0);
            output.assert_probes(0, 0);
            input.assert_probes(0, 0);
        }

        #[test]
        fn detailed_listing_reads_each_direction_and_keeps_unreadable_devices() {
            let duplex = ListedDevice::new("duplex", "Interface", true, true);
            let mut unavailable = ListedDevice::new("unavailable", "Busy output", false, true);
            unavailable.config_available = false;
            let host = InventoryHost {
                devices: vec![duplex.clone(), unavailable.clone()],
                default_input: Some(duplex.id),
                default_output: Some(duplex.id),
                ..Default::default()
            };

            let (outputs, inputs) =
                list_audio_devices(&host, AudioDeviceDetails::DefaultConfig).unwrap();
            assert_eq!(outputs.len(), 2);
            assert_eq!(inputs.len(), 1);
            assert_eq!(outputs[0].sample_rate, Some(48_000));
            assert_eq!(outputs[0].channels, Some(2));
            assert!(outputs[0].is_default);
            assert_eq!(inputs[0].sample_rate, Some(44_100));
            assert_eq!(inputs[0].channels, Some(1));
            assert!(inputs[0].is_default);
            assert_eq!(outputs[1].id, unavailable.listed_id());
            assert_eq!(outputs[1].name, "Busy output");
            assert_eq!(outputs[1].sample_rate, None);
            assert_eq!(outputs[1].channels, None);
            duplex.assert_probes(1, 1);
            unavailable.assert_probes(0, 1);
        }

        #[test]
        fn inventory_refresh_observes_hotplug_and_default_changes() {
            let old = ListedDevice::new("old", "Old output", false, true);
            let new = ListedDevice::new("new", "New interface", true, true);
            let mut host = InventoryHost {
                devices: vec![old.clone()],
                default_output: Some(old.id),
                ..Default::default()
            };
            let (outputs, inputs) =
                list_audio_devices(&host, AudioDeviceDetails::Metadata).unwrap();
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].id, old.listed_id());
            assert!(outputs[0].is_default);
            assert!(inputs.is_empty());

            host.devices = vec![new.clone()];
            host.default_input = Some(new.id);
            host.default_output = Some(new.id);
            let (outputs, inputs) =
                list_audio_devices(&host, AudioDeviceDetails::Metadata).unwrap();
            assert_eq!(outputs.len(), 1);
            assert_eq!(inputs.len(), 1);
            assert_eq!(outputs[0].id, new.listed_id());
            assert_eq!(inputs[0].id, new.listed_id());
            assert!(outputs[0].is_default);
            assert!(inputs[0].is_default);
            old.assert_probes(0, 0);
            new.assert_probes(0, 0);
        }

        #[test]
        fn inventory_reports_host_enumeration_failure() {
            let host = InventoryHost {
                unavailable: true,
                ..Default::default()
            };
            assert!(matches!(
                list_audio_devices(&host, AudioDeviceDetails::Metadata),
                Err(DevicePlaybackError::Unavailable(_))
            ));
        }
    }
    mod output_activation {
        use super::*;

        #[cfg(feature = "test-support")]
        fn resume_tone(frame: u64) -> AudioEvent {
            AudioEvent {
                onset_id: frame + 1,
                generation: 7,
                target_frame: frame,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn limiter_controls_keep_callback_output_continuous_without_allocating() {
            let _tripwire_guard = tripwire_assertion_guard();
            let mut manual = ManualLiveOutput::new(48_000, 7).expect("manual output");
            let mut event = resume_tone(0);
            event.synth = Some(crate::SynthSource::One);
            event.gain = 2.0;
            event.duration_secs = 2.0;
            assert!(manual.device().push(event));
            let mut output = [0.0f32; 256 * 2];
            for _ in 0..16 {
                manual.render(&mut output);
            }
            let mut previous = output[output.len() - 2];
            assert!(previous > 0.1);
            let before = tripwire::Violations::capture();
            for (threshold_db, character, makeup) in [
                (-18.0, crate::limiter::Character::Transparent, false),
                (-6.0, crate::limiter::Character::Hard, true),
                (-12.0, crate::limiter::Character::Warm, true),
                (-3.0, crate::limiter::Character::Punchy, false),
                (f32::NAN, crate::limiter::Character::Transparent, false),
            ] {
                manual.device().set_limiter(Some(crate::LimiterSettings {
                    threshold_db,
                    character,
                }));
                manual.device().set_limiter_makeup(makeup);
                for _ in 0..8 {
                    manual.render(&mut output);
                    for frame in output.as_chunks::<2>().0 {
                        assert!(frame[0].is_finite() && frame[0] > 0.01);
                        assert!(frame[0] <= 1.0);
                        assert!((frame[0] - previous).abs() < 0.01);
                        assert_eq!(frame[0], frame[1]);
                        previous = frame[0];
                    }
                }
            }
            assert!(tripwire::Violations::capture().since(before).clean());
            assert_eq!(manual.device().report().callback_scope_misses, 0);
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn manual_output_renders_queued_audio_and_acknowledges_stop() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            let mut manual = ManualLiveOutput::new(48_000, 7).expect("manual output");
            let device = manual.device();
            assert!(device.is_silent());
            assert_eq!(device.clock_frames(), 0);
            assert!(device.push(AudioEvent {
                onset_id: 1,
                generation: 7,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            assert_eq!(device.report().callbacks, 0);
            assert_eq!(device.report().ring_depth, 1);

            let mut output = [0.0f32; 256 * 2];
            let before = tripwire::Violations::capture();
            manual.render(&mut output);
            assert!(tripwire::Violations::capture().since(before).clean());
            assert!(output.iter().all(|sample| sample.is_finite()));
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
            let report = manual.device().report();
            assert_eq!(report.callbacks, 1);
            assert_eq!(report.submitted_frames, 256);
            assert_eq!(report.accepted_events, 1);
            assert_eq!(report.ring_depth, 0);
            assert_eq!(report.ring_role_conflicts, 0);
            assert_eq!(report.callback_scope_misses, 0);

            manual.device().stop();
            assert!(!manual.device().report().stop_acknowledged);
            let before = tripwire::Violations::capture();
            for buffer in 0..3 {
                manual.render(&mut output);
                // The first 256 frames sit inside the 480-frame ramp.
                assert_eq!(manual.device().report().stop_acknowledged, buffer > 0);
            }
            assert!(tripwire::Violations::capture().since(before).clean());
            let report = manual.device().report();
            assert!(report.stop_acknowledged);
            assert_eq!(report.callbacks, 4);
            assert_eq!(report.submitted_frames, 1024);
            assert_eq!(report.accepted_events, 1);
            assert!(output.iter().all(|sample| *sample == 0.0));
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn startup_mute_applies_before_piano_and_routed_audio() {
            let _tripwire_guard = tripwire_assertion_guard();
            for piano in [false, true] {
                let shared = LiveShared::new(7);
                shared.orbit_pairs[1].store(1, Ordering::Relaxed);
                let mut backend =
                    LiveScalarBackend::with_dispatch(48_000, 4, DspDispatch::portable())
                        .expect("portable backend");
                let mut meters = test_meter();
                let mut output = [0.0f32; 256 * 4];
                let mut render = |output: &mut [f32]| {
                    let before = tripwire::Violations::capture();
                    tripwire::audio_scope(|| {
                        write_live_output(&mut backend, &mut meters, output, 4, &shared);
                    });
                    assert!(tripwire::Violations::capture().since(before).clean());
                };
                render(&mut output);
                assert!(output.iter().all(|sample| *sample == 0.0));

                shared.meter.set_gain(0.0);
                let mut note = resume_tone(256);
                note.controls.piano = piano;
                note.controls.orbit = 1;
                note.synth = Some(crate::SynthSource::One);
                assert!(shared.immediate.push(note));
                for _ in 0..3 {
                    render(&mut output);
                    assert!(output.iter().all(|sample| *sample == 0.0));
                }
                assert_eq!(shared.accepted_events.load(Ordering::Relaxed), 1);
                shared.meter.set_gain(1.0);
                render(&mut output);
                assert!(output[..4].iter().all(|sample| sample.abs() < 0.005));
                render(&mut output);
                assert!(output.iter().any(|sample| sample.abs() > 0.01));
            }
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn a_resumed_output_serves_sound_again_after_a_stop() {
            let _tripwire_guard = tripwire_assertion_guard();
            let mut manual = ManualLiveOutput::new(48_000, 7).expect("manual output");
            let mut output = [0.0f32; 256 * 2];
            assert!(manual.device().push(resume_tone(0)));
            manual.render(&mut output);
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));

            assert!(manual.device().push(resume_tone(2048)));
            assert!(manual.device().push_immediate(resume_tone(256)));
            manual.device().stop();
            for _ in 0..3 {
                manual.render(&mut output);
            }
            assert!(manual.device().report().stop_acknowledged);
            assert!(output.iter().all(|sample| *sample == 0.0));

            manual.device().resume();
            assert!(!manual.device().report().stop_acknowledged);
            for _ in 0..6 {
                manual.render(&mut output);
                assert!(output.iter().all(|sample| *sample == 0.0));
            }
            assert_eq!(manual.device().report().accepted_events, 1);
            assert_eq!(manual.device().report().ring_depth, 0);

            let mut piano = resume_tone(manual.device().clock_frames());
            piano.controls.piano = true;
            assert!(manual.device().push_immediate(piano));
            manual.render(&mut output);
            assert!(
                output.iter().any(|sample| sample.abs() > 1e-6),
                "the resumed stream renders the new note"
            );
            manual.device().resume();
            manual.device().resume();
            manual.render(&mut output);
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
            assert_eq!(manual.device().report().accepted_events, 2);

            manual.device().stop();
            manual.render(&mut output);
            assert!(!manual.device().report().stop_acknowledged);
            manual.render(&mut output);
            assert!(manual.device().report().stop_acknowledged);
        }

        /// A resumed output keeps the inserts of its chains. Each chain is
        /// silent until its next note. When an insert keeps a tail through
        /// its reset, the next note moves the level up from 0. The chains
        /// run on plugin workers.
        #[cfg(feature = "test-support")]
        #[test]
        fn a_resumed_output_silences_a_kept_tail_until_the_next_note() {
            /// Rings at 0.5 after a note or any input, also after a reset.
            struct Ringing(crate::insert::InsertKey, f32);
            impl crate::insert::OrbitInsert for Ringing {
                fn key(&self) -> crate::insert::InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: crate::insert::InsertParam, _: u32) {}
                fn note(&mut self, _: crate::insert::InsertNote, _: u32) {
                    self.1 = 0.5;
                }
                fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                    for (left, right) in left.iter_mut().zip(right) {
                        if *left != 0.0 || *right != 0.0 {
                            self.1 = 0.5;
                        }
                        (*left, *right) = (self.1, self.1);
                    }
                }
                fn any_thread(&self) -> bool {
                    true
                }
            }
            let _tripwire_guard = tripwire_assertion_guard();
            crate::set_plugin_pool_for_test(Some(2));
            let key = crate::insert::InsertKey {
                plugin: 1,
                preset: 0,
            };
            let instrument_key = crate::insert::InsertKey {
                plugin: 2,
                preset: 0,
            };
            for instrument in [false, true] {
                let mut manual = ManualLiveOutput::new(48_000, 7).expect("manual output");
                manual.device().set_insert_provider(Arc::new(|key, _, _| {
                    Some(Box::new(Ringing(key, 0.0)) as Box<_>)
                }));
                let note = |frame| {
                    let mut event = resume_tone(frame);
                    event.controls.effects[0] = Some(crate::insert::InsertControls::new(key));
                    if instrument {
                        let note = crate::insert::InsertNote {
                            pitch: 69.0,
                            velocity: 1.0,
                            frames: 48,
                        };
                        event.controls.instrument = Some(
                            crate::insert::InsertControls::new(instrument_key).with_note(note),
                        );
                    }
                    event
                };
                LiveReverbBatch::install_inserts(manual.device(), &[note(0)]);
                assert!(manual.device().push(note(0)));
                let mut output = [0.0f32; 256 * 2];
                manual.render(&mut output);
                assert!(
                    output[510] > 0.4,
                    "instrument {instrument}: the insert rings"
                );

                manual.device().stop();
                for _ in 0..3 {
                    manual.render(&mut output);
                }
                assert!(manual.device().report().stop_acknowledged);
                manual.device().resume();
                for _ in 0..4 {
                    manual.render(&mut output);
                    assert!(
                        output.iter().all(|sample| *sample == 0.0),
                        "instrument {instrument}: the kept tail sounds after the resume"
                    );
                }

                // The next note is 128 frames into the block.
                assert!(
                    manual
                        .device()
                        .push(note(manual.device().clock_frames() + 128))
                );
                manual.render(&mut output);
                let left: Vec<f32> = output.iter().step_by(2).copied().collect();
                assert!(left[..128].iter().all(|sample| *sample == 0.0));
                assert!(
                    left[128].abs() < 0.01,
                    "instrument {instrument}: a step at the next note: {}",
                    left[128]
                );
                manual.render(&mut output);
                assert!(
                    output[510] > 0.4,
                    "instrument {instrument}: the next note plays through the insert"
                );

                // A note at the first frame after the resume also moves the
                // level up from 0.
                manual.device().stop();
                for _ in 0..3 {
                    manual.render(&mut output);
                }
                assert!(manual.device().report().stop_acknowledged);
                manual.device().resume();
                assert!(manual.device().push(note(manual.device().clock_frames())));
                manual.render(&mut output);
                let left: Vec<f32> = output.iter().step_by(2).copied().collect();
                assert!(
                    left[0].abs() < 0.01,
                    "instrument {instrument}: a step at the first frame: {}",
                    left[0]
                );
                assert!(
                    left.windows(2).all(|pair| (pair[1] - pair[0]).abs() < 0.1),
                    "instrument {instrument}: a step after the resume"
                );
                assert!(
                    manual
                        .device()
                        .holds_insert(crate::insert::effect_slot(1, 0), key)
                );
                if instrument {
                    assert!(
                        manual
                            .device()
                            .holds_insert(crate::insert::instrument_slot(1), instrument_key)
                    );
                }
                let report = manual.device().report();
                assert_eq!(report.callback_scope_misses, 0);
                assert_eq!(report.plugin_workers, 2);
            }
            crate::set_plugin_pool_for_test(None);
        }

        /// Passes its input on after the work `spin` asks for its next
        /// block, the work `steady` asks for each block and the time off the
        /// CPU `pause` asks for, in microseconds. It runs on any thread, and
        /// counts its blocks on a plugin worker in `away`.
        #[cfg(feature = "test-support")]
        struct Spinning {
            key: crate::insert::InsertKey,
            spin: Arc<AtomicU64>,
            steady: Arc<AtomicU64>,
            pause: Arc<AtomicU64>,
            away: Arc<AtomicU64>,
        }

        #[cfg(feature = "test-support")]
        impl crate::insert::OrbitInsert for Spinning {
            fn key(&self) -> crate::insert::InsertKey {
                self.key
            }
            fn set_param(&mut self, _: crate::insert::InsertParam, _: u32) {}
            fn process(&mut self, _: &mut [f32], _: &mut [f32]) {
                let spin = self.spin.swap(0, Ordering::SeqCst) + self.steady.load(Ordering::SeqCst);
                let spin = Duration::from_micros(spin);
                let start = Instant::now();
                while start.elapsed() < spin {
                    std::hint::spin_loop();
                }
                let pause = Duration::from_micros(self.pause.swap(0, Ordering::SeqCst));
                if !pause.is_zero() {
                    std::thread::sleep(pause);
                }
                if crate::pool::on_plugin_worker() {
                    self.away.fetch_add(1, Ordering::SeqCst);
                }
            }
            fn any_thread(&self) -> bool {
                true
            }
        }

        /// Builds a [`Spinning`] copy for each key, with `spin`, `steady`,
        /// `pause` and `away`.
        #[cfg(feature = "test-support")]
        fn spinning(
            spin: &Arc<AtomicU64>,
            steady: &Arc<AtomicU64>,
            pause: &Arc<AtomicU64>,
            away: &Arc<AtomicU64>,
        ) -> Arc<crate::insert::InsertProvider> {
            let (spin, steady) = (Arc::clone(spin), Arc::clone(steady));
            let (pause, away) = (Arc::clone(pause), Arc::clone(away));
            Arc::new(move |key, _, _| {
                Some(Box::new(Spinning {
                    key,
                    spin: Arc::clone(&spin),
                    steady: Arc::clone(&steady),
                    pause: Arc::clone(&pause),
                    away: Arc::clone(&away),
                }))
            })
        }

        /// The plugin workers run above half load. A light set posts no
        /// job, also one that spends 13% of each sub-block. After a
        /// sub-block, a buffer past 80% of its time so far opens the gate for
        /// its next sub-block. A buffer past 80% keeps the gate open for the
        /// next buffer. The buffer that takes the workers opens it only at
        /// its end. The average opens the gate past half load. The gate
        /// closes when the average falls below 35%, and a stop leaves it as
        /// it was.
        #[cfg(feature = "test-support")]
        #[test]
        fn a_light_set_posts_no_job_and_a_heavy_buffer_turns_the_pool_on() {
            let _tripwire_guard = tripwire_assertion_guard();
            // At 8 kHz a sub-block of 128 frames lasts 16 ms, so a light set
            // stays far below each threshold, also after its first sub-block.
            let mut manual = ManualLiveOutput::new(8_000, 7).expect("manual output");
            // Workers with the real gate. Only a worker runs the job of the
            // chain.
            let pool = crate::pool::PluginPool::start_test_pool(2, false).expect("a pool");
            let shared = Arc::clone(pool.shared());
            for bus in 0..crate::insert::MAX_INSERT_BUSES {
                shared.claim_before_join(crate::pool::JobKind::Effect, bus);
            }
            *manual.device().plugin_pool.lock().unwrap() = Some(pool);
            let (spin, away) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
            let steady = Arc::new(AtomicU64::new(0));
            manual
                .device()
                .set_insert_provider(spinning(&spin, &steady, &Arc::default(), &away));
            let mut tone = resume_tone(0);
            tone.controls.effects[0] = Some(crate::insert::InsertControls::new(Default::default()));
            LiveReverbBatch::install_inserts(manual.device(), std::slice::from_ref(&tone));
            assert!(manual.device().push(tone));
            // A buffer of 2048 frames has 16 sub-blocks and a period of
            // 256 ms. A render gives the jobs a worker ran in it.
            let mut output = vec![0.0f32; 2048 * 2];
            let mut render = |manual: &mut ManualLiveOutput| {
                manual.render(&mut output);
                away.swap(0, Ordering::SeqCst)
            };
            // The buffer that takes the workers posts no job, also at 90%:
            // the callback has its class from the next buffer on. Its end
            // opens the gate for the next buffer.
            spin.store(230_400, Ordering::SeqCst);
            assert_eq!(render(&mut manual), 0);
            assert!(shared.gate_is_open());
            assert_eq!(render(&mut manual), 16, "each sub-block posts its job");
            assert!(!shared.gate_is_open());
            for _ in 0..8 {
                assert_eq!(render(&mut manual), 0, "a light set posted a job");
            }
            // A steady set spends 2 ms of each sub-block of 16 ms. It stays
            // near 13% of its time so far and posts no job. Against the time
            // of one sub-block, it passes 80% by its seventh sub-block.
            steady.store(2_000, Ordering::SeqCst);
            assert_eq!(render(&mut manual), 0, "a steady set at 13% posted a job");
            assert!(!shared.gate_is_open());
            steady.store(0, Ordering::SeqCst);
            // A start of 14 ms passes 80% of its first sub-block of 16 ms.
            // It is about 5% of the buffer. It pools from its second
            // sub-block, and the gate closes at the end of the buffer.
            spin.store(14_000, Ordering::SeqCst);
            assert_eq!(render(&mut manual), 15);
            assert!(!shared.gate_is_open());
            assert_eq!(render(&mut manual), 0);
            // A heavy start pools from its second sub-block, and the buffer
            // at 90% keeps the gate open for the next buffer.
            spin.store(230_400, Ordering::SeqCst);
            assert_eq!(render(&mut manual), 15);
            assert!(shared.gate_is_open());
            assert_eq!(render(&mut manual), 16);
            assert!(!shared.gate_is_open());
            assert_eq!(render(&mut manual), 0);
            // The stop ramp and the stopped buffers leave the gate as it was:
            // closed, also after a heavy ramp, or open.
            for open in [false, true] {
                spin.store(230_400, Ordering::SeqCst);
                if open {
                    assert_eq!(render(&mut manual), 15);
                }
                manual.device().stop();
                for _ in 0..4 {
                    render(&mut manual);
                }
                assert_eq!(spin.load(Ordering::SeqCst), 0, "no heavy block ran");
                assert!(manual.device().report().stop_acknowledged);
                assert_eq!(shared.gate_is_open(), open);
                manual.device().resume();
            }
            assert!(render(&mut manual) > 0, "the resume posted no job");
            assert!(!shared.gate_is_open());
            // The load meter gives the time of the jobs.
            let load = manual.device().report().realtime_load;
            assert!(load.worker_busy_nanos > 0 && load.join_wait_nanos > 0);

            // The average opens the gate past half load, with no buffer past
            // 80%, and closes it below 35%.
            let period = 2048 * 1_000_000_000 / 8_000;
            let mut end_buffer = |percent: u64| {
                manual.backend.end_pool_buffer(2048, period * percent / 100);
                shared.gate_is_open()
            };
            for _ in 0..64 {
                assert!(!end_buffer(0));
            }
            // At 70% the average is 1750, 3062, 4046, 4784, then 5338.
            for _ in 0..4 {
                assert!(!end_buffer(70));
            }
            assert!(end_buffer(70));
            for _ in 0..16 {
                assert!(end_buffer(40));
            }
            assert!(!end_buffer(0));
            for _ in 0..16 {
                assert!(!end_buffer(40));
            }
            // One buffer past 80% opens the gate, with an average of 2125.
            for _ in 0..64 {
                assert!(!end_buffer(0));
            }
            assert!(end_buffer(85));
            assert!(!end_buffer(0));
        }

        /// A stopped output posts no job, and its plugin workers park. A
        /// resumed output posts jobs again. The report counts the jobs the
        /// callback ran and the late ones.
        #[cfg(feature = "test-support")]
        #[test]
        fn a_stopped_output_parks_its_plugin_workers() {
            let _tripwire_guard = tripwire_assertion_guard();
            crate::set_plugin_pool_for_test(Some(2));
            let mut manual = ManualLiveOutput::new(48_000, 7).expect("manual output");
            let (spin, away) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
            let pause = Arc::new(AtomicU64::new(0));
            manual
                .device()
                .set_insert_provider(spinning(&spin, &Arc::default(), &pause, &away));
            let mut tone = resume_tone(0);
            tone.controls.effects[0] = Some(crate::insert::InsertControls::new(Default::default()));
            LiveReverbBatch::install_inserts(manual.device(), std::slice::from_ref(&tone));
            let shared = Arc::clone(
                manual
                    .device()
                    .plugin_pool
                    .lock()
                    .unwrap()
                    .as_ref()
                    .expect("plugin workers")
                    .shared(),
            );
            assert!(manual.device().push(tone));
            let mut output = [0.0f32; 256 * 2];
            // With no claim by a worker, the callback runs the job of each
            // sub-block.
            shared.hold_claims(true);
            manual.render(&mut output);
            assert_eq!(manual.device().report().plugin_jobs_inline, 2);
            shared.hold_claims(false);
            // From here only a worker runs the job of the chain.
            for bus in 0..crate::insert::MAX_INSERT_BUSES {
                shared.claim_before_join(crate::pool::JobKind::Effect, bus);
            }
            for _ in 0..4 {
                manual.render(&mut output);
            }
            assert!(
                away.load(Ordering::SeqCst) > 0,
                "the chain ran on no worker"
            );
            manual.device().stop();
            for _ in 0..3 {
                manual.render(&mut output);
            }
            assert!(manual.device().report().stop_acknowledged);
            let stopped = away.load(Ordering::SeqCst);
            for _ in 0..20 {
                manual.render(&mut output);
            }
            assert_eq!(
                away.load(Ordering::SeqCst),
                stopped,
                "a stopped block posted a job"
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while shared.parked_workers() < 2 {
                assert!(
                    Instant::now() < deadline,
                    "a worker of a stopped output did not park"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            manual.device().resume();
            manual.render(&mut output);
            assert!(
                away.load(Ordering::SeqCst) > stopped,
                "the resume posted no job"
            );
            // A job of 2 ms of work the callback waits for is not late: the
            // plugin needs that time.
            spin.store(2_000, Ordering::SeqCst);
            manual.render(&mut output);
            assert_eq!(manual.device().report().plugin_late_jobs, 0);
            // A job of 3 ms off the CPU the callback waits for is late.
            pause.store(3_000, Ordering::SeqCst);
            manual.render(&mut output);
            let report = manual.device().report();
            assert_eq!(report.plugin_late_jobs, 1);
            assert_eq!(report.plugin_workers, 2);
            assert_eq!(report.plugin_jobs_inline, 2);
            assert!(report.plugin_join_wait_nanos >= 2_000_000);
            crate::set_plugin_pool_for_test(None);
        }

        /// A stop fades an orbit routed to another output pair with the main
        /// pair, also the output of a plugin chain on the orbit. The stopped
        /// output then plays no routed audio.
        #[cfg(feature = "test-support")]
        #[test]
        fn a_stop_fades_a_routed_orbit_and_leaves_it_silent() {
            /// Passes its input on.
            struct Through(crate::insert::InsertKey);
            impl crate::insert::OrbitInsert for Through {
                fn key(&self) -> crate::insert::InsertKey {
                    self.0
                }
                fn set_param(&mut self, _: crate::insert::InsertParam, _: u32) {}
                fn process(&mut self, _: &mut [f32], _: &mut [f32]) {}
            }
            let _tripwire_guard = tripwire_assertion_guard();
            let key = crate::insert::InsertKey {
                plugin: 1,
                preset: 0,
            };
            for chain in [false, true] {
                let mut manual = ManualLiveOutput::new(48_000, 7).expect("manual output");
                // On a stereo output, pair 1 plays on the main pair.
                manual.device().set_orbit_output(1, 1);
                let mut tone = resume_tone(0);
                tone.freq_hz = 0.0;
                tone.synth = Some(crate::SynthSource::One);
                tone.controls.orbit = 1;
                tone.controls.envelope = crate::Envelope {
                    attack_secs: 0.001,
                    decay_secs: 0.0,
                    sustain: 1.0,
                    release_secs: 0.01,
                };
                if chain {
                    manual.device().set_insert_provider(Arc::new(|key, _, _| {
                        Some(Box::new(Through(key)) as Box<_>)
                    }));
                    tone.controls.effects[0] = Some(crate::insert::InsertControls::new(key));
                    LiveReverbBatch::install_inserts(manual.device(), std::slice::from_ref(&tone));
                }
                assert!(manual.device().push(tone));
                let mut output = [0.0f32; 256 * 2];
                manual.render(&mut output);
                manual.render(&mut output);
                let mut previous = output[510];
                assert!(
                    previous > 0.01,
                    "chain {chain}: the routed orbit sounds: {previous}"
                );

                manual.device().stop();
                for _ in 0..3 {
                    manual.render(&mut output);
                    for &left in output.iter().step_by(2) {
                        assert!(
                            (left - previous).abs() < 0.01,
                            "chain {chain}: a step in the stop ramp: {previous} to {left}"
                        );
                        previous = left;
                    }
                }
                assert!(manual.device().report().stop_acknowledged);
                for _ in 0..4 {
                    manual.render(&mut output);
                    assert!(
                        output.iter().all(|sample| *sample == 0.0),
                        "chain {chain}: the stopped output plays the routed orbit again"
                    );
                }
                assert_eq!(
                    manual
                        .device()
                        .holds_insert(crate::insert::effect_slot(1, 0), key),
                    chain
                );
                assert_eq!(manual.device().report().callback_scope_misses, 0);
            }
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn pending_stop_preserves_new_kabel_assets_and_discards_old_onsets() {
            let _tripwire_guard = tripwire_assertion_guard();
            let program = rustel_kabel::Program::from_json(
                r#"{"nodes":[{"type":"n","value":0.25},{"type":"output","inputs":[0]}]}"#,
            )
            .unwrap();
            let note = |frame| {
                let mut event = resume_tone(frame);
                event.controls.kabel = Some(crate::KabelControls {
                    program: program.into(),
                    start_secs: frame as f64 / 48_000.0,
                    gate_end_secs: frame as f64 / 48_000.0 + 1.0,
                    end_secs: frame as f64 / 48_000.0 + 1.0,
                });
                event
            };
            let mut manual = ManualLiveOutput::new(48_000, 7).unwrap();
            let mut output = [0.0; 128 * 2];
            let initial = note(0);
            let publication = manual.device().ensure_kabel(&initial).unwrap();
            assert!(manual.device().push(initial));
            drop(publication);
            manual.render(&mut output);
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));

            let old = [note(2048), note(128)];
            let publication = manual.device().prepare_kabel_events(&old).unwrap();
            assert!(manual.device().push(old[0].clone()));
            assert!(manual.device().push_immediate(old[1].clone()));
            drop(publication);
            manual.device().stop();
            manual.device().resume();

            let new = [note(640), note(640)];
            let publication = manual.device().prepare_kabel_events(&new).unwrap();
            assert!(manual.device().push(new[0].clone()));
            assert!(manual.device().push_immediate(new[1].clone()));
            drop(publication);
            let before = tripwire::Violations::capture();
            for _ in 0..4 {
                manual.render(&mut output);
                assert_eq!(manual.device().report().accepted_events, 1);
            }
            assert!(!manual.device().stop_pending());
            manual.render(&mut output);
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
            assert_eq!(manual.device().report().accepted_events, 3);
            assert_eq!(manual.device().report().refused_voices, 0);
            assert_eq!(manual.device().report().ring_depth, 0);
            let violations = tripwire::Violations::capture().since(before);
            assert!(violations.clean(), "Stop/resume allocated: {violations:?}");
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn resuming_a_pending_stop_finishes_the_fade_before_admitting_new_notes() {
            let _tripwire_guard = tripwire_assertion_guard();
            for before_resume in [0, 1] {
                for new_notes in [false, true] {
                    let mut manual = ManualLiveOutput::new(48_000, 7).unwrap();
                    let mut output = [0.0f32; 128 * 2];
                    assert!(!manual.device().stop_pending());
                    assert!(manual.device().push(resume_tone(0)));
                    manual.render(&mut output);
                    assert!(output.iter().any(|sample| sample.abs() > 1e-6));
                    assert!(manual.device().push(resume_tone(2048)));
                    assert!(manual.device().push_immediate(resume_tone(128)));
                    assert!(!manual.device().stop_and_wait(Duration::ZERO));
                    assert!(manual.device().stop_pending());
                    manual.device().stop();
                    for _ in 0..before_resume {
                        manual.render(&mut output);
                    }
                    manual.device().resume();
                    manual.device().resume();
                    assert!(manual.device().stop_pending());
                    if new_notes {
                        assert!(manual.device().push(resume_tone(640)));
                        assert!(manual.device().push_immediate(resume_tone(640)));
                    }
                    for _ in before_resume..4 {
                        manual.render(&mut output);
                        assert_eq!(manual.device().report().accepted_events, 1);
                    }
                    assert!(!manual.device().report().stop_acknowledged);
                    assert!(!manual.device().stop_pending());
                    assert!(!manual.device().shared.stop.is_stopped());
                    manual.render(&mut output);
                    assert_eq!(output.iter().any(|sample| sample.abs() > 1e-6), new_notes);
                    assert_eq!(
                        manual.device().report().accepted_events,
                        if new_notes { 3 } else { 1 }
                    );
                    assert_eq!(manual.device().report().ring_depth, 0);
                    if new_notes {
                        manual.device().stop();
                        manual.device().resume();
                        let frame = manual.device().clock_frames() + 1024;
                        assert!(manual.device().push(resume_tone(frame)));
                        assert!(manual.device().push_immediate(resume_tone(frame)));
                        manual.device().stop();
                        for _ in 0..4 {
                            manual.render(&mut output);
                        }
                        assert!(manual.device().report().stop_acknowledged);
                        assert!(!manual.device().stop_pending());
                        manual.device().resume();
                        for _ in 0..8 {
                            manual.render(&mut output);
                            assert!(output.iter().all(|sample| *sample == 0.0));
                        }
                        assert_eq!(manual.device().report().accepted_events, 3);
                    }
                }
            }
        }

        #[test]
        fn an_acknowledged_callback_cannot_acknowledge_a_later_stop() {
            let stop = LiveStop::new();
            stop.request();
            stop.complete(OUTPUT_STOP_REQUESTED);
            assert!(stop.acknowledged());
            let observed = stop.0.load(Ordering::Acquire);
            stop.resume();
            stop.request();
            stop.complete(observed);
            assert!(!stop.acknowledged());

            stop.resume();
            stop.request();
            stop.request();
            stop.complete(OUTPUT_RESUME_PENDING);
            assert!(
                stop.acknowledged(),
                "the second stop cancels pending resume"
            );
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn tripwire_rejects_prior_violation_without_erasing_it_and_fresh_output_arms() {
            let _tripwire_guard = tripwire_assertion_guard();
            let mut manual = ManualLiveOutput::new(48_000, 1).expect("manual output");
            let device = &mut manual.device;
            device.arm_callback_tripwire_checked().expect("first arm");
            device
                .arm_callback_tripwire_checked()
                .expect("repeat clean arm");
            assert_eq!(device.report().callback_allocations, 0);
            assert_eq!(device.report().callback_frees, 0);

            tripwire::audio_scope(|| {
                let allocation = vec![1_u8; 8];
                std::hint::black_box(allocation);
            });
            let report = device.report();
            assert_eq!(report.callback_allocations, 1);
            assert_eq!(report.callback_frees, 1);
            assert_eq!(
                device.arm_callback_tripwire_checked(),
                Err(CallbackTripwireArmError::PriorViolation {
                    allocations: 1,
                    frees: 1,
                    scope_misses: 0,
                })
            );
            assert_eq!(device.report().callback_allocations, 1);
            assert_eq!(device.report().callback_frees, 1);

            drop(manual);
            let mut fresh = ManualLiveOutput::new(48_000, 2).expect("fresh output");
            fresh
                .device
                .arm_callback_tripwire_checked()
                .expect("fresh reporting window");
            assert_eq!(fresh.device().report().callback_allocations, 0);
            assert_eq!(fresh.device().report().callback_frees, 0);
        }

        #[test]
        fn pending_callbacks_write_silence_in_every_host_format() {
            fn check<T>()
            where
                T: SizedSample + FromSample<f32> + PartialEq + std::fmt::Debug,
            {
                assert!(
                    sample_format_from_cpal(T::FORMAT).is_some(),
                    "{}",
                    T::FORMAT
                );
                let gate = CallbackGate::new();
                let mut output = [T::from_sample(0.5); 7];
                gate.render_or_silence(&mut output, |_| panic!("pending callback entered DSP"));
                assert_eq!(output, [T::from_sample(0.0); 7]);
            }
            check::<f32>();
            check::<f64>();
            check::<i8>();
            check::<i16>();
            check::<I24>();
            check::<i32>();
            check::<i64>();
            check::<u8>();
            check::<u16>();
            check::<U24>();
            check::<u32>();
            check::<u64>();
        }

        #[test]
        fn pending_callbacks_leave_queues_and_shared_state_for_activation() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            let dispatch = DspDispatch::portable();
            let mut device = LiveScalarDevice::start_silent_with_options(
                48_000,
                7,
                LiveOutputOptions::default().with_dispatch(dispatch),
            )
            .expect("silent output");
            device.silent.take().expect("callback").stop();
            // SAFETY: the prior callback is joined before this thread becomes the
            // consumer, so asset inspection below never races a silent callback.
            unsafe { device.shared.ring.release_consumer() };
            device.shared.input.set_channels(1);
            device.shared.input.write(&[0.25; 4096], 1);
            device.set_master_gain(0.5);
            device.set_analysis_enabled(true);
            device
                .install_sample(
                    crate::sample::SampleId(0),
                    crate::sample::DecodedSample::from_parts(48_000, 1, vec![0.1; 256])
                        .expect("sample"),
                )
                .expect("sample install");
            let params = crate::reverb::ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            device.ensure_reverb(0, params);
            device.ensure_fx_reverb(params);
            assert!(device.push(AudioEvent {
                onset_id: 1,
                generation: 7,
                target_frame: device.clock_frames(),
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: Some(crate::SynthSource::Input { channel: 0 }),
                wavetable: None,
                cut: None,
            }));
            let shared = &device.shared;
            let mut backend = PreparedLiveOutput::backend(
                shared,
                48_000,
                LiveOutputOptions::default().with_dispatch(dispatch),
                &[],
            )
            .expect("backend");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let gate = CallbackGate::new();
            let before = shared.report(1);
            assert_eq!(before.ring_depth, 1);
            assert_eq!(before.asset_queues.sample_installs, 1);
            assert_eq!(before.asset_queues.orbit_reverb_installs, 1);
            assert_eq!(before.asset_queues.fx_reverb_installs, 1);
            let origin = shared.playback_origin_nanos.load(Ordering::Acquire);
            let violations = tripwire::Violations::capture();
            let mut output = [0.5f32; 256];
            // A host may call more than once during build and again during play.
            for _ in 0..3 {
                gate.render_or_silence(&mut output, |output| {
                    shared.playback_origin_nanos.store(123, Ordering::Release);
                    write_live_output(&mut backend, &mut meters, output, 2, shared);
                });
                assert_eq!(output, [0.0; 256]);
                assert_eq!(shared.report(1), before);
                assert_eq!(shared.playback_origin_nanos.load(Ordering::Acquire), origin);
            }
            gate.activate().expect("activate once");
            gate.render_or_silence(&mut output, |output| {
                write_live_output(&mut backend, &mut meters, output, 2, shared);
            });
            assert!(tripwire::Violations::capture().since(violations).clean());
            assert!(output.iter().all(|sample| sample.is_finite()));
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
            let after = shared.report(1);
            assert_eq!(after.submitted_frames, before.submitted_frames + 128);
            assert_eq!(after.accepted_events, before.accepted_events + 1);
            assert_eq!(after.ring_depth, 0);
            assert_eq!(after.ring_role_conflicts, 0);
            assert_eq!(after.asset_queues.sample_installs, 0);
            assert_eq!(after.asset_queues.orbit_reverb_installs, 0);
            assert_eq!(after.asset_queues.fx_reverb_installs, 0);
            assert!(gate.activate().is_err(), "activation cannot be repeated");
        }

        #[test]
        fn callback_errors_before_activation_do_not_fail_the_running_output() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            let shared = LiveShared::new(7);
            let pending = CallbackGate::new();
            let before = shared.report(1);
            let violations = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                pending.report_error(&shared.failed, &shared.callback_errors);
                pending.report_error(&shared.failed, &shared.callback_errors);
            });
            assert!(tripwire::Violations::capture().since(violations).clean());
            assert!(!shared.failed.load(Ordering::Acquire));
            assert_eq!(shared.report(1), before);
            assert!(pending.activate().is_err());
            let mut output = [1.0f32; 8];
            pending.render_or_silence(&mut output, |_| panic!("failed candidate rendered"));
            assert_eq!(output, [0.0; 8]);
            let mut started = None;
            assert_eq!(
                silent_elapsed(&mut started, &pending, || panic!(
                    "failed gate read the clock"
                )),
                None
            );
            assert!(started.is_none());

            let active = CallbackGate::new();
            active.activate().expect("active output");
            active.report_error(&shared.failed, &shared.callback_errors);
            active.report_error(&shared.failed, &shared.callback_errors);
            assert!(
                active.is_active(),
                "active errors keep the normal stop path"
            );
            assert!(shared.failed.load(Ordering::Acquire));
            assert_eq!(shared.callback_errors.load(Ordering::Acquire), 2);
        }

        #[test]
        fn silent_pacing_starts_at_activation_not_preparation() {
            let gate = CallbackGate::new();
            let prepared_at = Instant::now();
            let mut started = None;
            assert_eq!(
                silent_elapsed(&mut started, &gate, || panic!(
                    "pending gate read the clock"
                )),
                None
            );
            let activated_at = prepared_at + Duration::from_secs(30);
            assert_eq!(
                silent_elapsed(&mut started, &gate, || panic!(
                    "pending gate read the clock"
                )),
                None
            );
            assert!(started.is_none());
            gate.activate().expect("activate");
            assert_eq!(
                silent_elapsed(&mut started, &gate, || activated_at),
                Some(Duration::ZERO)
            );
            assert_eq!(started, Some(activated_at));
            assert_eq!(
                silent_elapsed(&mut started, &gate, || {
                    activated_at + Duration::from_millis(10)
                }),
                Some(Duration::from_millis(10)),
            );
        }

        #[test]
        fn prepared_silent_drop_joins_without_stopping_shared_playback() {
            let shared = LiveShared::new(7);
            shared.frames.store(48_000, Ordering::Release);
            shared.input.set_channels(1);
            for fail in [false, true] {
                let before = shared.report(1);
                let input_owners = Arc::strong_count(&shared.input);
                let prepared = PreparedLiveOutput::silent(
                    shared.clone(),
                    48_000,
                    LiveOutputOptions::default().with_dispatch(DspDispatch::portable()),
                    &[],
                )
                .expect("prepared output");
                let gate = Arc::clone(&prepared.gate);
                prepared.start_gated().expect("gated start");
                if fail {
                    gate.report_error(&shared.failed, &shared.callback_errors);
                    assert!(prepared.activate().is_err());
                } else {
                    drop(prepared);
                }
                // The callback retains a gate reference until its thread exits.
                assert_eq!(Arc::strong_count(&gate), 1, "candidate thread was joined");
                assert_eq!(Arc::strong_count(&shared.input), input_owners);
                assert_eq!(shared.report(1), before);
                assert!(!shared.stop.is_stopped());
                assert_eq!(shared.input.channels(), 1);
            }
        }

        #[test]
        fn prepared_silent_activation_transfers_the_thread_to_its_handle() {
            let shared = LiveShared::new(7);
            let prepared = PreparedLiveOutput::silent(
                shared.clone(),
                48_000,
                LiveOutputOptions::default(),
                &[],
            )
            .expect("prepared output");
            let gate = Arc::clone(&prepared.gate);
            prepared.start_gated().expect("gated start");
            assert_eq!(shared.clock_frames(), 0);
            let handle = prepared.activate().expect("activate");
            let deadline = Instant::now() + Duration::from_secs(3);
            while shared.callbacks.load(Ordering::Acquire) == 0 {
                assert!(Instant::now() < deadline, "activated callback never ran");
                std::thread::sleep(Duration::from_millis(2));
            }
            drop(handle);
            assert_eq!(
                Arc::strong_count(&gate),
                1,
                "active handle joined its thread"
            );
            assert!(shared.clock_frames() >= u64::from(SILENT_BUFFER_FRAMES));
            assert_eq!(shared.ring.role_conflicts.load(Ordering::Acquire), 0);
            assert!(!shared.stop.is_stopped());
        }

        /// A held silent clock stays put however long the test looks at it,
        /// and a released one resumes where it stopped: no burst of blocks for
        /// the time spent held.
        ///
        /// The burst is bounded without racing the silent thread. Its clock
        /// only ever counts wall time since its gate opened, less the time it
        /// spent held, and delivers nothing it has not counted. The gate opens
        /// after `opened` (taken once the output is prepared, so preparation
        /// does not blunt the check), and the thread's held span covers the one
        /// measured here from the hold's acknowledgement to just before the
        /// release. So however late this thread runs, the frozen clock is never
        /// past the unheld wall time from `opened` to now, where resuming from
        /// the old origin puts it a whole hold ahead.
        #[cfg(feature = "test-support")]
        #[test]
        fn a_held_silent_clock_freezes_and_resumes_without_a_catch_up_burst() {
            const RATE: u32 = 48_000;
            const HOLD: Duration = Duration::from_millis(100);
            let shared = LiveShared::new(7);
            let options = LiveOutputOptions::default();
            let prepared = PreparedLiveOutput::silent(shared.clone(), RATE, options, &[])
                .expect("prepared silent output");
            let opened = Instant::now();
            let device =
                LiveScalarDevice::start_prepared(shared, options, prepared).expect("silent output");
            let deadline = Instant::now() + Duration::from_secs(5);
            while device.shared.clock_frames() == 0 {
                assert!(Instant::now() < deadline, "the silent output never started");
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(device.hold_silent_clock_for_test(true));
            let held_from = Instant::now();
            let held_at = device.shared.clock_frames();
            std::thread::sleep(HOLD);
            assert_eq!(
                device.shared.clock_frames(),
                held_at,
                "a held clock does not move"
            );
            let held = held_from.elapsed();

            assert!(device.hold_silent_clock_for_test(false));
            let deadline = Instant::now() + Duration::from_secs(5);
            while device.shared.clock_frames() == held_at {
                assert!(Instant::now() < deadline, "a released clock never resumed");
                std::thread::sleep(Duration::from_micros(200));
            }
            assert!(device.hold_silent_clock_for_test(true));
            let frozen = device.shared.clock_frames();
            let unheld = opened.elapsed().saturating_sub(held);
            let pace = (unheld.as_secs_f64() * f64::from(RATE)) as u64;
            // One frame of slack for the float products on either side.
            assert!(
                frozen <= pace + 1,
                "the released clock resumed with a catch-up burst: {frozen} frames against \
             {pace} frames of unheld wall time ({held:?} held)"
            );
            assert!(device.hold_silent_clock_for_test(false));
        }

        #[test]
        fn silent_output_reports_only_facts_it_actually_has() {
            let device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            assert!(!device.dispatch().is_forced_portable());
            let facts = device.audio_facts();
            let output = facts.output();

            assert_eq!(output.host(), &AudioHost::Silent);
            assert_eq!(output.host().id(), None);
            assert_eq!(output.device_id(), SILENT_OUTPUT_NAME);
            assert_eq!(output.sample_rate_hz(), 48_000);
            assert_eq!(output.channels(), 2);
            assert_eq!(output.sample_format(), None);
            assert_eq!(output.buffer_preference(), AudioBufferPreference::Auto);
            assert_eq!(output.requested_buffer_frames(), SILENT_BUFFER_FRAMES);
            assert_eq!(output.reported_buffer_frames(), Some(SILENT_BUFFER_FRAMES));
            assert!(!output.host_timestamps_available());
            assert_eq!(facts.input(), None);
        }
    }
    mod output_recycle {
        use super::*;

        #[test]
        fn silent_recycle_reinstalls_unchanged_reverb_parameters() {
            fn take_prepared_reverbs(
                device: &LiveScalarDevice,
            ) -> (
                Box<crate::reverb::OrbitReverb>,
                Box<crate::reverb::OrbitReverb>,
            ) {
                let install = device
                    .shared
                    .samples
                    .reverb_installs
                    .pop()
                    .expect("prepared orbit reverb");
                assert_eq!(install.orbit, 0);
                // SAFETY: the sole callback has been joined before this helper
                // runs. Popping transfers the allocation to this thread.
                let orbit = unsafe { Box::from_raw(install.reverb) };
                let install = device
                    .shared
                    .samples
                    .fx_reverb_installs
                    .pop()
                    .expect("prepared stage reverb");
                // SAFETY: as above, this is the popped allocation's only owner.
                let stage = unsafe { Box::from_raw(install.reverb) };
                (orbit, stage)
            }

            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.silent.take().expect("silent callback").stop();
            let params = crate::reverb::ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            device.ensure_reverb(0, params);
            device.ensure_fx_reverb(params);
            let (orbit, stage) = take_prepared_reverbs(&device);
            assert_eq!(orbit.params(), params);
            assert_eq!(stage.params(), params);
            drop((orbit, stage));

            // Empty queues do not invalidate the producer fingerprints. Repeated
            // requests must remain cache hits until the backend is replaced.
            device.ensure_reverb(0, params);
            device.ensure_fx_reverb(params);
            assert_eq!(device.shared.samples.reverb_installs.len(), 0);
            assert_eq!(device.shared.samples.fx_reverb_installs.len(), 0);

            device.recycle_output().expect("recycle silent output");
            device.silent.take().expect("replacement callback").stop();
            device.ensure_reverb(0, params);
            device.ensure_fx_reverb(params);
            let (orbit, stage) = take_prepared_reverbs(&device);
            assert_eq!(orbit.params(), params);
            assert_eq!(stage.params(), params);
            assert_eq!(device.shared.samples.reverb_installs.len(), 0);
            assert_eq!(device.shared.samples.fx_reverb_installs.len(), 0);
        }

        /// The callback count is what the studio settles a retiring id
        /// against: it climbs one per completed block, and a recycle keeps the
        /// shared counter, so a count read before it is still comparable after.
        #[test]
        fn callbacks_count_completed_blocks_and_survive_a_recycle() {
            fn wait_past(device: &LiveScalarDevice, count: u64) -> u64 {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let now = device.callbacks();
                    if now > count {
                        return now;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the silent output stopped at {now} callbacks"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }

            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            let first = wait_past(&device, 0);
            let second = wait_past(&device, first);
            assert!(second > first, "one more block, one more callback");

            device.recycle_output().expect("recycle silent output");
            let after = wait_past(&device, second);
            assert!(
                after > second,
                "the count carried over the recycle: {second} then {after}"
            );
        }

        #[test]
        fn silent_recycle_keeps_input_audio_routed() {
            fn wait_for_input(device: &LiveScalarDevice, sign: f32) -> LiveAnalysisSnapshot {
                let start = device.clock_frames();
                assert!(device.push(AudioEvent {
                    onset_id: start,
                    generation: device.generation(),
                    target_frame: start,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.5,
                    duration_secs: 10.0,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: Some(crate::SynthSource::Input { channel: 0 }),
                    wavetable: None,
                    cut: None,
                }));
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut output = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
                loop {
                    if let Some(snapshot) = device.copy_analysis_window(&mut output) {
                        assert!(output.iter().all(|sample| sample.is_finite()));
                        if snapshot.end_frame > start
                            && output.iter().any(|sample| sample * sign > 1e-6)
                        {
                            return snapshot;
                        }
                    }
                    assert!(
                        Instant::now() < deadline,
                        "input stayed silent: rendered {} frames after enqueue",
                        device.clock_frames().saturating_sub(start),
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
            }

            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.shared.input.set_channels(1);
            device.shared.input.write(&[0.25; 4096], 1);
            device.set_analysis_enabled(true);
            let before = wait_for_input(&device, 1.0);

            device.recycle_output().expect("recycle silent output");
            // New input with the opposite sign cannot be mistaken for PCM from
            // the discarded backend, even if an old analysis window survived.
            device.shared.input.write(&[-0.25; 4096], 1);
            let after = wait_for_input(&device, -1.0);
            assert_ne!(after.epoch, before.epoch);
            assert_eq!(device.input_channels(), 1);
        }

        #[test]
        fn silent_selector_retains_prepared_output_options() {
            for dispatch in [DspDispatch::automatic(), DspDispatch::portable()] {
                let preference = AudioBufferPreference::Frames(512);
                let options = LiveOutputOptions::default()
                    .with_dispatch(dispatch)
                    .with_buffer_preference(preference);
                let device = LiveScalarDevice::start_output_with_options(
                    Some(SILENT_OUTPUT_NAME),
                    7,
                    options,
                )
                .expect("selected silent output");
                assert_eq!(device.audio_facts().output().host(), &AudioHost::Silent);
                assert_eq!(device.output_options().buffer_preference(), preference);
                assert_eq!(
                    device.audio_facts().output().buffer_preference(),
                    preference
                );
                assert_eq!(device.requested_buffer_frames(), 512);
                let selected = device.output_options().dispatch();
                assert_eq!(selected.is_forced_portable(), dispatch.is_forced_portable());
                assert_eq!(
                    selected.convolution_kernel_kind(),
                    dispatch.convolution_kernel_kind()
                );
                assert_eq!(
                    selected.supersaw_kernel_kind(),
                    dispatch.supersaw_kernel_kind()
                );
                assert_eq!(
                    selected.wavetable_kernel_kind(),
                    dispatch.wavetable_kernel_kind()
                );
            }
        }

        #[test]
        fn explicit_silent_buffer_survives_recycle() {
            let preference = AudioBufferPreference::Frames(513);
            let options = LiveOutputOptions::default()
                .with_buffer_preference(preference)
                .with_dispatch(DspDispatch::portable());
            let mut device = LiveScalarDevice::start_silent_with_options(44_100, 7, options)
                .expect("silent output with explicit buffer");
            let mut previous_publication = 0;
            for (recycled, sample_rate) in [(false, 44_100), (true, SILENT_SAMPLE_RATE)] {
                if recycled {
                    device.recycle_output().expect("recycle silent output");
                }
                assert_eq!(device.buffer_preference(), preference);
                assert_eq!(device.output_options().buffer_preference(), preference);
                assert!(device.dispatch().is_forced_portable());
                assert_eq!(device.sample_rate(), sample_rate);
                assert_eq!(device.requested_buffer_frames(), 513);
                assert_eq!(device.reported_buffer_frames(), Some(513));
                let facts = device.audio_facts();
                assert_eq!(facts.output().buffer_preference(), preference);
                assert_eq!(facts.output().requested_buffer_frames(), 513);
                assert_eq!(facts.output().reported_buffer_frames(), Some(513));

                // Load telemetry publishes every 16 callbacks. Recycle publishes
                // an empty reset first; the changed rate also excludes an old
                // callback's late publication from proving replacement delivery.
                let deadline = Instant::now() + Duration::from_secs(3);
                let load = loop {
                    let load = device.report().realtime_load;
                    if load.publication > previous_publication
                        && load.sample_rate_hz == sample_rate
                        && load.total_callbacks >= REALTIME_LOAD_PUBLISH_INTERVAL
                    {
                        break load;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "no fresh callback telemetry after recycled={recycled}: {load:?}"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                };
                assert_eq!(load.last_callback_frames, 513);
                let period_nanos = 513 * 1_000_000_000 / u64::from(sample_rate);
                assert_eq!(load.last_callback_period_nanos, period_nanos);
                assert_eq!(load.total_period_nanos, load.total_callbacks * period_nanos);
                previous_publication = load.publication;
            }
        }

        #[test]
        fn rejected_output_discovery_preserves_queued_state() {
            let _tripwire_guard = tripwire_assertion_guard();
            let mut device = LiveScalarDevice::start_silent(44_100, 7).expect("silent output");
            // Quiesce the callback without dropping its owner so queue and telemetry
            // comparisons cannot race normal playback progress.
            device.silent.as_mut().expect("silent callback").stop();
            // SAFETY: `stop` joined the silent callback, the ring's only consumer.
            unsafe { device.shared.ring.release_consumer() };
            let close = Arc::clone(&device.silent.as_ref().unwrap().close);
            device.shared.frames.store(44_100, Ordering::Release);
            device.set_generation(7, 88_200, TakeoverCut::None);
            device.set_master_gain(0.5);
            device
                .shared
                .record
                .set_enabled(true)
                .expect("legacy recording");
            let event = AudioEvent {
                onset_id: 42,
                generation: 7,
                target_frame: 88_200,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            assert!(device.push(event));
            let report = device.report();
            let facts = device.audio_facts();
            let mut discoveries = 0;
            let error = device
                .recycle_output_to_with(
                    Some("unavailable-output"),
                    |selector| {
                        discoveries += 1;
                        assert_eq!(selector, Some("unavailable-output"));
                        Err(DevicePlaybackError::Unavailable(
                            "discovery rejected".into(),
                        ))
                    },
                    LiveScalarDevice::prepare_recycled_output,
                )
                .expect_err("discovery must fail");
            assert_eq!(error.to_string(), "discovery rejected");
            assert_eq!(discoveries, 1);
            assert!(!device.shared.stop.is_stopped());
            assert!(Arc::ptr_eq(
                &device.silent.as_ref().expect("retained output").close,
                &close
            ));
            assert_eq!(device.audio_facts(), facts);
            assert_eq!(device.report(), report);
            assert_eq!(device.shared.takeover_frame.load(Ordering::Acquire), 88_200);
            assert!(device.shared.record.is_enabled());
            assert_eq!(
                device.shared.ring.pop().map(|event| event.onset_id),
                Some(42)
            );
            assert!(device.shared.ring.is_empty());
            device.check_health().expect("discovery preserves health");
        }

        #[test]
        fn rejected_output_discovery_keeps_the_live_clock_running() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            let callback = device
                .silent
                .as_ref()
                .unwrap()
                .join
                .as_ref()
                .unwrap()
                .thread()
                .id();
            device
                .recycle_output_to_with(
                    Some("unavailable-output"),
                    |_| {
                        Err(DevicePlaybackError::Unavailable(
                            "discovery rejected".into(),
                        ))
                    },
                    LiveScalarDevice::prepare_recycled_output,
                )
                .expect_err("discovery must fail");
            assert!(!device.shared.stop.is_stopped());
            assert_eq!(
                device
                    .silent
                    .as_ref()
                    .expect("retained output")
                    .join
                    .as_ref()
                    .unwrap()
                    .thread()
                    .id(),
                callback
            );
            let before = device.clock_frames();
            let deadline = Instant::now() + Duration::from_secs(3);
            while device.clock_frames() <= before {
                assert!(
                    Instant::now() < deadline,
                    "retained output stopped advancing"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(device.generation(), 7);
            device.check_health().expect("discovery preserves health");
        }

        fn assert_recycled_sample_is_copied(
            device: &LiveScalarDevice,
            id: crate::SampleId,
            sample: &crate::DecodedSample,
        ) {
            device
                .set_recording_enabled(true)
                .expect("record copied PCM");
            let target = device.clock_frames() + u64::from(device.sample_rate()) / 4;
            let generation = device.generation() + 1;
            let channel = &device.shared.confirmations;
            let offer = confirmation_offer(channel, 1, generation, target, target + 128, 1);
            let mut event = confirmation_event(offer, 0, target);
            event.event.gain = 1.0;
            event.event.sample = Some(crate::SampleControls {
                sample: id,
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
            event.expected_sample_identity = Some(sample.identity());
            assert!(channel.publish(offer));
            device.set_generation(generation, target, TakeoverCut::None);
            assert!(device.push(event));

            let mut pcm = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(3);
            let terminal = loop {
                device.drain_recording(&mut pcm);
                if let Some(terminal) = channel.pop_terminal() {
                    break terminal;
                }
                assert!(
                    Instant::now() < deadline,
                    "seeded callback did not copy its window"
                );
                std::thread::sleep(Duration::from_millis(2));
            };
            device.drain_recording(&mut pcm);
            assert_eq!(terminal.key, offer.key);
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::Confirmed
            );
            assert!(pcm.iter().all(|value| value.is_finite()));
            assert!(
                pcm.iter().any(|value| value.abs() > 0.125),
                "new body must sound"
            );
            let queues = device.report().asset_queues;
            assert_eq!(queues.sample_installs, 0);
            assert_eq!(queues.sample_returns, 0);
            assert_eq!(queues.leaked, 0);
        }

        #[test]
        fn seeded_recycle_supersedes_pending_installs_and_uninstalls() {
            for pending_uninstall in [false, true] {
                let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
                // Keep the old handle but join its callback so both batons are
                // deterministically still pending when the real recycle begins.
                device.silent.as_mut().expect("old callback").stop();
                let old_close = Arc::clone(&device.silent.as_ref().unwrap().close);
                let id = crate::SampleId(9);
                let old = crate::DecodedSample::from_parts(48_000, 1, vec![0.125; 4096])
                    .expect("old sample");
                let samples = [(
                    id,
                    crate::DecodedSample::from_parts(48_000, 1, vec![0.75; 4096])
                        .expect("new sample"),
                )];
                device.install_sample(id, old).expect("queued old body");
                if pending_uninstall {
                    assert!(device.uninstall_sample(id));
                }
                assert_eq!(
                    device.report().asset_queues.sample_installs,
                    if pending_uninstall { 2 } else { 1 }
                );

                device
                    .recycle_output_to_with_samples(Some(SILENT_OUTPUT_NAME), &samples, || false)
                    .expect("seeded recycle");
                assert_eq!(Arc::strong_count(&old_close), 1, "old owner released");
                device.check_health().expect("replacement healthy");
                assert_recycled_sample_is_copied(&device, id, &samples[0].1);
            }
        }

        #[test]
        fn invalid_seeded_recycle_preserves_the_old_output_and_queued_batons() {
            let sample =
                crate::DecodedSample::from_parts(48_000, 1, vec![0.75; 32]).expect("sample");
            for invalid in [
                vec![(
                    crate::SampleId(crate::sample::SAMPLE_BANK_CAPACITY as u32),
                    sample.clone(),
                )],
                vec![(crate::SampleId(9), sample.clone()); crate::sample::SAMPLE_BANK_CAPACITY + 1],
            ] {
                let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
                device.silent.as_mut().expect("old callback").stop();
                let close = Arc::clone(&device.silent.as_ref().unwrap().close);
                device
                    .install_sample(crate::SampleId(9), sample.clone())
                    .expect("install baton");
                assert!(device.uninstall_sample(crate::SampleId(10)));
                let report = device.report();
                let facts = device.audio_facts();
                let epoch = device.confirmations().epoch();

                let error = device
                    .recycle_output_to_with_samples(Some(SILENT_OUTPUT_NAME), &invalid, || {
                        panic!("invalid seed reached recycle")
                    })
                    .expect_err("invalid snapshot must fail before teardown");
                assert!(matches!(error, DevicePlaybackError::ResourceLimit(_)));
                assert!(Arc::ptr_eq(
                    &close,
                    &device.silent.as_ref().expect("old output").close
                ));
                assert!(!device.shared.stop.is_stopped());
                assert_eq!(device.audio_facts(), facts);
                assert_eq!(device.report(), report);
                assert_eq!(device.confirmations().epoch(), epoch);
                device.check_health().expect("old output unchanged");
                let install = device
                    .shared
                    .samples
                    .installs
                    .pop()
                    .expect("preserved install");
                assert_eq!(install.id, crate::SampleId(9));
                // SAFETY: the callback is joined; this pop transfers unique ownership.
                let queued = unsafe { Box::from_raw(install.sample) };
                assert_eq!(queued.identity(), sample.identity());
                let uninstall = device
                    .shared
                    .samples
                    .installs
                    .pop()
                    .expect("preserved uninstall");
                assert_eq!(uninstall.id, crate::SampleId(10));
                assert!(uninstall.sample.is_null());
                assert_eq!(device.shared.samples.installs.len(), 0);
            }
        }

        #[test]
        fn recycle_preparation_failure_marks_output_unhealthy_and_can_recover() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.check_health().expect("initial output is healthy");
            let old_close = Arc::clone(&device.silent.as_ref().unwrap().close);
            let samples = [(
                crate::SampleId(9),
                crate::DecodedSample::from_parts(48_000, 1, vec![0.75; 4096]).expect("seed"),
            )];
            let identity = samples[0].1.identity();
            let pcm = samples[0].1.pcm().as_ptr();
            let mut refused_health = [false; 2];
            let mut preparations = 0;
            for refused in &mut refused_health {
                let error = device
                    .recycle_output_to_with(
                        None,
                        |_| panic!("silent output needs no discovery"),
                        |device, opened| {
                            preparations += 1;
                            assert!(opened.is_none());
                            assert!(device.stream.is_none() && device.silent.is_none());
                            assert!(device.shared.stop.is_stopped());
                            assert!(old_close.load(Ordering::Acquire));
                            assert_eq!(Arc::strong_count(&old_close), 1, "old thread was joined");
                            let prepared =
                                device.prepare_recycled_output_with_samples(opened, &samples)?;
                            let OutputHandle::Silent(silent) = &prepared.handle else {
                                panic!("expected seeded candidate");
                            };
                            let close = Arc::clone(&silent.close);
                            drop(prepared);
                            assert!(close.load(Ordering::Acquire));
                            assert_eq!(Arc::strong_count(&close), 1, "failed candidate joined");
                            Err(DevicePlaybackError::Unavailable(
                                "preparation rejected".into(),
                            ))
                        },
                    )
                    .expect_err("preparation must fail");
                assert_eq!(error.to_string(), "preparation rejected");
                assert!(device.stream.is_none() && device.silent.is_none());
                assert!(device.shared.stop.is_stopped());
                assert_eq!(samples[0].1.identity(), identity);
                assert_eq!(samples[0].1.pcm().as_ptr(), pcm);
                assert_eq!(samples[0].1.stereo_at(0.0), Some((0.75, 0.75)));
                *refused = device.check_health().is_err();
            }
            assert_eq!(preparations, 2);

            // The old thread is joined before this sole replacement is prepared.
            device
                .recycle_output_to_with_samples(None, &samples, || false)
                .expect("successful seeded silent retry");
            assert!(device.stream.is_none() && device.silent.is_some());
            assert!(!device.shared.stop.is_stopped());
            device
                .check_health()
                .expect("successful retry clears failure");
            let before = device.clock_frames();
            let deadline = Instant::now() + Duration::from_secs(3);
            while device.clock_frames() <= before {
                assert!(
                    Instant::now() < deadline,
                    "replacement output stopped advancing"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(device.generation(), 7);
            device.check_health().expect("replacement remains healthy");
            assert_recycled_sample_is_copied(&device, samples[0].0, &samples[0].1);
            assert_eq!(
                refused_health, [true; 2],
                "handleless retries must fail health"
            );
        }

        #[test]
        fn recycle_activation_failure_marks_output_unhealthy() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.check_health().expect("initial output is healthy");
            let old_close = Arc::clone(&device.silent.as_ref().unwrap().close);
            let mut candidate_close = None;
            let mut preparations = 0;
            let error = device
                .recycle_output_to_with(
                    None,
                    |_| panic!("silent output needs no discovery"),
                    |device, opened| {
                        preparations += 1;
                        assert!(opened.is_none());
                        assert!(device.stream.is_none() && device.silent.is_none());
                        assert!(old_close.load(Ordering::Acquire));
                        assert_eq!(Arc::strong_count(&old_close), 1, "old thread was joined");
                        let prepared = device.prepare_recycled_output(opened)?;
                        let OutputHandle::Silent(silent) = &prepared.handle else {
                            panic!("expected pending silent output");
                        };
                        candidate_close = Some(Arc::clone(&silent.close));
                        assert_eq!(
                            prepared.gate.state.load(Ordering::Acquire),
                            CALLBACK_PENDING
                        );
                        prepared
                            .gate
                            .report_error(&device.shared.failed, &device.shared.callback_errors);
                        assert_eq!(prepared.gate.state.load(Ordering::Acquire), CALLBACK_FAILED);
                        Ok(prepared)
                    },
                )
                .expect_err("failed pending gate must reject activation");
            assert_eq!(preparations, 1);
            assert_eq!(
                error.to_string(),
                "live output failed before activation or was already activated"
            );
            assert!(device.stream.is_none() && device.silent.is_none());
            assert!(device.shared.stop.is_stopped());
            let candidate_close = candidate_close.expect("candidate was prepared");
            assert!(candidate_close.load(Ordering::Acquire));
            assert_eq!(
                Arc::strong_count(&candidate_close),
                1,
                "candidate thread was joined"
            );
            assert_eq!(device.shared.callback_errors.load(Ordering::Acquire), 0);
            assert!(
                device.check_health().is_err(),
                "handleless activation must fail health"
            );
        }

        #[test]
        fn silent_recycle_does_not_discover_a_native_output() {
            let mut device = LiveScalarDevice::start_silent(44_100, 7).expect("silent output");
            for selector in [None, Some(SILENT_OUTPUT_NAME)] {
                device
                    .recycle_output_to_with(
                        selector,
                        |_| panic!("silent output needs no discovery"),
                        LiveScalarDevice::prepare_recycled_output,
                    )
                    .expect("silent recycle");
                assert_eq!(device.name(), SILENT_OUTPUT_NAME);
                assert_eq!(device.sample_rate(), SILENT_SAMPLE_RATE);
                assert_eq!(device.generation(), 7);
            }
        }

        #[test]
        fn cancelled_recycle_never_activates_the_pending_output() {
            use std::cell::Cell;
            let samples = [(
                crate::SampleId(9),
                crate::DecodedSample::from_parts(48_000, 1, vec![0.75; 4096]).expect("seed"),
            )];
            let identity = samples[0].1.identity();
            let pcm = samples[0].1.pcm().as_ptr();
            for cancel_at in 1..=5 {
                let mut device = LiveScalarDevice::start_silent(44_100, 7).expect("silent output");
                let old_close = Arc::clone(&device.silent.as_ref().unwrap().close);
                let facts = device.audio_facts();
                let checks = Cell::new(0);
                let mut candidate_close = None;
                let error = device
                    .recycle_output_to_with_cancel(
                        Some(SILENT_OUTPUT_NAME),
                        |_| panic!("silent output needs no discovery"),
                        |device, opened| {
                            let prepared =
                                device.prepare_recycled_output_with_samples(opened, &samples)?;
                            let OutputHandle::Silent(silent) = &prepared.handle else {
                                panic!("expected pending silent output");
                            };
                            candidate_close = Some(Arc::clone(&silent.close));
                            assert_eq!(
                                prepared.gate.state.load(Ordering::Acquire),
                                CALLBACK_PENDING
                            );
                            Ok(prepared)
                        },
                        || {
                            checks.set(checks.get() + 1);
                            checks.get() == cancel_at
                        },
                    )
                    .expect_err("cancel output replacement");
                assert!(matches!(error, DevicePlaybackError::Cancelled));
                assert_eq!(checks.get(), cancel_at);
                assert_eq!(samples[0].1.identity(), identity);
                assert_eq!(samples[0].1.pcm().as_ptr(), pcm);
                assert_eq!(samples[0].1.stereo_at(0.0), Some((0.75, 0.75)));
                assert_eq!(device.audio_facts(), facts, "no candidate facts committed");
                if cancel_at <= 2 {
                    assert!(device.has_output());
                    assert!(!old_close.load(Ordering::Acquire));
                    device.check_health().expect("old output untouched");
                    assert!(candidate_close.is_none());
                } else {
                    assert!(!device.has_output());
                    assert!(old_close.load(Ordering::Acquire));
                    assert_eq!(Arc::strong_count(&old_close), 1, "old callback joined");
                    assert!(device.check_health().is_err());
                    if cancel_at == 3 {
                        assert!(
                            candidate_close.is_none(),
                            "Stop after teardown skips preparation"
                        );
                    } else {
                        let close = candidate_close.expect("candidate prepared");
                        assert!(close.load(Ordering::Acquire));
                        assert_eq!(Arc::strong_count(&close), 1, "pending callback joined");
                    }
                }
            }
        }

        #[test]
        fn portable_selection_survives_silent_recycle_and_asset_preparation() {
            let options = LiveOutputOptions::default().with_dispatch(DspDispatch::portable());
            let mut device = LiveScalarDevice::start_silent_with_options(48_000, 7, options)
                .expect("portable silent output");
            device.recycle_output().expect("recycle silent output");
            assert!(device.dispatch().is_forced_portable());
            assert!(device.output_options().dispatch().is_forced_portable());

            // Joining the sole callback leaves the test as the only asset-ring
            // consumer, so it can inspect the prepared installs without a race.
            device.silent.take().expect("silent callback").stop();
            let params = crate::reverb::ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            device.ensure_reverb(0, params);
            let install = device
                .shared
                .samples
                .reverb_installs
                .pop()
                .expect("prepared orbit reverb");
            // SAFETY: the callback is joined and popping transferred the unique
            // allocation to this thread. No other owner can read or free it.
            let reverb = unsafe { Box::from_raw(install.reverb) };
            assert!(reverb.dispatch().is_forced_portable());
            assert_eq!(
                reverb.dispatch().convolution_kernel_kind(),
                crate::ConvolutionKernelKind::Portable,
            );

            device.ensure_fx_reverb(params);
            let install = device
                .shared
                .samples
                .fx_reverb_installs
                .pop()
                .expect("prepared stage reverb");
            // SAFETY: as above, this thread now exclusively owns the popped box.
            let reverb = unsafe { Box::from_raw(install.reverb) };
            assert!(reverb.dispatch().is_forced_portable());
            assert_eq!(
                reverb.dispatch().convolution_kernel_kind(),
                crate::ConvolutionKernelKind::Portable,
            );
        }

        #[test]
        fn frame_rebase_preserves_time_in_both_rate_directions() {
            assert_eq!(rebase_frame_rate(48_000, 48_000, 44_100), 44_100);
            assert_eq!(rebase_frame_rate(24_000, 48_000, 44_100), 22_050);
            assert_eq!(rebase_frame_rate(44_100, 44_100, 48_000), 48_000);
            assert_eq!(rebase_frame_rate(22_050, 44_100, 48_000), 24_000);

            for (frame, old_rate, new_rate) in [
                (1, 48_000, 44_100),
                (1, 44_100, 48_000),
                (98_765, 96_000, 44_100),
                (98_765, 44_100, 96_000),
            ] {
                let rebased = rebase_frame_rate(frame, old_rate, new_rate);
                assert!(
                    u128::from(rebased) * u128::from(old_rate)
                        >= u128::from(frame) * u128::from(new_rate),
                    "recycle moved the clock backwards: {frame}@{old_rate} -> {rebased}@{new_rate}"
                );
                assert!(
                    u128::from(rebased.saturating_sub(1)) * u128::from(old_rate)
                        < u128::from(frame) * u128::from(new_rate),
                    "recycle rounded forward by more than one new-rate frame"
                );
            }
        }

        #[test]
        fn recycled_stream_rebases_takeover_and_resets_stream_local_clock_state() {
            let shared = LiveShared::new(7);
            shared.frames.store(48_000, Ordering::Release);
            shared
                .rendering_until_frame
                .store(49_000, Ordering::Release);
            shared.takeover_frame.store(24_000, Ordering::Release);
            shared.playback_origin_nanos.store(123, Ordering::Release);
            shared.buffer_playback_nanos.store(456, Ordering::Release);
            shared.playback_latency_nanos.store(789, Ordering::Release);
            shared.last_callback_nanos.store(999, Ordering::Release);

            shared.prepare_recycled_stream(48_000, 44_100);
            assert_eq!(shared.clock_frames(), 44_100);
            assert_eq!(shared.render_frontier_frames(), 44_100);
            assert_eq!(shared.takeover_frame.load(Ordering::Acquire), 22_050);
            assert_eq!(
                shared.playback_origin_nanos.load(Ordering::Acquire),
                UNSET_STREAM_INSTANT_NANOS
            );
            assert_eq!(shared.buffer_playback_nanos.load(Ordering::Acquire), 0);
            assert_eq!(shared.playback_latency_nanos.load(Ordering::Acquire), 0);
            assert_eq!(shared.last_callback_nanos.load(Ordering::Acquire), 0);

            shared.prepare_recycled_stream(44_100, 48_000);
            assert_eq!(shared.clock_frames(), 48_000);
            assert_eq!(shared.render_frontier_frames(), 48_000);
            assert_eq!(shared.takeover_frame.load(Ordering::Acquire), 24_000);
            assert_eq!(shared.generation.load(Ordering::Acquire), 7);
        }

        /// A countdown can span an output change: a quantised launch arms its
        /// line, then the output is recycled onto a device at another rate
        /// before the line. The armed frame moves with the clock, its flag bits
        /// kept; an empty word and the withdrawal marker stay as they are.
        #[test]
        fn recycled_stream_rebases_an_armed_line_and_keeps_its_flags() {
            let shared = LiveShared::new(7);
            for drop_bits in [0b01u64, 0b11] {
                shared
                    .line_arm
                    .store((44_100 << 2) | drop_bits, Ordering::Release);
                shared.prepare_recycled_stream(44_100, 48_000);
                let word = shared.line_arm.load(Ordering::Acquire);
                assert_eq!(word >> 2, 48_000, "the line is one second in either domain");
                assert_eq!(word & 0b11, drop_bits);
            }
            for unarmed in [0, crate::LINE_ARM_WITHDRAWN] {
                shared.line_arm.store(unarmed, Ordering::Release);
                shared.prepare_recycled_stream(48_000, 44_100);
                assert_eq!(shared.line_arm.load(Ordering::Acquire), unarmed);
            }
        }

        #[test]
        fn recycled_stream_discards_pending_kabel_at_every_sample_rate() {
            for new_rate in [48_000, 44_100] {
                let mut device = LiveScalarDevice::start_silent(48_000, 7).unwrap();
                device.silent.take().unwrap().stop();
                let event = kabel_event(kabel_program(), 1);
                let publication = device
                    .prepare_kabel_events(std::slice::from_ref(&event))
                    .unwrap();
                let channel = &device.shared.samples;
                assert_eq!(channel.program_installs.len(), 1);
                assert_eq!(channel.kabel_installs.len(), 1);

                // The old callback has joined and replacement callbacks remain
                // gated while queues and their producer reservations are reset.
                device.shared.prepare_recycled_stream(48_000, new_rate);
                device.clear_reverb_caches();
                drop(publication);

                assert_eq!(channel.program_installs.len(), 0);
                assert_eq!(channel.kabel_installs.len(), 0);
                assert!(device.kabel_cache.lock().unwrap().is_empty());
                assert!(device.program_cache.lock().unwrap().is_empty());
                let _publication = device.prepare_kabel_events(&[event]).unwrap();
                assert_eq!(channel.program_installs.len(), 1);
                assert_eq!(channel.kabel_installs.len(), 1);
                assert_eq!(channel.leaked.load(Ordering::Relaxed), 0);
            }
        }

        #[test]
        fn recycled_stream_discards_only_rate_specific_pending_reverbs() {
            use crate::assets::{
                FxReverbInstall, ReturnedFxReverb, ReturnedReverb, ReturnedSample,
            };
            use crate::assets::{ReverbInstall, SampleInstall};
            use crate::reverb::{OrbitReverb, ReverbParams};
            use crate::sample::{DecodedSample, SampleId};

            fn enqueue<T>(value: T, send: impl FnOnce(*mut T) -> bool) -> *mut T {
                let pointer = Box::into_raw(Box::new(value));
                if !send(pointer) {
                    // SAFETY: a refused enqueue leaves this allocation here.
                    drop(unsafe { Box::from_raw(pointer) });
                    panic!("fresh asset queue refused an entry");
                }
                pointer
            }

            let params = ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            for new_rate in [48_000, 44_100] {
                // No callback owns these queues: preparation runs after the old
                // stream joins and before the replacement gate opens.
                let shared = LiveShared::new(7);
                let channel = &shared.samples;
                let orbit = || OrbitReverb::generate(48_000, params);
                let stage = || OrbitReverb::generate_streaming(48_000, params);
                let pending_orbits: [_; 2] = std::array::from_fn(|index| {
                    enqueue(orbit(), |reverb| {
                        channel
                            .reverb_installs
                            .push(ReverbInstall {
                                orbit: index as u8,
                                reverb,
                            })
                            .is_ok()
                    })
                });
                let pending_stages: [_; 2] = std::array::from_fn(|_| {
                    enqueue(stage(), |reverb| {
                        channel
                            .fx_reverb_installs
                            .push(FxReverbInstall { reverb })
                            .is_ok()
                    })
                });
                let decoded = DecodedSample::from_parts(96_000, 1, vec![0.25, 0.5]).unwrap();
                let sample = enqueue(decoded.clone(), |sample| {
                    channel
                        .installs
                        .push(SampleInstall {
                            id: SampleId(8),
                            sample,
                        })
                        .is_ok()
                });
                let returned_sample = enqueue(decoded.clone(), |sample| {
                    channel.returns.push(ReturnedSample(sample)).is_ok()
                });
                let returned_orbit = enqueue(orbit(), |reverb| {
                    channel.reverb_returns.push(ReturnedReverb(reverb)).is_ok()
                });
                let returned_stage = enqueue(stage(), |reverb| {
                    channel
                        .fx_reverb_returns
                        .push(ReturnedFxReverb(reverb))
                        .is_ok()
                });
                assert_eq!(channel.reverb_installs.len(), 2);
                assert_eq!(channel.fx_reverb_installs.len(), 2);

                shared.prepare_recycled_stream(48_000, new_rate);

                // Decoded PCM carries its own rate; returns already belong to the
                // producer and must not be consumed by stream preparation.
                assert_eq!(channel.installs.len(), 1);
                assert_eq!(channel.returns.len(), 1);
                assert_eq!(channel.reverb_returns.len(), 1);
                assert_eq!(channel.fx_reverb_returns.len(), 1);
                let installed = channel.installs.pop().unwrap();
                // SAFETY: the pop transfers the queue's unique allocation here.
                let installed_pcm = unsafe { Box::from_raw(installed.sample) };
                assert_eq!(installed.id, SampleId(8));
                assert_eq!(installed.sample, sample);
                assert_eq!(*installed_pcm, decoded);
                let returned = channel.returns.pop().unwrap();
                // SAFETY: this returned allocation has been popped exactly once.
                let returned_pcm = unsafe { Box::from_raw(returned.0) };
                assert_eq!(returned.0, returned_sample);
                assert_eq!(*returned_pcm, decoded);
                let returned = channel.reverb_returns.pop().unwrap();
                // SAFETY: this is the sole owner after popping the return.
                let orbit_return = unsafe { Box::from_raw(returned.0) };
                assert_eq!(returned.0, returned_orbit);
                let returned = channel.fx_reverb_returns.pop().unwrap();
                // SAFETY: this is a distinct, once-popped stage allocation.
                let stage_return = unsafe { Box::from_raw(returned.0) };
                assert_eq!(returned.0, returned_stage);
                drop((orbit_return, stage_return));

                if new_rate == 48_000 {
                    for (index, expected) in pending_orbits.into_iter().enumerate() {
                        let install = channel.reverb_installs.pop().unwrap();
                        // SAFETY: same-rate preparation left the queued owner.
                        let reverb = unsafe { Box::from_raw(install.reverb) };
                        assert_eq!(install.orbit, index as u8);
                        assert_eq!(install.reverb, expected);
                        drop(reverb);
                    }
                    for expected in pending_stages {
                        let install = channel.fx_reverb_installs.pop().unwrap();
                        // SAFETY: as above, the pop transfers unique ownership.
                        let reverb = unsafe { Box::from_raw(install.reverb) };
                        assert_eq!(install.reverb, expected);
                        drop(reverb);
                    }
                }
                assert_eq!(channel.reverb_installs.len(), 0);
                assert_eq!(channel.fx_reverb_installs.len(), 0);
                assert_eq!(channel.leaked.load(Ordering::Relaxed), 0);
                assert_eq!(shared.generation.load(Ordering::Acquire), 7);
            }
        }
    }
    mod output_selection {
        use super::*;

        #[test]
        fn invalid_buffer_is_rejected_before_output_discovery() {
            let invalid = AudioBufferPreference::Frames(0);
            let options = LiveOutputOptions::default().with_buffer_preference(invalid);
            let expected = invalid
                .validate()
                .expect_err("invalid preference")
                .to_string();
            let error = LiveScalarDevice::start_output_with_options(
                Some("missing-output-for-buffer-validation"),
                1,
                options,
            )
            .err()
            .expect("invalid preference must not open output");
            assert_eq!(error.to_string(), expected);
            let error = LiveScalarDevice::start_silent_with_options(48_000, 1, options)
                .err()
                .expect("invalid preference must not prepare silent output");
            assert_eq!(error.to_string(), expected);
        }

        #[test]
        fn live_environment_controls_use_the_product_namespace() {
            assert_eq!(MAX_LIVE_LATENCY_ENV, "RUSTEL_MAX_LIVE_LATENCY_MS");
            assert_eq!(LIVE_BUFFER_FRAMES_ENV, "RUSTEL_LIVE_BUFFER_FRAMES");
            assert_eq!(PLUGIN_THREADS_ENV, "RUSTEL_PLUGIN_THREADS");
            // The options set the plugin workers before the environment,
            // and the cores of the process set them last.
            let plugin_threads = super::super::inserts::plugin_threads;
            let cores = crate::pool::plugin_workers();
            assert_eq!(plugin_threads(Some(3), Some("5")), 3);
            assert_eq!(plugin_threads(Some(0), None), 0);
            assert_eq!(plugin_threads(None, Some("5")), 5);
            assert_eq!(plugin_threads(None, Some("0")), 0);
            assert_eq!(plugin_threads(None, Some("five")), cores);
            assert_eq!(plugin_threads(None, None), cores);
        }

        #[test]
        fn live_buffer_request_stays_inside_the_advertised_device_range() {
            // The platform default is environment-dependent: WSL's forwarded
            // sink underruns at 256 frames, so the selector prefers 2048 there.
            let platform_default = if std::env::var_os("WSL_DISTRO_NAME").is_some()
                || std::env::var_os("WSL_INTEROP").is_some()
            {
                2048
            } else {
                TARGET_LIVE_BUFFER_FRAMES
            };
            let env_override = std::env::var(LIVE_BUFFER_FRAMES_ENV)
                .ok()
                .and_then(|frames| frames.parse::<u32>().ok())
                .filter(|frames| {
                    (AudioBufferPreference::MIN_FRAMES..=AudioBufferPreference::MAX_FRAMES)
                        .contains(frames)
                });
            let target = env_override.unwrap_or(platform_default);
            let select = |range: &SupportedBufferSize| {
                select_live_buffer_frames(range, AudioBufferPreference::Auto)
                    .expect("supported buffer range")
            };
            assert_eq!(select(&SupportedBufferSize::Unknown), target);
            for (min, max) in [(64, 1024), (512, 2048), (32, 128)] {
                assert_eq!(
                    select(&SupportedBufferSize::Range { min, max }),
                    target.clamp(min, max),
                    "{min}..={max}"
                );
            }
        }

        #[test]
        fn explicit_buffer_selection_ignores_automatic_defaults() {
            for (range, expected, context) in [
                (SupportedBufferSize::Unknown, 96, "explicit buffer"),
                (
                    SupportedBufferSize::Range {
                        min: 256,
                        max: 1024,
                    },
                    256,
                    "clamped buffer",
                ),
            ] {
                assert_eq!(
                    select_live_buffer_frames(&range, AudioBufferPreference::Frames(96))
                        .expect(context),
                    expected,
                    "{context}"
                );
            }
        }

        /// WASAPI reports its shared period as `min == max`. Treat it as a minimum:
        /// larger requests pass through and smaller requests use the period.
        /// Other hosts keep fixed sizes and real ranges as hard constraints.
        /// This test needs no device and runs on every platform.
        #[test]
        fn a_fixed_shared_mode_period_is_a_floor_not_a_ceiling() {
            use AudioBufferPreference::Frames;
            let period = SupportedBufferSize::Range { min: 480, max: 480 };
            let floor = live_buffer_range(&period, true);
            assert_eq!(Frames(2_048).resolve(256, floor), Ok(2_048));
            assert_eq!(Frames(16_384).resolve(256, floor), Ok(16_384));
            assert_eq!(Frames(32).resolve(256, floor), Ok(480), "the period floor");
            assert_eq!(AudioBufferPreference::Auto.resolve(128, floor), Ok(480));
            let hard = live_buffer_range(&period, false);
            assert_eq!(Frames(2_048).resolve(256, hard), Ok(480));
            for fixed_period_is_floor in [false, true] {
                let ranged = SupportedBufferSize::Range {
                    min: 64,
                    max: 1_024,
                };
                assert_eq!(
                    Frames(2_048).resolve(256, live_buffer_range(&ranged, fixed_period_is_floor)),
                    Ok(1_024)
                );
                assert_eq!(
                    live_buffer_range(&SupportedBufferSize::Unknown, fixed_period_is_floor),
                    AudioBufferRange::Unknown
                );
            }
            assert_eq!(fixed_device_period(&period), Some(480));
            assert_eq!(
                fixed_device_period(&SupportedBufferSize::Range {
                    min: 64,
                    max: 1_024
                }),
                None
            );
            // The live selector applies the floor on Windows, where WASAPI is
            // the host, and keeps the hard constraint elsewhere.
            assert_eq!(
                select_live_buffer_frames(&period, Frames(2_048)).expect("fixed period"),
                if cfg!(windows) { 2_048 } else { 480 }
            );
        }

        #[test]
        fn listed_device_match_prefers_id_then_exact_friendly_then_disambiguated() {
            let candidates = [
                ("id-a".to_owned(), "Speakers".to_owned(), 0u8),
                ("id-b".to_owned(), "Speakers".to_owned(), 1u8),
                ("id-c".to_owned(), "Microphone".to_owned(), 2u8),
                ("id-d".to_owned(), "Mic".to_owned(), 3u8),
            ];
            // Exact "mic" must beat the earlier substring in "Microphone".
            for (requested, expected) in [
                ("id-b", Some(1)),
                ("ID-B", Some(1)),
                ("Speakers", Some(0)),
                ("Speakers (id-b)", Some(1)),
                ("microphone", Some(2)),
                ("mic", Some(3)),
                ("MIC", Some(3)),
                ("phone", Some(2)),
                ("missing", None),
            ] {
                assert_eq!(
                    match_listed_device(&candidates, requested),
                    expected,
                    "{requested}"
                );
            }
        }
    }
    #[cfg(feature = "test-support")]
    mod sample_resampling {
        use super::*;
        use crate::{DecodedSample, SampleControls, SampleHold, SampleId, SampleResamplingMode};

        const SAMPLE_ID: SampleId = SampleId(9);
        const SOURCE_RATE: u32 = 24_000;
        const OUTPUT_RATE: u32 = 48_000;

        fn sample() -> DecodedSample {
            let pcm = (0..SOURCE_RATE * 8)
                .map(|frame| if frame % 2 == 0 { 0.0 } else { 0.2 })
                .collect();
            DecodedSample::from_parts(SOURCE_RATE, 1, pcm).expect("local sample")
        }

        fn event(generation: u64, target_frame: u64) -> AudioEvent {
            AudioEvent {
                onset_id: 1,
                generation,
                target_frame,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 1.0,
                duration_secs: 8.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: Some(SampleControls {
                    sample: SAMPLE_ID,
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
                }),
                synth: None,
                wavetable: None,
                cut: None,
            }
        }

        #[test]
        fn manual_callback_uses_the_selected_sample_resampling_mode() {
            let _tripwire_guard = tripwire_assertion_guard();
            let samples = [(SAMPLE_ID, sample())];
            let render = |mode: Option<SampleResamplingMode>| {
                let mut output = match mode {
                    Some(mode) => ManualLiveOutput::new_with_samples_and_options(
                        OUTPUT_RATE,
                        7,
                        &samples,
                        LiveOutputOptions::default().with_sample_resampling_mode(mode),
                    ),
                    None => ManualLiveOutput::new_with_samples(OUTPUT_RATE, 7, &samples),
                }
                .expect("manual output");
                output.device().set_limiter(None);
                assert!(output.device().push(event(7, 0)));
                let mut pcm = [0.0; 1024 * 2];
                let before = tripwire::Violations::capture();
                output.render(&mut pcm);
                assert!(tripwire::Violations::capture().since(before).clean());
                assert!(pcm.iter().all(|sample| sample.is_finite()));
                assert!(pcm.iter().any(|sample| sample.abs() > 1e-4));
                let report = output.device().report();
                assert_eq!(report.accepted_events, 1);
                assert_eq!(report.ring_role_conflicts, 0);
                assert_eq!(report.callback_scope_misses, 0);
                pcm
            };

            assert_eq!(
                LiveOutputOptions::default().sample_resampling_mode(),
                SampleResamplingMode::Linear
            );
            let linear = render(Some(SampleResamplingMode::Linear));
            let raw = render(Some(SampleResamplingMode::Raw));
            assert_eq!(
                render(None),
                linear,
                "default output keeps linear interpolation"
            );
            assert!(
                linear
                    .iter()
                    .zip(raw.iter())
                    .any(|(a, b)| (a - b).abs() > 1e-4),
                "fractional source positions must change callback PCM"
            );
        }

        fn assert_silent_pcm(device: &LiveScalarDevice, mode: SampleResamplingMode) {
            assert_eq!(device.sample_rate(), OUTPUT_RATE);
            assert_eq!(device.output_options().sample_resampling_mode(), mode);
            let accepted = device.report().accepted_events;
            let target_frame =
                device.clock_frames() + u64::from(device.requested_buffer_frames()) * 3;
            assert!(device.push(event(device.generation(), target_frame)));
            let mut pcm = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(snapshot) = device.copy_analysis_window(&mut pcm)
                    && snapshot.end_frame >= target_frame + LIVE_ANALYSIS_WINDOW_SAMPLES as u64
                    && device.report().accepted_events > accepted
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "silent output did not render the sample"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            let tail = &pcm[pcm.len() - 256..];
            assert!(tail.iter().all(|sample| sample.is_finite()));
            assert!(tail.iter().any(|sample| sample.abs() > 1e-4));
            // At half-rate playback, Raw repeats each zero twice. Linear inserts a
            // nonzero midpoint, so only one in four output frames stays zero.
            let zeroes = tail.iter().filter(|sample| **sample == 0.0).count();
            assert_eq!(
                zeroes,
                match mode {
                    SampleResamplingMode::Raw => 128,
                    SampleResamplingMode::Linear => 64,
                },
                "silent callback must use {mode:?} interpolation"
            );
        }

        #[test]
        fn silent_output_retains_sample_resampling_through_replacement_and_retry() {
            let _tripwire_guard = tripwire_assertion_guard();
            let samples = [(SAMPLE_ID, sample())];
            for mode in [SampleResamplingMode::Raw, SampleResamplingMode::Linear] {
                let options = LiveOutputOptions::default()
                    .with_buffer_preference(AudioBufferPreference::Frames(128))
                    .with_sample_resampling_mode(mode);
                let mut device = LiveScalarDevice::start_output_with_options(
                    Some(SILENT_OUTPUT_NAME),
                    7,
                    options,
                )
                .expect("silent selector");
                device.set_analysis_enabled(true);
                device
                    .install_sample(SAMPLE_ID, samples[0].1.clone())
                    .expect("install sample");
                assert_silent_pcm(&device, mode);

                device.set_buffer_preference(AudioBufferPreference::Frames(257));
                assert_eq!(device.output_options().sample_resampling_mode(), mode);
                device
                    .recycle_output_to_with_samples(Some(SILENT_OUTPUT_NAME), &samples, || false)
                    .expect("replace silent output");
                assert_eq!(device.requested_buffer_frames(), 257);
                assert_silent_pcm(&device, mode);

                assert!(device.fail_output_replacement_for_test().is_err());
                assert_eq!(device.output_options().sample_resampling_mode(), mode);
                device
                    .recycle_output_to_with_samples(Some(SILENT_OUTPUT_NAME), &samples, || false)
                    .expect("retry silent output");
                assert_eq!(device.requested_buffer_frames(), 257);
                assert_silent_pcm(&device, mode);
            }
        }
    }
    mod stream_errors {
        use super::*;

        fn live_errors(gate: &Arc<CallbackGate>, shared: &LiveShared) -> impl FnMut(cpal::Error) {
            live_error_callback(
                Arc::clone(gate),
                shared.failed.clone(),
                shared.callback_errors.clone(),
                shared.callback_xruns.clone(),
            )
        }

        #[test]
        fn an_xrun_leaves_every_stream_running() {
            let xrun = || cpal::Error::with_message(cpal::ErrorKind::Xrun, "overload");

            let failed = Arc::new(AtomicBool::new(false));
            flag_stream_failure(Arc::clone(&failed))(xrun());
            assert!(!failed.load(Ordering::Acquire));

            let shared = LiveShared::new(7);
            let active = Arc::new(CallbackGate::new());
            active.activate().expect("active output");
            live_errors(&active, &shared)(xrun());
            assert!(active.is_active());
            assert_eq!(shared.report(1).callback_xruns, 1);
            let pending = Arc::new(CallbackGate::new());
            live_errors(&pending, &shared)(xrun());
            assert!(!shared.failed.load(Ordering::Acquire));
            assert_eq!(shared.callback_errors.load(Ordering::Acquire), 0);
            assert_eq!(shared.report(1).callback_xruns, 1);
            let writer = active.begin_error();
            assert_eq!(active.state.load(Ordering::SeqCst), CALLBACK_ACTIVE);
            let (retired_tx, retired_rx) = std::sync::mpsc::sync_channel(1);
            let retiring = Arc::clone(&active);
            let retirement = std::thread::spawn(move || {
                retiring.retire();
                retired_tx.send(()).expect("retirement receiver");
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while active.state.load(Ordering::SeqCst) != CALLBACK_RETIRED {
                assert!(Instant::now() < deadline, "retirement did not start");
                std::thread::yield_now();
            }
            assert!(matches!(
                retired_rx.recv_timeout(Duration::from_millis(20)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            // An admitted notification can finish before replacement status resets.
            shared.callback_errors.fetch_add(1, Ordering::Relaxed);
            shared.failed.store(true, Ordering::Release);
            drop(writer);
            retired_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("retirement waits for the admitted notification");
            retirement.join().expect("retirement thread");
            assert!(shared.failed.load(Ordering::Acquire));
            shared.failed.store(false, Ordering::Release);
            assert!(!active.is_active());
            assert!(active.activate().is_err());
            live_errors(&active, &shared)(xrun());
            live_errors(&active, &shared)(cpal::ErrorKind::DeviceNotAvailable.into());
            assert!(!shared.failed.load(Ordering::Acquire));
            assert_eq!(shared.callback_errors.load(Ordering::Acquire), 1);
            assert_eq!(shared.report(1).callback_xruns, 1);
            pending
                .activate()
                .expect("an xrun before activation leaves the candidate pending");
            live_errors(&pending, &shared)(xrun());
            assert!(!shared.failed.load(Ordering::Acquire));
            assert_eq!(shared.report(1).callback_xruns, 2);
        }

        #[test]
        fn a_lost_device_still_fails_the_stream() {
            for kind in [
                cpal::ErrorKind::DeviceNotAvailable,
                cpal::ErrorKind::StreamInvalidated,
            ] {
                let failed = Arc::new(AtomicBool::new(false));
                flag_stream_failure(Arc::clone(&failed))(kind.into());
                assert!(failed.load(Ordering::Acquire), "{kind:?}");

                let shared = LiveShared::new(7);
                let active = Arc::new(CallbackGate::new());
                active.activate().expect("active output");
                live_errors(&active, &shared)(kind.into());
                assert!(shared.failed.load(Ordering::Acquire), "{kind:?}");
                assert_eq!(shared.callback_errors.load(Ordering::Acquire), 1);
                assert_eq!(shared.report(1).callback_xruns, 0);
            }
        }
    }
}
