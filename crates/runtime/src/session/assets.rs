use super::*;

impl Session {
    /// Start the manifest-verified default sample library.
    ///
    /// Construction does no network I/O. Pinned manifests are queued on the
    /// library's bounded loader and failures surface through the normal
    /// loud-once sample failure channel; this only returns an error when the
    /// compiled manifest metadata or loader construction itself is invalid.
    pub fn enable_default_samples(&mut self) -> Result<(), RuntimeError> {
        if self.samples.is_none() {
            let library = crate::samples::SampleLibrary::load_default_async_for_session()
                .map_err(RuntimeError::Message)?;
            library.set_direct_diagnostic_logging(self.direct_diagnostic_logging);
            library.set_render_rate(self.config.sample_rate);
            self.samples = Some(std::sync::Arc::new(library));
        }
        Ok(())
    }

    pub fn sample_library(&self) -> Option<&std::sync::Arc<crate::samples::SampleLibrary>> {
        self.samples.as_ref()
    }

    /// The sample library's settled epoch ([`crate::samples::SampleLibrary::settled_epoch`]):
    /// unchanged since a window was refused as still loading means that
    /// window would be refused again. Zero without a library, where nothing
    /// loads.
    #[cfg(feature = "device-audio")]
    pub(crate) fn sample_settled_epoch(&self) -> u64 {
        self.samples
            .as_ref()
            .map_or(0, |library| library.settled_epoch())
    }

    /// Tell live conversion which channels an open input can provide. Without
    /// an open input, `in` remains a silent source; the native channel ceiling
    /// is still enforced by voice conversion regardless of this observation.
    pub fn set_audio_input_channels(&mut self, channels: Option<usize>) {
        let channels = channels
            .filter(|channels| *channels > 0)
            .map(|channels| channels.min(rustel_audio::input::MAX_INPUT_CHANNELS));
        if channels != self.audio_input_channels {
            // Another input: what it lacks is news again.
            self.input_warnings_reported.clear();
        }
        self.audio_input_channels = channels;
    }

    /// A voice on an input channel the open input does not have plays
    /// silence, because the ring reads nothing past its channels, as
    /// `s("in")` does with no input open. The warning is reported once. It
    /// is not a refusal: a refusal would reject the whole replacement, and
    /// the rollback would fail too when the last audible score reads the
    /// same channel, for example after a microphone is swapped for a mono
    /// one under a running score.
    pub(super) fn input_channel_warning(&self, event: &rustel_audio::AudioEvent) -> Option<String> {
        let channels = self.audio_input_channels?;
        let Some(rustel_audio::SynthSource::Input { channel }) = event.synth else {
            return None;
        };
        (usize::from(channel) >= channels).then(|| {
            format!(
                "in:{channel} is unavailable: the open input provides channels 0..{} - it plays silence",
                channels - 1
            )
        })
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn set_sample_library_for_test(&mut self, library: Arc<crate::samples::SampleLibrary>) {
        self.samples = Some(library);
    }

    /// Fetch the map of a `samples("…")` a score names, ahead of the score
    /// being evaluated, under the same grant an evaluation would use. See
    /// [`crate::samples::SampleLibrary::look_up_samples_source`].
    pub fn look_up_samples_source(&mut self, spec: &str) -> Result<(), RuntimeError> {
        if self.samples.is_none() {
            self.enable_default_samples()?;
        }
        let Some(library) = self.samples.clone() else {
            return Ok(());
        };
        library
            .look_up_samples_source(spec, &self.config.score_sample_access)
            .map_err(RuntimeError::Message)
    }

    /// Start loading every sound that a saved score's text names, without
    /// waiting.
    ///
    /// A watch-save is on disk one debounce before it installs. Until then
    /// the pattern in the Session is the outgoing one, so a query would warm
    /// the score being replaced. Reading the saved text starts the decode
    /// early. Otherwise a sound that the save introduces (`hh:0`) begins
    /// decoding at the first onset that asks for it, and that onset is
    /// skipped as "still loading".
    ///
    /// The call is asynchronous and runs no query, so it does not spend the
    /// producer's turn. A miss is not an error: the first hits are skipped
    /// and the sound enters later.
    pub fn warm_sounds_in_source(&mut self, source: &str) -> usize {
        let names = crate::sounds::to_warm(source, self.prebake_selects_variants);
        if names.is_empty() {
            return 0;
        }
        // Whatever the library already is: enabling one here would put a
        // default manifest fetch on the producer's thread, and a session
        // with no library has nothing to warm anyway.
        let Some(library) = self.samples.clone() else {
            return 0;
        };
        let files = match library.warm_score_sounds_async(&names, &self.config.score_sample_access)
        {
            Ok(crate::samples::PrefetchStatus::Requested(files)) => files,
            Ok(crate::samples::PrefetchStatus::Deferred)
            | Ok(crate::samples::PrefetchStatus::Unknown) => 0,
            // A save that names something unfetchable is not worth a word:
            // the install behind it reports whatever is actually wrong, and
            // this ran before anyone asked for a sound.
            Err(_) => 0,
        };
        self.preload_requested = self.preload_requested.saturating_add(files);
        files
    }

    /// Start loading these sound names, whatever the score is doing with them.
    ///
    /// Returns how many files could be requested immediately. If a pending
    /// manifest may redefine a name, its request is queued behind that
    /// manifest and is not included in the immediate count.
    pub fn prefetch_sounds(&mut self, names: &[String]) -> usize {
        match self.prefetch_sounds_checked(names) {
            Ok(files) => files,
            Err(error) => {
                self.report_diagnostic(
                    "samples-failed",
                    error.to_string(),
                    serde_json::json!({ "samples_failed": { "message": error.to_string() } }),
                );
                0
            }
        }
    }

    /// Checked form of [`Self::prefetch_sounds`] for clients that own their
    /// diagnostic surface and must not write behind a full-screen UI.
    pub fn prefetch_sounds_checked(&mut self, names: &[String]) -> Result<usize, RuntimeError> {
        if names.is_empty() {
            return Ok(0);
        }
        if self.samples.is_none() {
            self.enable_default_samples()?;
        }
        let Some(library) = self.samples.clone() else {
            return Ok(0);
        };
        let files = match library.register_score_batch_async_for_session(
            Vec::new(),
            names,
            &self.config.score_sample_access,
        ) {
            Ok(crate::samples::PrefetchStatus::Requested(files)) => files,
            Ok(crate::samples::PrefetchStatus::Deferred)
            | Ok(crate::samples::PrefetchStatus::Unknown) => 0,
            Err(error) => return Err(RuntimeError::Message(error)),
        };
        self.preload_requested = self.preload_requested.saturating_add(files);
        Ok(files)
    }

    /// Prepare a live score without downloading unused GM variants.
    /// Actual queried events go first; source-only guesses stay behind them
    /// and behind any manifest that may redefine their sound names.
    ///
    /// Answers the sounds the queried window resolved, as
    /// [`Self::kick_sample_loads_within`] does, and the warm's refusal, if
    /// any, beside them.
    pub fn warm_live_samples_checked(
        &mut self,
        names: &[String],
        from_cycle: f64,
        query_budget: Duration,
    ) -> (
        Vec<(String, crate::sounds::Variants)>,
        Result<(), RuntimeError>,
    ) {
        if let Err(error) = self.enable_default_samples() {
            return (Vec::new(), Err(error));
        }
        let window = self.kick_sample_loads_within(from_cycle, 4.0, query_budget);
        let library = self.samples.as_ref().expect("enabled sample library");
        let files = match library.warm_score_sounds_async(names, &self.config.score_sample_access) {
            Ok(crate::samples::PrefetchStatus::Requested(files)) => files,
            Ok(crate::samples::PrefetchStatus::Deferred)
            | Ok(crate::samples::PrefetchStatus::Unknown) => 0,
            Err(error) => return (window, Err(RuntimeError::Message(error))),
        };
        self.preload_requested = self.preload_requested.saturating_add(files);
        (window, Ok(()))
    }

    /// Hold until the sounds this set is about to play have arrived.
    ///
    /// Startup only, and not conditional on `preload`: a score written for
    /// strudel.cc has no preload line and must play correctly without one.
    /// The caller warms what the opening cycles ask for, an explicit
    /// `preload` adds what the opening does not reveal, and this waits for
    /// both.
    ///
    /// During a live set the producer never blocks on a network. The
    /// studio's load mode decides what waits instead: in wait, a start holds
    /// its downbeat and an edit keeps the last score playing until their
    /// sounds have loaded; in async, both land at once and a late note is
    /// skipped.
    ///
    /// Returns (files an explicit `preload` asked for, whether the deadline
    /// passed). A deadline that passes is not an error: the set starts and
    /// the late hits play when their samples arrive.
    pub fn wait_for_sample_loads(&mut self, deadline: Duration) -> (usize, bool) {
        let requested = std::mem::take(&mut self.preload_requested);
        let Some(library) = self.samples.clone() else {
            return (requested, false);
        };
        let started = Instant::now();
        library.wait_until_idle(deadline);
        (requested, started.elapsed() >= deadline)
    }

    /// Apply one accepted evaluation's `samples(...)` registrations and
    /// `preload(...)` requests as one ordered loader transaction.
    ///
    /// Registration failures are loud per-call logs, never evaluation
    /// errors - strudel.cc logs and plays on. A score that defines sounds
    /// creates the library even when the host had not enabled defaults yet.
    pub(super) fn apply_sample_effects(
        &mut self,
        mut effects: rustel_jsruntime::SamplesEffects,
        names: Vec<String>,
    ) {
        if effects.is_empty() && names.is_empty() {
            return;
        }
        if !effects.is_empty() && self.config.score_sample_access.is_denied() {
            let message =
                "setup/score samples() cannot access files or the network without a host grant";
            self.report_diagnostic(
                "samples-failed",
                message,
                serde_json::json!({
                    "samples_failed": {
                        "message": message
                    }
                }),
            );
            effects.clear();
            if names.is_empty() {
                return;
            }
        }
        if self.samples.is_none()
            && let Err(error) = self.enable_default_samples()
        {
            self.report_diagnostic(
                "samples-failed",
                error.to_string(),
                serde_json::json!({ "samples_failed": { "message": error.to_string() } }),
            );
            return;
        }
        let Some(library) = self.samples.clone() else {
            return;
        };
        let requested = match library.register_score_batch_async_for_session(
            effects,
            &names,
            &self.config.score_sample_access,
        ) {
            Ok(crate::samples::PrefetchStatus::Requested(files)) => files,
            Ok(crate::samples::PrefetchStatus::Deferred)
            | Ok(crate::samples::PrefetchStatus::Unknown) => 0,
            Err(error) => {
                self.report_diagnostic(
                    "samples-failed",
                    error.clone(),
                    serde_json::json!({ "samples_failed": { "message": &error } }),
                );
                0
            }
        };
        self.preload_requested = self.preload_requested.saturating_add(requested);
    }

    pub(super) fn ensure_voicings(&mut self) -> Result<(), RuntimeError> {
        if !self.voicings_ready {
            self.js
                .install_voicings_prebake()
                .map_err(RuntimeError::Js)?;
            self.voicings_ready = true;
        }
        Ok(())
    }

    // Only the device-audio play path consults this today; keep it compiling
    // (not warning) in feature-less builds.
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    pub(super) fn sample_lookup(&self) -> &dyn rustel_voice::SampleLookup {
        match &self.samples {
            Some(library) => library.as_ref(),
            None => &rustel_voice::BundledOnly,
        }
    }

    /// Kick loads for every sample the score references in its first
    /// `cycles`, then wait (bounded) for the library to settle. Called once
    /// before the live transport starts so the first bar is not missing its
    /// drums; NEVER called during a running set (loads stay async there).
    pub fn prefetch_samples(&mut self, cycles: f64, deadline: std::time::Duration) {
        let started = Instant::now();
        self.kick_sample_loads(cycles);
        if let Some(library) = self.samples.clone() {
            library.wait_until_idle(deadline);
            // The first query may have happened while the default/custom
            // manifest was still pending. Once the map exists, resolve the
            // opening window again so its PCM enters the ordinary loader,
            // without granting either phase a fresh deadline.
            if let Some(remaining) = deadline.checked_sub(started.elapsed()) {
                self.kick_sample_loads(cycles);
                library.wait_until_idle(remaining);
            }
        }
        self.log_sample_failures();
    }

    /// Start fetches for the current graph's samples without waiting. A
    /// watch-save that introduces `hh:4` must not wait until the first
    /// converted onset to begin decoding - that onset is skipped as
    /// "still loading". The live loop still delivers via `take_ready`.
    pub fn kick_sample_loads(&mut self, cycles: f64) {
        self.kick_sample_loads_from(0.0, cycles);
    }

    /// Warm the sounds a span of the score will ask for, starting at
    /// `from_cycle`.
    ///
    /// Pass the cycle where the music is, and enough cycles to reach every
    /// name in an alternation such as `<clap:1 clap:6>`, and the next section.
    /// A span fixed at cycle zero misses those sounds, and the artist hears
    /// the later download as a dropped beat.
    ///
    /// Bounded by a wall clock because this runs on the producer thread: a
    /// partial warm-up is fine (the next install warms the rest), a stalled
    /// producer is not.
    pub fn kick_sample_loads_from(&mut self, from_cycle: f64, cycles: f64) {
        self.kick_sample_loads_within(from_cycle, cycles, Duration::from_millis(15));
    }

    /// [`Self::kick_sample_loads_from`] with an explicit query ceiling.
    ///
    /// The 15 ms default exists because a mid-set warm-up runs on the producer
    /// thread, where a long query is a dropout. At STARTUP there is no music to
    /// protect and the opposite is true: a ceiling that refuses the query
    /// resolves no names at all, so nothing is warmed and the set opens on the
    /// downloads it was supposed to avoid. A dense score can then spend about
    /// 15 ms in the query while warming nothing.
    ///
    /// Answers each sound the window resolved, banked spelling included,
    /// with the variants its onsets pick there: what the score is about to
    /// play, names it builds in JavaScript among them. Empty when the query
    /// did not finish.
    ///
    /// The plugins of the window load too, for the score of a live output.
    /// This part needs no sample library.
    pub fn kick_sample_loads_within(
        &mut self,
        from_cycle: f64,
        cycles: f64,
        ceiling: Duration,
    ) -> Vec<(String, crate::sounds::Variants)> {
        let library = self.samples.clone();
        // A session with no sample library still has the plugins of its
        // live output.
        #[cfg(feature = "vst")]
        let plugins = self.plugins_live();
        #[cfg(not(feature = "vst"))]
        let plugins = false;
        if library.is_none() && !plugins {
            return Vec::new();
        }
        let haps = self.window_haps(from_cycle, cycles, ceiling);
        // A plugin loads as a sample does: before the first note asks.
        #[cfg(feature = "vst")]
        if plugins {
            self.ensure_plugins(&haps);
        }
        let Some(library) = library else {
            return Vec::new();
        };
        let mut resolved: std::collections::BTreeMap<String, std::collections::BTreeSet<i64>> =
            std::collections::BTreeMap::new();
        for sound in Self::sounds_in(&haps) {
            // `.bank("tr909")` makes the sound `tr909_bd`. Warming only `bd`
            // fetches a different bank's files, and the lane's first hit is
            // refused. Warm the banked spelling too, as the voice looks it up.
            let variant = crate::sounds::Variants::index(sound.n);
            for name in sound
                .banked
                .as_deref()
                .into_iter()
                .chain([sound.s.as_str()])
            {
                let _ = rustel_voice::SampleLookup::resolve(
                    library.as_ref(),
                    name,
                    sound.n,
                    sound.midi,
                );
                resolved.entry(name.to_owned()).or_default().insert(variant);
            }
        }
        resolved
            .into_iter()
            .map(|(name, variants)| (name, crate::sounds::Variants::Only(variants)))
            .collect()
    }

    /// The sound of every onset the active score has from `from_cycle` for
    /// `cycles`, with the `n` and note it plays; empty when the query does
    /// not finish within `ceiling`. Nothing is loaded.
    pub fn window_sounds(
        &self,
        from_cycle: f64,
        cycles: f64,
        ceiling: Duration,
    ) -> Vec<WindowSound> {
        Self::sounds_in(&self.window_haps(from_cycle, cycles, ceiling))
    }

    /// The plugin copies the notes of the score need from `from_cycle` for
    /// `cycles` that are not ready, at the rate of the output. A copy the
    /// output holds on its slot is ready. A start that waits for its sounds
    /// waits while the count is above 0, and the caller decides how long.
    /// The call makes the host build each copy it counts, and waits for
    /// nothing. A call less than [`PLUGIN_LOOK_EVERY`] after the look of the
    /// same score gives the count of that look. A session with no live
    /// output waits for no copy: the count is 0, with no call to the
    /// plugin host.
    #[cfg(feature = "vst")]
    pub fn plugins_pending(&self, from_cycle: f64, cycles: f64, ceiling: Duration) -> usize {
        if !self.plugins_live() {
            return 0;
        }
        let now = Instant::now();
        let generation = self.generation();
        if let Some((looked, of, pending)) = self.plugin_look.get()
            && of == generation
            && now.duration_since(looked) < PLUGIN_LOOK_EVERY
        {
            return pending;
        }
        // The notes of a look do not play now, so their console lines go.
        let haps = self
            .js
            .with_logs_dropped(|| self.window_haps(from_cycle, cycles, ceiling));
        let pending = self.ensure_plugins(&haps);
        self.plugin_look.set(Some((now, generation, pending)));
        pending
    }

    /// Makes the plugin host build each plugin copy that the haps of the
    /// score in play need, at the rate of the output: see
    /// [`crate::vst::ensure`]. Gives the count of the copies not ready.
    #[cfg(feature = "vst")]
    fn ensure_plugins(&self, haps: &[crate::HapJson]) -> usize {
        let rate = self.plugin_rate.load(std::sync::atomic::Ordering::Relaxed);
        let wants = crate::vst::wants(haps, |value| crate::vst::live_chain(&self.chains, value));
        crate::vst::ensure(&wants, rate).len()
    }

    /// The haps the active score has from `from_cycle` for `cycles`. Empty
    /// when the query does not finish within `ceiling`.
    fn window_haps(&self, from_cycle: f64, cycles: f64, ceiling: Duration) -> Vec<crate::HapJson> {
        let begin = Fraction::from_f64(from_cycle.max(0.0)).unwrap_or(Fraction::ZERO);
        let span = Fraction::from_f64(cycles.max(1.0)).unwrap_or(Fraction::int(2));
        let ceiling = Instant::now() + ceiling;
        rustel_core::with_query_deadline(ceiling, || self.query_report(begin, begin.add(span)))
            .map(|report| report.haps)
            .unwrap_or_default()
    }

    fn sounds_in(haps: &[crate::HapJson]) -> Vec<WindowSound> {
        let mut sounds = Vec::new();
        for hap in haps {
            let crate::ValueJson::Raw(serde_json::Value::Object(object)) = &hap.value else {
                continue;
            };
            let Some(s) = object.get("s").and_then(|s| s.as_str()) else {
                continue;
            };
            let n = object.get("n").and_then(|n| n.as_f64()).unwrap_or(0.0);
            let midi = match object.get("note") {
                Some(serde_json::Value::String(note)) => {
                    rustel_core::util::note_to_midi(note, 3).unwrap_or(36.0)
                }
                Some(serde_json::Value::Number(note)) => note.as_f64().unwrap_or(36.0),
                _ => 36.0,
            };
            sounds.push(WindowSound {
                s: s.to_owned(),
                banked: rustel_voice::bank_prefixed(object, s).ok().flatten(),
                n,
                midi,
            });
        }
        sounds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(test)]
    mod sample_queue_tests {
        //! A score's `samples()` and warm-ups behind a busy manifest loader.

        use super::*;
        use crate::session::test_support::session_with_sample_origin;

        /// Updates made while the loader is busy install at once and say nothing
        /// about the wait. Their `samples()` registrations, a warm-up and a blocking
        /// registrant's map wait in line and land once the loader is free.
        #[test]
        fn samples_behind_a_busy_loader_register_without_a_word() {
            let mut session = session_with_sample_origin("http://127.0.0.1:9");
            session.set_direct_diagnostic_logging(false);
            let library = Arc::new(crate::samples::SampleLibrary::empty());
            session.samples = Some(Arc::clone(&library));
            let hold = library.hold_manifest_worker_for_test();
            let blocking = std::thread::spawn({
                let library = Arc::clone(&library);
                move || {
                    library.register_trusted_custom(
                        r#"{"blocked":"http://127.0.0.1:9/blocked.wav"}"#,
                        None,
                    )
                }
            });

            let started = Instant::now();
            let names = ["one", "two", "three"];
            for name in names {
                session
                    .evaluate(&format!(
                        "samples({{ {name}: ['http://127.0.0.1:9/{name}.wav'] }}); note('c4')"
                    ))
                    .expect("the update installs");
            }
            let (_, warmed) = session.warm_live_samples_checked(
                &["three".to_owned()],
                0.0,
                std::time::Duration::from_millis(50),
            );
            let elapsed = started.elapsed();
            drop(hold);

            warmed.expect("a warm-up waits in line too");
            assert!(
                elapsed < std::time::Duration::from_secs(1),
                "an update waited on the loader: {elapsed:?}"
            );
            let diagnostics = session.take_diagnostics();
            assert!(
                diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic.kind != "samples-failed"),
                "{diagnostics:?}"
            );
            blocking
                .join()
                .expect("the blocking registrant")
                .expect("its map lands");
            library.wait_until_idle(std::time::Duration::from_secs(15));
            for name in names.into_iter().chain(["blocked"]) {
                assert!(library.knows(name), "{name}");
            }
        }
    }

    #[test]
    fn score_sample_effects_have_no_io_capability_by_default() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"samples('http://127.0.0.1:9/strudel.json'); s('bd')"#)
            .expect("the pattern remains usable when registration is denied");
        assert!(
            session.sample_library().is_none(),
            "a denied score-level samples() call initialized an I/O subsystem"
        );
    }
}
