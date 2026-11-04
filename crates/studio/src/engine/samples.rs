use super::*;

impl StudioEngine {
    /// Fetch the map of a `samples("…")` a score names, ahead of the
    /// score's evaluation, under the session's own grant.
    pub fn look_up_samples(&mut self, spec: &str) -> Result<(), RuntimeError> {
        self.session.look_up_samples_source(spec)
    }

    /// What the retained sound costs, kept and otherwise, and the limits
    /// it is held to.
    pub(super) fn sample_memory(&mut self) -> SampleMemory {
        let mut memory = SampleMemory {
            preview_budget_bytes: self.preview_budget_bytes,
            unused_idle: self.unused_sample_idle,
            recent_limit_bytes: self.recent_tabs_allowance(),
            ..SampleMemory::default()
        };
        if self.retained_samples.is_empty() {
            return memory;
        }
        // A display: a stale answer does until the next due pass.
        self.refresh_protection(Instant::now());
        let Some(protection) = &self.protection else {
            return memory;
        };
        for (id, sample) in &self.retained_samples {
            if protection.ids.contains(id) {
                memory.live_bytes += sample.pcm_bytes();
                if protection.recent.contains(id) {
                    memory.recent_bytes += sample.pcm_bytes();
                }
            } else {
                memory.preview_bytes += sample.pcm_bytes();
            }
        }
        memory
    }

    /// What the audio side holds: the open output's own account, the
    /// inputs the engine opened itself and what the engine keeps for the
    /// output. `None` with neither an output nor an input open. Each input
    /// is counted by the stream that fills it: a sample recorded from the
    /// chosen input reads the output's ring or the monitor's, and only one
    /// recorded from the default input has a ring of its own.
    pub(super) fn audio_memory(&self) -> Option<AudioMemory> {
        let device = self.audio_device().map(LiveScalarDevice::memory);
        let monitor = self.input_monitor.as_ref();
        let recording = self
            .sample_recording
            .as_ref()
            .and_then(|recording| recording.own_input.as_ref());
        if device.is_none() && monitor.is_none() && recording.is_none() {
            return None;
        }
        let device = device.unwrap_or_default();
        let own_inputs: usize = monitor
            .into_iter()
            .chain(recording)
            .map(|input| input.ring().touched_bytes())
            .sum();
        let backlog = self
            .live
            .as_ref()
            .map_or(0, |live| live.producer.backlog_bytes());
        Some(AudioMemory {
            event_ring: device.event_rings,
            input: device.input + own_inputs,
            record: device.record,
            reverbs: device.reverbs,
            backend: device.backend,
            other: device.analysis + backlog,
        })
    }

    pub(super) fn warm_source(
        &mut self,
        source: &str,
        query_budget: Duration,
    ) -> Result<(), RuntimeError> {
        let window = self.warm_names(source, query_budget)?;
        // A different score names different sounds, so what the last sweep
        // decided was still wanted has to be decided again.
        if self.current_score != source {
            self.ask_idle_sweep();
            self.current_score = source.to_owned();
            self.current_score_names = protected_names(source);
            self.protection_generation = self.protection_generation.wrapping_add(1);
        }
        if window != self.score_window_names {
            self.score_window_names = window;
            self.protection_generation = self.protection_generation.wrapping_add(1);
        }
        Ok(())
    }

    /// Start the sounds a text names loading, and query the installed
    /// pattern's window, without making the text the score. Answers the
    /// sounds that window resolved: see [`Session::kick_sample_loads_within`].
    pub(super) fn warm_names(
        &mut self,
        source: &str,
        query_budget: Duration,
    ) -> Result<Vec<(String, Variants)>, RuntimeError> {
        let names = rustel_runtime::sounds::to_warm(source, self.every_variant());
        // An update can happen deep into an alternation: warm the window
        // about to play, with its actual note and variant choices first.
        // Source-only bets must not put unused GM fonts ahead of these.
        let from_cycle = if self.live.is_some() {
            self.session.cycle_at_time(self.device_time())
        } else {
            0.0
        };
        // Querying is bounded and delivery remains asynchronous; Ctrl-Enter
        // never waits for a remote sample download.
        let now = self.device_time();
        let (window, warmed) = self.session.with_panic_recovery(now, |session| {
            Ok(session.warm_live_samples_checked(&names, from_cycle, query_budget))
        })?;
        if let Err(error) = warmed {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("sample-prefetch", error.to_string()),
            );
        }
        Ok(window)
    }

    /// Start an armed launch's sounds loading without making it the
    /// current score.
    ///
    /// The playing score still names what the sample sweep must keep. If
    /// the launch became the current score here, the playing score's
    /// samples would count as unused during the countdown, and for good
    /// after a cancelled launch, and the idle sweep could retire them
    /// while the set still sounds them. The playing pattern is not queried
    /// either: until the launch lands, the only installed pattern is the
    /// outgoing one, and its sounds are already loaded. The incoming score
    /// is not installed yet, so its names come from its text. They include
    /// the `<bank>_<name>` that a `.bank()` builds, which a plain scan
    /// misses.
    pub(super) fn warm_incoming(&mut self, source: &str) {
        let names = rustel_runtime::sounds::to_warm(source, self.every_variant());
        if names.is_empty() {
            return;
        }
        if self.session.sample_library().is_none()
            && let Err(error) = self.session.enable_default_samples()
        {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("sample-prefetch", error.to_string()),
            );
            return;
        }
        let Some(library) = self.session.sample_library() else {
            return;
        };
        if let Err(error) =
            library.warm_score_sounds_async(&names, &self.session.config().score_sample_access)
        {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("sample-prefetch", error),
            );
        }
    }

    pub(super) fn collect_session_diagnostics(&mut self) {
        for failure in self.session.take_sample_failures() {
            let diagnostic = self.import_failure(failure);
            queue_diagnostic(&mut self.pending_diagnostics, diagnostic);
        }
        for diagnostic in self.session.take_diagnostics() {
            // The studio log does not list the voice resolver's notices.
            if diagnostic.kind == rustel_runtime::VOICE_NOTICE_DIAGNOSTIC {
                continue;
            }
            // Said once, in one line, when the load is over.
            if diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC {
                self.note_late(&diagnostic.message);
                continue;
            }
            // Nothing was skipped: the header's loading line follows the
            // sound or the plugin copy until its notes play.
            if diagnostic.kind == rustel_runtime::SAMPLE_AWAITED_DIAGNOSTIC {
                continue;
            }
            let level = if diagnostic.kind == "live-error" {
                StudioDiagnosticLevel::Error
            } else if diagnostic.kind == "log" {
                // A `.log()` line is the score talking, not complaining:
                // information, so the studio's log panel keeps it in its
                // quiet floor and nothing paints it as a failure.
                StudioDiagnosticLevel::Info
            } else {
                StudioDiagnosticLevel::Warning
            };
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic {
                    kind: diagnostic.kind,
                    message: diagnostic.message,
                    recoverable: diagnostic.recoverable,
                    level,
                    alert: None,
                },
            );
        }
    }

    pub(super) fn install_ready_samples(&mut self) {
        let Some(device) = self
            .live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref())
        else {
            return;
        };
        let Some(library) = self.session.sample_library() else {
            return;
        };
        let mut ready = library.take_ready().into_iter();
        while let Some((id, decoded)) = ready.next() {
            let retained = decoded.clone();
            if let Err(decoded) = device.install_sample(id, decoded) {
                let mut retry = Vec::with_capacity(ready.len().saturating_add(1));
                retry.push((id, decoded));
                retry.extend(ready);
                library.requeue_ready_batch_before_newer(retry);
                break;
            }
            if (id.0 as usize) < SAMPLE_BANK_CAPACITY {
                self.sample_last_used.insert(id, Instant::now());
                self.retained_samples.insert(id, retained);
            }
        }
        device.reclaim_samples();
    }

    /// Snapshot the ready bank before either fallible output preparation.
    /// The library merge keeps newer publications and forgotten ids ahead
    /// of retained copies. Arrivals after this drain stay on its ready queue.
    pub(super) fn collect_recycle_samples(&mut self) -> Vec<(SampleId, DecodedSample)> {
        let library = self.session.sample_library();
        let ready = if let Some(library) = library {
            requeue_retained_samples(library, &self.retained_samples);
            library.take_ready()
        } else {
            self.retained_samples
                .iter()
                .map(|(id, decoded)| (*id, decoded.clone()))
                .collect()
        };
        let mut slots = vec![None; SAMPLE_BANK_CAPACITY];
        for (id, decoded) in ready {
            let Some(slot) = slots.get_mut(id.0 as usize) else {
                continue;
            };
            if library
                .is_none_or(|library| library.decoded_identity(id) == Some(decoded.identity()))
            {
                *slot = Some(decoded);
            }
        }
        let samples: Vec<_> = slots
            .into_iter()
            .enumerate()
            .filter_map(|(slot, decoded)| decoded.map(|decoded| (SampleId(slot as u32), decoded)))
            .collect();
        // Keep ownership even if preparation tears down the old output and
        // then fails or Stop wins. Stop's normal requeue can recover these.
        let now = Instant::now();
        for (id, decoded) in &samples {
            self.retained_samples.insert(*id, decoded.clone());
            self.sample_last_used.insert(*id, now);
        }
        samples
    }

    pub(super) fn requeue_recycle_samples(&self, samples: Vec<(SampleId, DecodedSample)>) {
        if let Some(library) = self.session.sample_library() {
            library.requeue_ready_batch_before_newer(samples);
        }
    }

    /// Keep a stopped engine's samples to the memory policy.
    ///
    /// The worker runs the playing tick only while there is a device, so
    /// this is the only place a stopped studio enforces the preview budget
    /// and the idle sweep. It used to live on the tick's no-device branch,
    /// which the worker never reaches: once stopped, nothing was ever
    /// dropped, and what browsing decoded stayed for the session.
    ///
    /// A parked output defers the sweep only while it can still sound, so a
    /// keyboard tail or a ringing score keeps its samples. A stopped stream
    /// holds no voice, but its bank still needs an uninstall for each sample
    /// the policy forgets.
    pub(super) fn tend_idle_sample_memory(&mut self) {
        if self.live.is_some() {
            return;
        }
        if let Some(device) = self.piano_output.as_ref() {
            let report = device.report();
            let pressure = report.realtime_pressure;
            // Pressure telemetry can still describe audio before Stop.
            // Stop acknowledgement guarantees that no voice remains.
            if device.stop_pending()
                || self.piano.iter().any(Option::is_some)
                || (!report.stop_acknowledged
                    && (pressure.active_voices > 0 || pressure.pending_events > 0))
            {
                return;
            }
        }
        self.release_every_retiring_sample();
        self.adopt_idle_ready_samples();
        let now = Instant::now();
        self.enforce_sample_memory(now);
        self.free_memory_when_due(now);
    }

    /// Count what the loaders finished while no output is open as retained.
    ///
    /// With no device, nothing installs a finished decode. It waits in the
    /// library's ready queue, and the preview budget and the idle sweep
    /// count only what the engine retains. Browsing the Examples while
    /// stopped warms every row the cursor rests on, whole soundfonts
    /// included, so without this step a stopped studio grows by every
    /// sound it passes while the budget reads under its ceiling. Retaining
    /// the queue is the state a stop already leaves behind (retained, and
    /// requeued for the next device), so the policy that follows applies
    /// unchanged: what it forgets leaves the queue with the library's
    /// tables, and the next device installs only what is left.
    fn adopt_idle_ready_samples(&mut self) {
        let Some(library) = self.session.sample_library() else {
            return;
        };
        let retained = &self.retained_samples;
        let waiting = library.peek_ready_unless(|id| retained.contains_key(&id));
        if waiting.is_empty() {
            return;
        }
        let now = Instant::now();
        for (id, decoded) in waiting {
            if (id.0 as usize) < SAMPLE_BANK_CAPACITY {
                self.sample_last_used.insert(id, now);
                self.retained_samples.insert(id, decoded);
            }
        }
    }

    /// Replace the origins of the permissions page. The next evaluation imports
    /// scripts from them and from the origins the session config trusts.
    pub fn set_import_origins(&mut self, origins: &[String]) {
        self.config.import_origins = origins.to_vec();
        self.session.set_score_import_access(import_access(
            &self.config.session,
            &self.config.import_origins,
            self.config.csound_allowed,
        ));
    }

    /// Replace what the permissions page permits a score's `samples()`,
    /// images and videos to read. The next evaluation and the next pack
    /// import read it. Banks already loaded stay.
    pub fn set_sample_grants(&mut self, grants: SampleGrants) {
        let access = sample_access(&self.config.session, &grants);
        self.config.sample_grants = grants;
        if let Some(library) = self.session.sample_library() {
            library.set_import_policy(access.clone());
        }
        self.session.set_score_sample_access(access);
    }

    /// Flip the permissions page's orchestra switch. The next evaluation
    /// reads it, and a playing orchestra keeps sounding until then.
    pub fn set_csound(&mut self, allowed: bool) {
        self.config.csound_allowed = allowed;
        self.session.set_score_import_access(import_access(
            &self.config.session,
            &self.config.import_origins,
            allowed,
        ));
    }

    /// Preview RAM policy from the settings sheet. Zero budget means no
    /// ceiling; zero idle means unused samples are never dropped by time.
    pub fn set_sample_memory_policy(&mut self, budget_bytes: usize, idle: Duration) {
        self.preview_budget_bytes = budget_bytes;
        self.unused_sample_idle = idle;
        self.enforce_sample_memory(Instant::now());
    }

    /// What the performer can reach now, from the studio: see
    /// [`LiveMaterial`]. Its sounds are protected from the preview ceiling
    /// and the idle sweep from the next turn.
    pub fn set_live_material(&mut self, material: &LiveMaterial) {
        // Only a tab leaving asks the sweep again, not every text sent: a
        // text goes after each pause in typing, and a name half deleted
        // mid-edit would be swept and decoded again a moment later.
        if material.tabs_closed != self.live_tabs_closed {
            self.live_tabs_closed = material.tabs_closed;
            self.ask_idle_sweep();
            // The tab gone may be the one the score played from: once no
            // tab holds its text, what it sounded goes at Stop, or now.
            let mut open = material.pinned.iter().chain(&material.recent);
            if self
                .played_text
                .as_deref()
                .is_some_and(|played| !open.any(|text| **text == *played))
            {
                self.played_text = None;
                if self.live.is_none() {
                    self.played_by_text.clear();
                    self.played_by_score.clear();
                }
            }
        }
        #[cfg(feature = "vst")]
        {
            // A plugin the column shows no more goes with the next sweep.
            // A text edit asks for no sweep, as for the sounds.
            if material.plugins != self.live_plugins_shown {
                self.live_plugins_shown.clone_from(&material.plugins);
                self.plugins_swept = false;
            }
            let texts = || material.pinned.iter().chain(&material.recent);
            self.live_plugins_named = texts()
                .flat_map(|text| rustel_runtime::vst::names_in_source(text))
                .collect();
            self.live_plugin_strings = texts()
                .flat_map(|text| rustel_runtime::vst::strings_in_source(text))
                .collect();
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut pinned: Vec<(String, Variants)> = Vec::new();
        for text in &material.pinned {
            for (name, variants) in protected_names(text) {
                match seen.get(&name) {
                    Some(&at) => pinned[at].1.merge(&variants),
                    None => {
                        seen.insert(name.clone(), pinned.len());
                        pinned.push((name, variants));
                    }
                }
            }
        }
        let recent: Vec<Vec<(String, Variants)>> = material
            .recent
            .iter()
            .map(|text| protected_names(text))
            .collect();
        if pinned == self.live_pinned
            && recent == self.live_recent
            && material.setups_select_variants == self.live_setups_select_variants
        {
            return;
        }
        self.live_pinned = pinned;
        self.live_recent = recent;
        self.live_setups_select_variants = material.setups_select_variants;
        self.protection_generation = self.protection_generation.wrapping_add(1);
    }

    /// Whether a setup picks variants - one open in a tab, or one this
    /// session has applied - so that every text keeps and warms every
    /// variant of what it names: a setup's helper can set `n` for any
    /// score.
    pub(super) fn every_variant(&self) -> bool {
        self.live_setups_select_variants || self.session.prebake_selects_variants()
    }

    /// What protection is worked out against: the texts, an armed launch
    /// and whether a score plays, which change only when the performer
    /// does something; and the retained set and the
    /// library's settled epoch, which move with every sound that lands.
    fn protection_key(&self) -> ProtectionKey {
        let launch = self.pending_launch.as_ref().map_or(0, |pending| {
            pending.boundary_cycle.to_bits() ^ ((pending.source.len() as u64) << 1) | 1
        }) ^ self
            .waiting_source()
            .map_or(0, |source| (source.len() as u64) << 32);
        let playing = u64::from(self.live.is_some());
        let every_variant = u64::from(self.every_variant());
        // A font's zones reach the queue, and so the retained set, a moment
        // before the font reads Ready; the library's settled epoch moves
        // once it does, so what a tab names is looked for again then.
        let settled = self
            .session
            .sample_library()
            .map_or(0, |library| library.settled_epoch());
        ProtectionKey {
            texts: self.protection_generation
                ^ launch.rotate_left(17)
                ^ (playing << 62)
                ^ (every_variant << 61),
            landed: (self.retained_fingerprint(), settled),
            played: self.played_by_score.len(),
            in_use: self.sample_use_until.len(),
            allowance: self.recent_tabs_allowance(),
        }
    }

    /// Bring [`Self::protection`] up to date if it may be, and say whether
    /// it is: the ids no memory policy may drop - the bundled bd, what the
    /// score playing names, the always-kept tabs, and the recently visited
    /// tabs that fit (see [`LiveMaterial`]).
    ///
    /// Working it out asks the library for every name of every tab, and the
    /// ceiling asks on every turn once a set's own sounds pass it, so while
    /// sounds keep landing it is worked out again only every
    /// [`PROTECTION_REFRESH`], and reads stale until then.
    pub(super) fn refresh_protection(&mut self, now: Instant) -> bool {
        let key = self.protection_key();
        if let Some(protection) = &self.protection {
            if protection.key == key {
                return true;
            }
            if protection.key.texts == key.texts
                && protection.key.allowance == key.allowance
                && now.saturating_duration_since(protection.worked_out_at) < PROTECTION_REFRESH
            {
                return false;
            }
        }
        let (ids, recent) = self.work_out_protected_ids();
        self.protection = Some(Protection {
            key,
            ids,
            recent,
            worked_out_at: now,
        });
        true
    }

    /// Every protected id, and those among them kept only for a recently
    /// visited tab.
    fn work_out_protected_ids(
        &self,
    ) -> (
        std::collections::HashSet<SampleId>,
        std::collections::HashSet<SampleId>,
    ) {
        let mut wanted = std::collections::HashSet::new();
        let mut recent = std::collections::HashSet::new();
        wanted.insert(rustel_audio::BUNDLED_BD_SAMPLE_ID);
        let Some(library) = self.session.sample_library() else {
            return (wanted, recent);
        };
        let launch = self
            .pending_launch
            .as_ref()
            .map(|pending| pending.source.as_str())
            .into_iter()
            .chain(self.waiting_source())
            .flat_map(protected_names)
            .collect::<Vec<_>>();
        // A setup that picks variants can set `n` for any text: each name
        // then keeps every variant, whatever its own text says.
        let every_variant = self.every_variant();
        let variants = |sound| widened(sound, every_variant);
        // A launch counting down to its line: what it names must be there
        // when it lands, and a launch cancelled lets it go again.
        // The score is kept for being the score only while it sounds: what
        // its text names, and what its pattern resolved for the window
        // about to play, from the first turn on. Once stopped, its tab
        // keeps what it names if the tab is still open - the one last
        // heard is always kept - and a closed tab, the last one played
        // included, holds nothing.
        let score = self
            .live
            .is_some()
            .then(|| {
                self.current_score_names
                    .iter()
                    .chain(&self.score_window_names)
            })
            .into_iter()
            .flatten();
        let mut ready = library.ready_ids();
        wanted
            .extend(ready.of_variants(score.chain(&self.live_pinned).chain(&launch).map(variants)));
        // A font is forgotten whole, so an in-use zone keeps its siblings.
        wanted.extend(self.played_by_score.iter().copied());
        wanted.extend(library.whole_fonts_of(&self.played_by_score));
        let in_use = self.sample_use_until.keys().copied().collect();
        wanted.extend(library.whole_fonts_of(&in_use));
        wanted.extend(in_use);
        // The oldest visit goes first: each tab is kept whole or not at
        // all, and a tab that does not fit ends the list, so a big tab
        // visited long ago cannot outstay the smaller ones after it. A
        // sound a kept tab shares with it is kept by that tab.
        let allowance = self.recent_tabs_allowance();
        let mut kept_bytes = 0usize;
        for tab in &self.live_recent {
            // Only what the tab can play is its to keep, and only that is
            // counted against the allowance.
            let fresh: Vec<SampleId> = ready
                .of_variants(tab.iter().map(variants))
                .into_iter()
                .filter(|id| !wanted.contains(id))
                .collect();
            let bytes: usize = fresh
                .iter()
                .filter_map(|id| self.retained_samples.get(id))
                .map(DecodedSample::pcm_bytes)
                .sum();
            if kept_bytes.saturating_add(bytes) > allowance {
                break;
            }
            kept_bytes += bytes;
            recent.extend(fresh.iter().copied());
            wanted.extend(fresh);
        }
        (wanted, recent)
    }

    /// How much decoded sound the recently visited tabs may keep:
    /// [`RECENT_TABS_BYTES`], or the most one sound may hold when the
    /// performer has raised that past it. A tab that does not fit ends the
    /// list, so one sound at a raised "biggest sound" in the tab visited
    /// last left every recent tab unprotected.
    pub(super) fn recent_tabs_allowance(&self) -> usize {
        self.recent_tabs_bytes
            .unwrap_or_else(|| RECENT_TABS_BYTES.max(rustel_audio::sample_pcm_ceiling()))
    }

    /// Changes whenever an id is retained or let go, or a slot is refilled
    /// with a body of another size: what protection is worked out against.
    fn retained_fingerprint(&self) -> u64 {
        self.retained_samples.iter().fold(
            self.retained_samples.len() as u64,
            |hash, (id, sample)| {
                hash ^ ((u64::from(id.0) << 40) ^ sample.pcm_bytes() as u64)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            },
        )
    }

    /// Take an id out of the engine's hands and start it on its way back
    /// to the library: the bank is told to empty the slot, and the id waits
    /// in `retiring` until nothing can read it.
    pub(super) fn retire_sample(&mut self, id: SampleId, quiet_after_frame: u64) {
        if id == rustel_audio::BUNDLED_BD_SAMPLE_ID
            || self.retiring.iter().any(|retiring| retiring.id == id)
        {
            return;
        }
        // A retained sample can still occupy the bank or the install ring.
        let installed = self.retained_samples.remove(&id).is_some();
        self.sample_last_used.remove(&id);
        let Some(device) = self
            .live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref())
        else {
            // With no output, no bank, ring or voice holds the id.
            if let Some(library) = self.session.sample_library() {
                library.release_ids([id]);
            }
            return;
        };
        let baton = if !installed {
            Baton::NotOwed
        } else if device.uninstall_sample(id) {
            Baton::Pushed {
                callbacks: device.callbacks(),
            }
        } else {
            // The install ring is full: the baton is owed, not lost. The
            // bank keeps the PCM until the retry lands.
            Baton::Owed
        };
        device.reclaim_samples();
        self.retiring.push(RetiringSample {
            id,
            baton,
            quiet_after_frame,
        });
    }

    /// Retry the uninstalls the bank refused last turn. The callback drains
    /// the install ring between blocks, so one turn is headroom enough in
    /// practice; whatever is still refused waits for the next. A retiring
    /// id cannot have been re-retained in the meantime: it is in no table,
    /// and the ready queue refuses the tombstone the library left on it.
    pub(super) fn retry_pending_uninstalls(&mut self) {
        if !self
            .retiring
            .iter()
            .any(|retiring| retiring.baton == Baton::Owed)
        {
            return;
        }
        let Some(device) = self
            .live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref())
        else {
            return;
        };
        for retiring in &mut self.retiring {
            if retiring.baton == Baton::Owed && device.uninstall_sample(retiring.id) {
                retiring.baton = Baton::Pushed {
                    callbacks: device.callbacks(),
                };
            }
        }
        device.reclaim_samples();
    }

    /// Hand the library every retiring id nothing can read any more.
    ///
    /// Three things have to be true of an id: its baton landed and two
    /// more callbacks completed, so a whole block ran with the slot empty
    /// and every voice on it retired; the device's committed frontier is
    /// past the last frame any event queued before the forget - score or
    /// preview - could target; and nothing at or before that frame is
    /// still waiting in the producer's backlog.
    pub(super) fn release_retired_samples(&mut self) {
        if self.retiring.is_empty() {
            return;
        }
        let Some(device) = self
            .live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref())
        else {
            return;
        };
        let Some(library) = self.session.sample_library() else {
            return;
        };
        let callbacks = device.callbacks();
        let frame = device.clock_frames();
        device.reclaim_samples();
        let backlog_floor = self
            .live
            .as_ref()
            .and_then(|live| live.producer.pending_target_floor())
            .unwrap_or(u64::MAX);
        let audition_horizon = self.audition_horizon_frame;
        let mut released = Vec::new();
        self.retiring.retain(|retiring| {
            let cleared = match retiring.baton {
                Baton::NotOwed => true,
                Baton::Owed => false,
                Baton::Pushed { callbacks: pushed } => {
                    callbacks >= pushed.saturating_add(BATON_SETTLE_CALLBACKS)
                }
            };
            let quiet_after = retiring.quiet_after_frame.max(audition_horizon);
            let quiet = frame > quiet_after && backlog_floor > quiet_after;
            if cleared && quiet {
                released.push(retiring.id);
                false
            } else {
                true
            }
        });
        if !released.is_empty() {
            library.release_ids(released);
        }
    }

    /// The voices and producer backlog are gone. A parked bank still owes
    /// its uninstalls. A dropped or rebuilt bank releases every id at once.
    pub(super) fn release_every_retiring_sample(&mut self) {
        self.audition_horizon_frame = 0;
        if !self.sample_use_until.is_empty() {
            self.sample_use_until.clear();
            self.protection_generation = self.protection_generation.wrapping_add(1);
            self.idle_swept = false;
        }
        if self.retiring.is_empty() {
            return;
        }
        if self.piano_output.is_some() {
            for retiring in &mut self.retiring {
                retiring.quiet_after_frame = 0;
            }
            self.retry_pending_uninstalls();
            self.release_retired_samples();
            return;
        }
        let released: Vec<SampleId> = self
            .retiring
            .drain(..)
            .map(|retiring| retiring.id)
            .collect();
        if let Some(library) = self.session.sample_library() {
            library.release_ids(released);
        }
    }

    #[cfg(test)]
    pub(crate) fn retain_sample_for_test(&mut self, id: SampleId, sample: DecodedSample) {
        self.sample_last_used.insert(id, Instant::now());
        self.retained_samples.insert(id, sample);
    }

    #[cfg(test)]
    pub(crate) fn owes_uninstall(&self, id: SampleId) -> bool {
        self.retiring
            .iter()
            .any(|retiring| retiring.id == id && retiring.baton == Baton::Owed)
    }

    #[cfg(test)]
    pub(crate) fn retiring_count(&self) -> usize {
        self.retiring.len()
    }

    #[cfg(test)]
    pub(crate) fn retained_sample_count(&self) -> usize {
        self.retained_samples.len()
    }

    #[cfg(test)]
    pub(crate) fn force_sample_idle_for_test(&mut self, ago: Duration) {
        self.last_preview_at = Instant::now().checked_sub(ago).unwrap_or_else(Instant::now);
        self.idle_swept = false;
        // The same time has passed for what protection was worked out.
        if let Some(protection) = &mut self.protection {
            protection.worked_out_at = protection
                .worked_out_at
                .checked_sub(ago)
                .unwrap_or(protection.worked_out_at);
        }
        self.enforce_sample_memory(Instant::now());
    }

    /// A preview happened: the idle clock restarts, and what the last
    /// sweep decided is no longer the answer.
    pub(super) fn note_preview(&mut self) {
        self.last_preview_at = Instant::now();
        self.idle_swept = false;
    }

    /// A stop, a different score or a tab gone: the idle sweep decides
    /// again what is still wanted, for the sounds and for the plugins.
    pub(super) fn ask_idle_sweep(&mut self) {
        self.idle_swept = false;
        #[cfg(feature = "vst")]
        {
            self.plugins_swept = false;
        }
    }

    /// Unloads the plugins nothing asks for, one time for each quiet spell,
    /// with the idle sweep of the sounds: after the same idle time, and
    /// again after [`Self::ask_idle_sweep`]. The sweep keeps the plugins
    /// the reference column shows, and each plugin a text names: a text of
    /// a tab, the score in play, an armed launch or a held edit. A plugin
    /// call keeps its plugin by a part of the name too. Another string
    /// keeps a plugin by its whole name only: see
    /// [`rustel_runtime::vst::whole_names`]. Each chain of the score keeps
    /// its plugins too, also after a Stop: see
    /// [`rustel_runtime::Session::plugin_names`]. So a plugin from a setup
    /// text, an import or a name the score computes stays loaded.
    /// The host keeps each plugin with a running copy, so an unload waits
    /// for the output of its score to close. A stop parks the output with
    /// the copies of its score. The sweep closes a parked output that holds
    /// a copy of a plugin the sweep unloads, so the copies end first. In
    /// Piano mode the output stays open, and the sweep runs again when
    /// Piano mode closes.
    #[cfg(feature = "vst")]
    pub(super) fn unload_idle_plugins(&mut self, now: Instant) {
        if self.plugins_swept
            || self.unused_sample_idle.is_zero()
            || now.saturating_duration_since(self.last_preview_at) < self.unused_sample_idle
        {
            return;
        }
        self.plugins_swept = true;
        let score = self.live.is_some().then_some(self.current_score.as_str());
        let launch = self.pending_launch.as_ref().map(|pending| &*pending.source);
        let mut keep: Vec<String> = self
            .live_plugins_shown
            .iter()
            .chain(&self.live_plugins_named)
            .cloned()
            .collect();
        let mut strings = self.live_plugin_strings.clone();
        for source in score.into_iter().chain(launch).chain(self.waiting_source()) {
            keep.extend(rustel_runtime::vst::names_in_source(source));
            strings.extend(rustel_runtime::vst::strings_in_source(source));
        }
        // Another string keeps a plugin by its whole name only.
        keep.extend(rustel_runtime::vst::whole_names(&strings));
        keep.extend(self.session.plugin_names());
        // The close sends the copies to the plugin thread before the unload.
        // Piano mode, a key that rings out or a score tail keeps the output
        // open. Its copies keep their plugins loaded.
        if self.live.is_none()
            && self.piano_output.is_some()
            && !self.piano_mode
            && self.piano.iter().all(Option::is_none)
            && !self.piano_has_score_tail
            && rustel_runtime::vst::engine_holds_unkept(&keep)
        {
            self.close_piano_output();
        }
        rustel_runtime::vst::unload_unused(&keep);
    }

    pub(super) fn enforce_sample_memory(&mut self, now: Instant) {
        #[cfg(feature = "vst")]
        self.unload_idle_plugins(now);
        if let Some(live) = &self.live {
            let before = self.sample_use_until.len();
            let frame = live.device.clock_frames();
            self.sample_use_until.retain(|_, until| frame <= *until);
            if self.sample_use_until.len() != before {
                self.protection_generation = self.protection_generation.wrapping_add(1);
                self.idle_swept = false;
            }
        }
        // This runs every turn, so the cheap refusals come first: asking
        // the score what it names scans the source and locks the library's
        // tables, and none of that is owed while there is nothing to drop.
        if self.retained_samples.is_empty() {
            return;
        }
        let idle_elapsed = !self.idle_swept
            && !self.unused_sample_idle.is_zero()
            && now.saturating_duration_since(self.last_preview_at) >= self.unused_sample_idle;
        if !idle_elapsed && self.preview_budget_bytes == 0 {
            return;
        }
        // Extras are bounded by everything retained, so the score is only
        // asked for once the total can actually be over the ceiling.
        if !idle_elapsed {
            let total: usize = self
                .retained_samples
                .values()
                .map(DecodedSample::pcm_bytes)
                .sum();
            if total <= self.preview_budget_bytes {
                return;
            }
        }
        if !self.refresh_protection(now) {
            return;
        }
        let Some(protection) = &self.protection else {
            return;
        };
        // Backpressure can leave dynamic sample choices in the producer
        // before they are accepted by the device and gain a use horizon.
        let mut pending: std::collections::HashSet<_> = self
            .live
            .as_ref()
            .into_iter()
            .flat_map(|live| live.producer.pending_sample_ids())
            .collect();
        if let Some(library) = self.session.sample_library() {
            pending.extend(library.whole_fonts_of(&pending));
        }
        let wanted = |id: &SampleId| protection.ids.contains(id) || pending.contains(id);
        let mut doomed: std::collections::HashSet<SampleId> = std::collections::HashSet::new();
        if idle_elapsed {
            doomed.extend(
                self.retained_samples
                    .keys()
                    .copied()
                    .filter(|id| !wanted(id)),
            );
        }
        let mut spared = None;
        if self.preview_budget_bytes != 0 {
            let mut extras: Vec<(Instant, usize, SampleId)> = self
                .retained_samples
                .iter()
                .filter(|(id, _)| !wanted(id) && !doomed.contains(id))
                .map(|(id, sample)| {
                    let used = self
                        .sample_last_used
                        .get(id)
                        .copied()
                        .unwrap_or(self.last_preview_at);
                    (used, sample.pcm_bytes(), *id)
                })
                .collect();
            extras.sort_by_key(|(used, _, _)| *used);
            // The newest extra is the one the last preview added. It can be
            // in a pending audition: a preview does not sound a sample that
            // is not retained, so dropping it leaves the browser in a
            // Loading wait until the audition times out. Or it can be
            // sounding now, and the callback reads the bank by id when the
            // event fires. So the ceiling spares it, as it spares
            // score-named samples that exceed the ceiling by themselves.
            spared = extras.pop().map(|(_, _, id)| id);
            let mut extra_bytes: usize = extras.iter().map(|(_, bytes, _)| *bytes).sum();
            for (_, bytes, id) in extras {
                if extra_bytes <= self.preview_budget_bytes {
                    break;
                }
                extra_bytes = extra_bytes.saturating_sub(bytes);
                doomed.insert(id);
            }
        }
        if idle_elapsed {
            // One sweep per quiet spell: the next preview, a score that
            // names something else, a stop or a tab leaving is what asks
            // for another.
            self.idle_swept = pending.is_empty();
        }
        if doomed.is_empty() {
            return;
        }
        // The library publishes decoded PCM once, through the ready queue,
        // and afterwards remembers only the id. Dropping that PCM without
        // saying so leaves its tables promising a sample nobody holds: the
        // sound previews as "did not load in time", and a score that names
        // it plays silence, for the rest of the session. Forgetting first
        // is what makes the next ask decode it again - off the sample
        // cache on disk, so it costs no network.
        //
        // A font is forgotten whole, so a part-dropped one names its
        // untouched zones back here: they are no use on their own and the
        // ids about to be re-issued are not theirs.
        // Frozen before the library forgets: nothing scheduled after the
        // forget can name these ids, so this is the last frame anything
        // that can is due at.
        let quiet_after_frame = self
            .live
            .as_ref()
            .map_or(0, |live| live.producer.scheduled_through_frame());
        if let Some(library) = self.session.sample_library().cloned() {
            let orphaned = library.forget_decoded(&doomed);
            doomed.extend(orphaned);
        }
        doomed.retain(|id| Some(*id) != spared);
        if !doomed.is_empty() {
            self.owe_free_memory();
        }
        for id in doomed {
            self.retire_sample(id, quiet_after_frame);
        }
    }

    /// Something large was let go: hand its memory back once idle.
    ///
    /// The first debt sets the moment, and later ones do not move it, so a
    /// stop followed by a string of sample releases ends in one call rather
    /// than waiting for the releases to stop.
    pub(super) fn owe_free_memory(&mut self) {
        self.free_memory_due
            .get_or_insert_with(|| Instant::now() + FREE_MEMORY_DELAY);
    }

    /// Hand freed memory back to the system when a release is owed, or when
    /// the engine has idled long enough that something it did not see may
    /// have been let go. Idle turns only: with no playback open there is no
    /// producer for the allocator's locks to hold up.
    fn free_memory_when_due(&mut self, now: Instant) {
        let since_last = now.saturating_duration_since(self.last_free_memory);
        let owed =
            self.free_memory_due.is_some_and(|due| now >= due) && since_last >= FREE_MEMORY_SPACING;
        if !owed && since_last < FREE_MEMORY_INTERVAL {
            return;
        }
        rustel_runtime::free_memory::release_free_memory();
        self.last_free_memory = now;
        self.free_memory_due = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;

    #[test]
    fn csound_toggles_keep_current_import_permissions() {
        let revoked = "https://revoked.example";
        let granted = "https://granted.example";
        let blocked = "https://blocked.example";
        let mut engine = StudioEngine::new(StudioConfig {
            import_origins: vec![revoked.to_owned()],
            ..StudioConfig::default()
        })
        .expect("engine");
        engine.set_import_origins(&[granted.to_owned()]);

        for allowed in [false, true] {
            engine.set_csound(allowed);
            for (origin, refused) in [(revoked, revoked), (granted, blocked)] {
                // All origins are checked before fetching either script.
                let source = format!(
                    "await import('{origin}/helper.js'); await import('{blocked}/helper.js')"
                );
                let error = engine
                    .session
                    .evaluate_prebake(&source)
                    .expect_err("an origin remains blocked");
                assert!(
                    matches!(&error, RuntimeError::UntrustedImport { origin } if origin == refused),
                    "{error}"
                );
            }
        }
    }

    /// The page's grants reach the session and the pack imports alike, and
    /// a change replaces the last one. The record of refused sources stays
    /// the host's, so the studio reads it from the policy it gave.
    #[test]
    fn sample_grants_reach_the_session_and_the_pack_imports() {
        let mut host = SessionConfig::default();
        host.score_sample_access.permit_builtin_sources();
        let refused = host.score_sample_access.clone();
        let github = rustel_runtime::SourceRule::parse("raw.githubusercontent.com").unwrap();
        let mut engine = StudioEngine::new(StudioConfig {
            session: host,
            ..StudioConfig::default()
        })
        .expect("engine");

        engine.set_sample_grants(SampleGrants {
            sources: vec!["http://kit.invalid".to_owned()],
            ..SampleGrants::default()
        });
        let granted = engine.session.config().score_sample_access.clone();
        assert!(granted.sources().contains(&github), "the built-ins stay");
        assert_eq!(
            granted.source_for("http://kit.invalid/kit.json"),
            Ok(Some(
                &rustel_runtime::SourceRule::parse("http://kit.invalid").unwrap()
            ))
        );
        let library = engine.session.sample_library().expect("library");
        assert_eq!(library.import_policy_for_test(), Some(granted));

        // Plain http needs a source that spells it out. The score plays on
        // without the bank, and the refusal is a diagnostic.
        engine
            .session
            .evaluate("samples('http://other.invalid/kit.json')\n$: s(\"bd\")")
            .expect("the score");
        assert_eq!(refused.take_refused_sources(), ["http://other.invalid"]);
    }

    mod sample_reclamation {
        use super::*;

        fn tick_until_the_score_is_confirmed(engine: &mut StudioEngine) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.played_reset_pending
                || engine.session.confirmed_audio_generation() != Some(engine.session.generation())
            {
                assert!(Instant::now() < deadline, "the score never became audible");
                engine
                    .tick(|_| Ok(()))
                    .expect("confirm the score's cutover");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        /// Replacing a score lets its old sounds go after their last queued note
        /// finishes. A computed name in the replacement stays without stopping
        /// the transport, even though no text scan can find it.
        #[test]
        fn a_live_edit_releases_what_only_the_previous_text_sounded() {
            let mut engine = engine_with_five_sounds(0);
            let (kick, pad) = (SampleId(70), SampleId(71));
            let first = "s(\"kick\")";
            // Built in JavaScript, so only what it sounded keeps the pad.
            let second = "const name = 'p' + 'ad'; s(name)";
            engine.set_live_material(&LiveMaterial {
                recent: vec![Arc::from(first), Arc::from(second)],
                ..LiveMaterial::default()
            });
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine.evaluate(first, false).expect("the first text");
            tick_until_the_text_sounds(&mut engine, kick);
            tick_until_the_score_is_confirmed(&mut engine);
            engine.evaluate(second, false).expect("the second text");
            tick_until_the_text_sounds(&mut engine, pad);
            tick_until_the_score_is_confirmed(&mut engine);
            assert!(!engine.played_by_score.contains(&kick));
            assert!(!engine.played_by_text.contains(&kick));
            engine.set_live_material(&LiveMaterial {
                recent: vec![Arc::from(second)],
                tabs_closed: 1,
                ..LiveMaterial::default()
            });
            engine.force_sample_idle_for_test(Duration::from_secs(5));
            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.retained_samples.contains_key(&kick) {
                assert!(
                    Instant::now() < deadline,
                    "the previous score's kick stayed"
                );
                engine
                    .tick(|_| Ok(()))
                    .expect("keep the replacement playing");
                std::thread::sleep(Duration::from_millis(2));
            }

            assert!(engine.is_playing(), "reclamation needs no Stop");
            assert_eq!(retained_names(&engine), ["pad"]);
            assert_eq!(
                engine.played_by_score,
                std::collections::HashSet::from([pad])
            );
        }

        /// An old score's long sample remains available while its accepted notes
        /// can still read it. When they finish, the same idle spell gets another
        /// sweep without a preview, an edit or a Stop to trigger it.
        #[test]
        fn an_outgoing_samples_tail_rearms_the_idle_sweep_when_it_finishes() {
            let mut engine = engine_with_five_sounds(0);
            let kick = SampleId(70);
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine
                .evaluate("s(\"kick\").speed(0.01).slow(4)", false)
                .expect("the long sample");
            tick_until_the_text_sounds(&mut engine, kick);
            tick_until_the_score_is_confirmed(&mut engine);
            engine.evaluate("silence", false).expect("remove the sound");
            tick_until_the_score_is_confirmed(&mut engine);
            assert!(!engine.played_by_score.contains(&kick));
            let end = *engine
                .sample_use_until
                .get(&kick)
                .expect("accepted sample use survives the edit");
            assert!(
                engine.live.as_ref().unwrap().device.clock_frames() < end,
                "the slow sample still has a tail"
            );

            engine.force_sample_idle_for_test(Duration::from_secs(5));
            assert!(engine.retained_samples.contains_key(&kick));
            assert!(engine.idle_swept, "the first sweep kept the playing tail");
            let deadline = Instant::now() + Duration::from_secs(8);
            while engine.retained_samples.contains_key(&kick) {
                assert!(
                    Instant::now() < deadline,
                    "the completed tail stayed retained"
                );
                engine.tick(|_| Ok(())).expect("play through the tail");
                if engine.live.as_ref().unwrap().device.clock_frames() <= end {
                    assert!(
                        engine.retained_samples.contains_key(&kick),
                        "the bank must remain available through the accepted horizon"
                    );
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(engine.is_playing());
            assert!(!engine.sample_use_until.contains_key(&kick));
        }

        /// Repeated replacements do not accumulate the generations heard earlier
        /// in the run, including an outgoing generation shielded during reload.
        #[test]
        fn repeated_live_edits_keep_only_the_current_texts_played_set() {
            let mut engine = engine_with_five_sounds(0);
            engine.unused_sample_idle = Duration::ZERO;
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            for (name, id) in [("kick", 70), ("pad", 71), ("snare", 72), ("hat", 73)] {
                let source = format!("const name = '{name}'; s(name)");
                engine.evaluate(&source, false).expect("replacement");
                tick_until_the_text_sounds(&mut engine, SampleId(id));
                tick_until_the_score_is_confirmed(&mut engine);
                assert_eq!(
                    engine.played_by_score,
                    std::collections::HashSet::from([SampleId(id)]),
                    "only {name} belongs to the current text"
                );
            }
            assert!(engine.is_playing());
        }

        /// A sample published while the producer queries is already resolvable,
        /// though the host cannot install and retain it until the next turn.
        #[test]
        fn an_accepted_sample_gets_its_horizon_before_the_ready_queue_is_installed() {
            let library = rustel_runtime::samples::SampleLibrary::empty_without_loading();
            let sample = SampleId(70);
            let decoded = DecodedSample::from_parts(48_000, 1, vec![0.5; 48_000]).expect("pcm");
            library.publish_ready_for_test("https://samples.invalid/late.wav", sample, decoded);
            let event = AudioEvent {
                onset_id: 1,
                generation: 1,
                ui_visuals: 0,
                target_frame: 12_000,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 1.0,
                duration_secs: 0.1,
                controls: Default::default(),
                sample: Some(rustel_audio::SampleControls {
                    sample,
                    playback_rate: 0.5,
                    begin: 0.0,
                    end: 1.0,
                    hold: rustel_audio::SampleHold::Slice,
                    muted: false,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    reversed: false,
                    nudge_secs: 0.0,
                    cut: None,
                }),
                wavetable: None,
                synth: None,
                cut: None,
            };
            let mut use_until = HashMap::new();
            note_sample_use(
                &event,
                &HashMap::new(),
                Some(&library),
                &mut use_until,
                48_000,
                0,
            );
            assert!(
                use_until[&sample] > event.target_frame + 96_000,
                "the slow sample's full tail stays protected before installation"
            );
            let render_frontier = 48_000;
            note_sample_use(
                &event,
                &HashMap::new(),
                Some(&library),
                &mut use_until,
                48_000,
                render_frontier,
            );
            assert!(
                use_until[&sample] > render_frontier + 96_000,
                "a late onset still gets the full sample lifetime after adoption"
            );
            assert_eq!(
                library.take_ready().len(),
                1,
                "installation still owns the PCM"
            );
        }

        #[test]
        fn a_refused_live_edit_restores_the_audible_scores_dynamic_sample_pins() {
            let mut engine = engine_with_five_sounds(0);
            let kick = SampleId(70);
            let playing = "const name = 'ki' + 'ck'; s(name)";
            engine.set_live_material(&LiveMaterial {
                recent: vec![Arc::from("s('kick')")],
                ..LiveMaterial::default()
            });
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine.evaluate(playing, false).expect("the playing score");
            tick_until_the_text_sounds(&mut engine, kick);
            tick_until_the_score_is_confirmed(&mut engine);
            engine.set_live_material(&LiveMaterial::default());

            engine
                .evaluate("s(\"nosuchsound*4\")", false)
                .expect("the candidate evaluates before its first query refuses");
            assert!(engine.played_reset_pending);
            assert!(engine.played_by_text.is_empty());
            assert!(engine.played_by_score.contains(&kick));
            engine.force_sample_idle_for_test(Duration::from_secs(5));
            assert!(engine.retained_samples.contains_key(&kick));

            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.session.active_source() != Some(playing) {
                assert!(
                    Instant::now() < deadline,
                    "the refused score never rolled back"
                );
                engine.tick(|_| Ok(())).expect("refuse the candidate");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(engine.played_text.as_deref(), Some(playing));
            assert_eq!(engine.current_score, playing);
            assert!(engine.played_by_text.contains(&kick));
            tick_until_the_score_is_confirmed(&mut engine);
            engine.force_sample_idle_for_test(Duration::from_secs(5));
            assert!(engine.retained_samples.contains_key(&kick));
            assert!(engine.is_playing());

            assert!(engine.stop(Duration::from_millis(500)).is_some());
            assert!(engine.played_by_score.contains(&kick));
        }

        #[test]
        fn a_panicking_edit_restores_the_audible_scores_dynamic_sample_pins() {
            let mut engine = engine_with_five_sounds(0);
            let kick = SampleId(70);
            let playing = "const name = 'ki' + 'ck'; s(name)";
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine.evaluate(playing, false).expect("the playing score");
            tick_until_the_text_sounds(&mut engine, kick);
            tick_until_the_score_is_confirmed(&mut engine);
            hold_clock(&engine, true);
            engine
                .evaluate("const name = 'p' + 'ad'; s(name).fast(16)", false)
                .expect("the pending edit");
            assert!(engine.played_reset_pending);
            assert!(engine.played_by_score.contains(&kick));
            assert!(!engine.played_by_text.contains(&kick));
            engine
                .session
                .inject_panic_for_test(rustel_runtime::SessionPanicPoint::Query);

            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.session.active_source() != Some(playing) {
                assert!(
                    Instant::now() < deadline,
                    "the panic never restored the score"
                );
                engine.tick(|_| Ok(())).expect("contain the query panic");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(engine.current_score, playing);
            assert_eq!(engine.played_text.as_deref(), Some(playing));
            assert!(engine.played_by_text.contains(&kick));
            engine.force_sample_idle_for_test(Duration::from_secs(5));
            assert!(engine.retained_samples.contains_key(&kick));

            hold_clock(&engine, false);
            tick_until_the_score_is_confirmed(&mut engine);
            assert!(engine.stop(Duration::from_millis(500)).is_some());
            assert!(engine.played_by_score.contains(&kick));
        }

        #[test]
        fn closing_a_tab_before_its_edit_is_confirmed_does_not_restore_its_stop_pins() {
            let mut engine = engine_with_five_sounds(0);
            engine.set_live_material(&LiveMaterial {
                recent: vec![Arc::from("s('kick')"), Arc::from("s('pad')")],
                ..LiveMaterial::default()
            });
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine.evaluate("s(\"kick\")", false).expect("first score");
            tick_until_the_text_sounds(&mut engine, SampleId(70));
            tick_until_the_score_is_confirmed(&mut engine);
            hold_clock(&engine, true);
            engine
                .evaluate("const name = 'p' + 'ad'; s(name).fast(16)", false)
                .expect("replacement");
            tick_until_the_text_sounds(&mut engine, SampleId(71));
            assert!(engine.played_reset_pending);

            engine.set_live_material(&LiveMaterial {
                tabs_closed: 1,
                ..LiveMaterial::default()
            });
            assert!(engine.played_text.is_none());
            assert!(engine.played_by_text.contains(&SampleId(71)));
            hold_clock(&engine, false);
            tick_until_the_score_is_confirmed(&mut engine);
            assert!(
                engine.played_text.is_none(),
                "confirmation reopened the closed text"
            );
            assert_eq!(
                engine.played_by_score,
                std::collections::HashSet::from([SampleId(71)])
            );

            assert!(engine.stop(Duration::from_millis(500)).is_some());
            assert!(engine.played_by_score.is_empty());
            assert!(engine.played_by_text.is_empty());
        }
    }

    #[test]
    fn unused_preview_samples_are_dropped_when_over_budget_or_idle() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 2048,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let sample = || DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm");
        for index in 1..6u32 {
            engine.retain_sample_for_test(SampleId(index), sample());
        }
        assert_eq!(engine.retained_sample_count(), 5);
        engine.set_sample_memory_policy(2048, Duration::from_secs(1));
        assert_eq!(
            engine.retained_sample_count(),
            1,
            "budget 2 KiB drops the four oldest 4 KiB extras and spares the newest: a preview refuses to sound a sample that is no longer retained"
        );
        for index in 10..13u32 {
            engine.retain_sample_for_test(SampleId(index), sample());
        }
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(
            engine.retained_sample_count(),
            0,
            "idle drop clears samples the empty score does not name"
        );
    }

    /// Eviction hands the name back to the library. If it does not, the
    /// sound stays Ready with no PCM, and a score that names it plays
    /// silence for the rest of the session.
    #[test]
    fn an_evicted_sample_is_handed_back_to_the_library() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 0,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let library = engine
            .session
            .sample_library()
            .cloned()
            .expect("a session sample library");
        const URL: &str = "http://127.0.0.1:9/preview.wav";
        library.remember_ready_for_test(URL, SampleId(21));
        engine.retain_sample_for_test(
            SampleId(21),
            DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm"),
        );
        assert!(library.knows_ready_for_test(URL));

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(
            engine.retained_sample_count(),
            0,
            "the idle sweep dropped it"
        );
        assert!(
            !library.knows_ready_for_test(URL),
            "a sound whose PCM is gone must be unknown again, or nothing ever decodes it a second time"
        );
    }

    /// The idle sweep costs a source scan and two of the library's tables.
    /// Once the idle has elapsed it is owed once, not on every turn of the
    /// engine loop until something is previewed again.
    #[test]
    fn the_idle_sweep_is_owed_once_per_quiet_spell() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 0,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        engine.retain_sample_for_test(SampleId(31), pcm());
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(engine.retained_sample_count(), 0);
        assert!(engine.idle_swept);

        // A sample retained after the sweep stays: the quiet spell already
        // had its answer, and asking again is a source scan and two library
        // tables per turn of the engine loop.
        engine.retain_sample_for_test(SampleId(32), pcm());
        engine.enforce_sample_memory(Instant::now());
        assert_eq!(engine.retained_sample_count(), 1);

        engine.note_preview();
        assert!(!engine.idle_swept, "a preview restarts the quiet spell");
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(engine.retained_sample_count(), 0);
    }

    /// The worker runs the playing tick only while there is a device; a
    /// stopped studio takes idle turns. Those turns are where a stopped
    /// engine keeps its samples to the policy, or nothing ever does: what
    /// the loaders finish waits in the library's ready queue, and browsing
    /// the Examples while stopped decodes every sound a row names.
    #[test]
    fn decodes_that_land_while_stopped_are_held_to_the_preview_budget() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 8 * 1024,
            unused_sample_idle: Duration::from_secs(60),
            ..StudioConfig::default()
        })
        .expect("engine");
        assert!(!engine.is_playing());
        let library = engine
            .session
            .sample_library()
            .cloned()
            .expect("a session sample library");
        let url = |index: u32| format!("http://127.0.0.1:9/warmed-{index}.wav");
        // Six 4 KiB decodes land one after another, as rows are passed.
        for index in 0..6u32 {
            let pcm = DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm");
            library.publish_ready_for_test(&url(index), SampleId(50 + index), pcm);
            engine.idle_turn(|_| Ok(()));
            std::thread::sleep(Duration::from_millis(2));
        }
        // While sounds keep landing, what they are is worked out again at
        // most every PROTECTION_REFRESH; the ceiling holds from that pass.
        std::thread::sleep(PROTECTION_REFRESH);
        engine.idle_turn(|_| Ok(()));

        // 8 KiB of extras kept, plus the newest spared beyond them.
        assert_eq!(engine.retained_sample_count(), 3);
        assert!(
            !library.knows_ready_for_test(&url(0)),
            "the oldest is forgotten, so asking for it decodes it again"
        );
        assert!(library.knows_ready_for_test(&url(5)), "the newest stays");
        let waiting: Vec<SampleId> = library
            .peek_ready_unless(|_| false)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            waiting.len(),
            3,
            "what the budget dropped left the queue too, so no device installs it: {waiting:?}"
        );
        assert!(
            engine.free_memory_due.is_some(),
            "and what it dropped is owed back to the system"
        );
    }

    /// A tab behind a pad, in a pane or last heard keeps every sound it
    /// names, commented-out lanes included, through the sweep and the
    /// ceiling, whatever was evaluated last.
    #[test]
    fn an_always_kept_tab_keeps_its_sounds_whatever_was_evaluated_last() {
        let mut engine = engine_with_five_sounds(4 * 1024);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from(
                "$: s(\"kick\")\n// $: s(\"pad\")\n_$: s(\"hat\")",
            )],
            recent: Vec::new(),
            tabs_closed: 0,
            setups_select_variants: false,
            ..LiveMaterial::default()
        });
        engine
            .warm_source("silence", Duration::ZERO)
            .expect("warm score");

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(retained_names(&engine), ["kick", "pad", "hat"]);
    }

    /// Tabs neither pinned nor open are kept by their last visit, each
    /// whole, while their sounds fit: the one visited longest ago goes
    /// first, and a sound a kept tab shares with it stays with that tab.
    #[test]
    fn recent_tabs_are_kept_newest_first_within_their_size() {
        let mut engine = engine_with_five_sounds(0);
        engine.recent_tabs_bytes = Some(8 * 1024);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\")")],
            recent: vec![
                // Visited last: 4 KiB.
                Arc::from("s(\"snare\")"),
                // Then: the kick is the pinned tab's, so only the hat counts.
                Arc::from("s(\"kick hat\")"),
                // Longest ago, and over the size: let go.
                Arc::from("s(\"bell\")"),
            ],
            tabs_closed: 0,
            setups_select_variants: false,
            ..LiveMaterial::default()
        });

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(retained_names(&engine), ["kick", "snare", "hat"]);
    }

    /// Applying a setup warms what it names without taking the score's
    /// place: taken, the playing score's sounds were unwanted, and the
    /// next sweep unloaded them from under the performance.
    #[test]
    fn a_setup_does_not_take_the_scores_place() {
        let mut engine = engine_with_five_sounds(0);
        // The score is kept for being the score only while it plays.
        play_silently(&mut engine);
        engine
            .warm_source("$: s(\"kick pad\")", Duration::ZERO)
            .expect("warm score");
        engine
            .evaluate_prebake_guarded(
                "const setup = 1",
                &std::sync::atomic::AtomicBool::new(false),
            )
            .expect("setup");
        assert_eq!(engine.current_score, "$: s(\"kick pad\")");

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(retained_names(&engine), ["kick", "pad"]);
    }

    /// A sound the playing score builds in JavaScript is kept once the
    /// window its warm queried resolves it, though no text scan reads it.
    #[test]
    fn a_sound_the_score_builds_in_javascript_is_kept_while_it_plays() {
        let mut engine = engine_with_five_sounds(0);
        let source = "const name = 'sn' + 'are'; s(name)";
        engine.session.evaluate(source).expect("score");
        play_silently(&mut engine);
        engine
            .warm_source(source, Duration::from_secs(1))
            .expect("warm score");
        assert_eq!(
            engine.score_window_names,
            [("snare".to_owned(), Variants::first())]
        );

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(retained_names(&engine), ["snare"]);
    }

    /// An always-kept tab keeps a hyphenated name whole through a stopped
    /// studio's sweep: `mlkr-grsl:3` is one sound, not `mlkr` and `grsl`.
    #[test]
    fn an_always_kept_tab_keeps_a_hyphenated_sound() {
        let mut engine = engine_with_five_sounds(0);
        let library = engine.session.sample_library().cloned().expect("library");
        library
            .register_trusted_custom(
                r#"{"_base":"https://samples.invalid/","mlkr-grsl":["a.wav","b.wav","c.wav","d.wav"]}"#,
                None,
            )
            .expect("bank map");
        let drum = SampleId(90);
        library.remember_ready_for_test("https://samples.invalid/d.wav", drum);
        engine.retain_sample_for_test(
            drum,
            DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm"),
        );
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("$: s(\"mlkr-grsl:3\")")],
            recent: Vec::new(),
            tabs_closed: 0,
            setups_select_variants: false,
            ..LiveMaterial::default()
        });

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert!(engine.retained_samples.contains_key(&drum));
        assert!(retained_names(&engine).is_empty());
    }

    /// A tab that leaves re-arms the idle sweep, so its sounds go at the
    /// next idle turn and do not wait for a preview.
    #[test]
    fn closing_a_tab_rearms_the_idle_sweep() {
        let mut engine = engine_with_five_sounds(0);
        engine.set_live_material(&kick_kept_with(&["s(\"snare\")"], 0));
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(retained_names(&engine), ["kick", "snare"]);
        assert!(engine.idle_swept, "this quiet spell has had its sweep");

        engine.set_live_material(&kick_kept_with(&[], 1));
        engine.enforce_sample_memory(Instant::now());

        assert_eq!(retained_names(&engine), ["kick"], "the closed tab's snare");
    }

    /// Texts are sent again after every pause in typing. A name half
    /// deleted mid-edit is still the performer's: it must not be swept on
    /// the spot and decoded again once the word is finished.
    #[test]
    fn an_edit_that_drops_a_name_does_not_rearm_the_sweep() {
        let mut engine = engine_with_five_sounds(0);
        engine.set_live_material(&kick_kept_with(&["s(\"snare\")"], 0));
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(retained_names(&engine), ["kick", "snare"]);

        engine.set_live_material(&kick_kept_with(&["s(\"sn\")"], 0));
        engine.enforce_sample_memory(Instant::now());

        assert_eq!(retained_names(&engine), ["kick", "snare"]);
    }

    /// The score last evaluated is kept while it plays. Once stopped, only
    /// an open tab keeps its sounds; a closed tab holds nothing, even the
    /// last one played.
    #[test]
    fn a_stopped_score_is_not_kept_for_itself() {
        let mut engine = engine_with_five_sounds(0);
        play_silently(&mut engine);
        engine
            .warm_source("$: s(\"kick\")", Duration::ZERO)
            .expect("warm score");
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(retained_names(&engine), ["kick"], "kept while it plays");
        assert!(engine.idle_swept, "and the sweep has run");

        assert!(engine.stop(Duration::from_millis(500)).is_some());
        engine.idle_turn(|_| Ok(()));

        assert!(
            retained_names(&engine).is_empty(),
            "stopped, it is nobody's: {:?}",
            retained_names(&engine)
        );
    }

    /// What the sound costs is split the way the policy holds it: kept
    /// whatever its size, kept within the recent tabs' allowance, and held
    /// to the preview ceiling. A sound a pinned tab names is the pinned
    /// tab's, whatever recent tab names it too.
    #[test]
    fn sample_memory_splits_kept_recent_and_previews() {
        let mut engine = engine_with_five_sounds(0);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick pad\")")],
            recent: vec![Arc::from("s(\"kick snare hat\")")],
            tabs_closed: 0,
            setups_select_variants: false,
            ..LiveMaterial::default()
        });

        let memory = engine.snapshot().sample_memory;

        // 4 KiB apiece: kick and pad pinned, snare and hat recent, bell a
        // preview.
        assert_eq!(memory.live_bytes, 16 * 1024, "{memory:?}");
        assert_eq!(memory.recent_bytes, 8 * 1024, "{memory:?}");
        assert_eq!(memory.preview_bytes, 4 * 1024, "{memory:?}");
    }

    /// The recent tabs' allowance follows **biggest sound**, which the
    /// settings move without sending a text: the split is worked out
    /// again at once, so the breakdown never shows one limit against a split
    /// made under another.
    #[test]
    fn a_changed_recent_allowance_is_worked_out_at_once() {
        let mut engine = engine_with_five_sounds(0);
        engine.recent_tabs_bytes = Some(8 * 1024);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\")")],
            recent: vec![
                Arc::from("s(\"snare\")"),
                Arc::from("s(\"hat\")"),
                Arc::from("s(\"bell\")"),
            ],
            tabs_closed: 0,
            setups_select_variants: false,
            ..LiveMaterial::default()
        });
        let before = engine.snapshot().sample_memory;
        assert_eq!(before.recent_bytes, 8 * 1024, "{before:?}");

        engine.recent_tabs_bytes = Some(12 * 1024);
        let raised = engine.snapshot().sample_memory;
        assert_eq!(raised.recent_limit_bytes, 12 * 1024);
        assert_eq!(raised.recent_bytes, 12 * 1024, "the bell fits now");

        engine.recent_tabs_bytes = Some(4 * 1024);
        let lowered = engine.snapshot().sample_memory;
        assert_eq!(lowered.recent_limit_bytes, 4 * 1024);
        assert_eq!(lowered.recent_bytes, 4 * 1024, "only the snare fits");
    }

    /// Each part is shown against the limit holding it, so the limits come
    /// from the engine that applies them: a policy the settings sent and a
    /// full queue refused would otherwise read as applied. They are there
    /// before any sound is.
    #[test]
    fn snapshot_reports_the_limits_the_engine_holds() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");
        engine.recent_tabs_bytes = Some(12 * 1024);
        engine.set_sample_memory_policy(96 * 1024 * 1024, Duration::from_secs(45));

        let memory = engine.snapshot().sample_memory;

        assert_eq!(memory.preview_budget_bytes, 96 * 1024 * 1024);
        assert_eq!(memory.unused_idle, Duration::from_secs(45));
        assert_eq!(memory.recent_limit_bytes, 12 * 1024);
        assert_eq!(memory.live_bytes + memory.preview_bytes, 0);
    }

    /// The script engine's heap is a part of the studio no other figure
    /// covers, and it is there as soon as a session is.
    #[test]
    fn script_heap_is_reported() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");

        assert!(engine.snapshot().script_heap_bytes > 0);
    }

    /// An armed launch keeps what it names, and lets it go when it is
    /// cancelled. The keyboard plays oscillators and keeps no sound, even
    /// when a bank has the name of its instrument.
    #[test]
    fn an_armed_launch_keeps_its_sounds_and_the_keyboard_keeps_none() {
        let mut engine = engine_with_five_sounds(4 * 1024);
        engine.pending_launch = Some(PendingLaunch {
            source: "$: s(\"bell\")".to_owned(),
            mini: false,
            boundary_cycle: 4.0,
            boundary_time: 8.0,
            rewind: false,
            unit_cycles: 1.0,
            preview: false,
            followed: Vec::new(),
            waited: false,
            #[cfg(feature = "vst")]
            prepared: true,
        });
        assert!(
            protected_afresh(&mut engine).contains(&SampleId(74)),
            "bell"
        );
        engine.pending_launch = None;
        assert!(
            !protected_afresh(&mut engine).contains(&SampleId(74)),
            "a cancelled launch lets it go"
        );

        let library = engine.session.sample_library().cloned().expect("library");
        library
            .register_trusted_custom(
                r#"{"_base":"https://samples.invalid/","triangle":"triangle.wav"}"#,
                None,
            )
            .expect("bank map");
        library.remember_ready_for_test("https://samples.invalid/triangle.wav", SampleId(81));
        engine.retain_sample_for_test(
            SampleId(81),
            DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm"),
        );
        engine.set_piano_settings(crate::PianoSound::Triangle, 100);
        assert!(
            !protected_afresh(&mut engine).contains(&SampleId(81)),
            "the keyboard's triangle is an oscillator, not the bank"
        );
    }

    #[test]
    fn tabs_keep_only_the_takes_their_source_can_play() {
        let cases: &[(&str, &[u32])] = &[
            (r#"$: s("recordings:0")"#, &[0]),
            (r#"$: s("recordings").n("<0 1>")"#, &[0, 1]),
            (r#"$: s("recordings").n(irand(4))"#, &[0, 1, 2, 3]),
        ];
        for &(source, expected) in cases {
            let mut engine = engine_with_four_takes();
            engine.set_live_material(&pinned_tab(source));
            engine.force_sample_idle_for_test(Duration::from_secs(5));
            assert_eq!(kept_takes(&engine), expected, "{source}");
        }
    }

    /// A setup that picks variants can set `n` for any score, so every tab
    /// keeps every take: a setup open in a tab, and one already applied.
    #[test]
    fn a_setup_that_picks_variants_keeps_every_take() {
        let mut engine = engine_with_four_takes();
        engine.set_live_material(&LiveMaterial {
            setups_select_variants: true,
            ..pinned_tab("s(\"recordings\")")
        });
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(kept_takes(&engine), [0, 1, 2, 3], "open in a tab");

        let mut engine = engine_with_four_takes();
        engine.set_live_material(&pinned_tab("s(\"recordings\")"));
        engine
            .evaluate_prebake_guarded(
                "globalThis.shuffled = pat => pat.n(irand(4))",
                &std::sync::atomic::AtomicBool::new(false),
            )
            .expect("setup");
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(kept_takes(&engine), [0, 1, 2, 3], "applied");
    }

    /// A recently visited tab is charged for the takes it can play, not
    /// for its whole bank: one take fits an allowance four would not.
    #[test]
    fn a_recent_tab_is_charged_only_for_the_takes_it_can_play() {
        let mut engine = engine_with_four_takes();
        engine.recent_tabs_bytes = Some(4 * 1024);
        engine.set_live_material(&LiveMaterial {
            recent: vec![Arc::from("s(\"recordings:2\")")],
            ..LiveMaterial::default()
        });

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(kept_takes(&engine), [2]);
        assert_eq!(engine.snapshot().sample_memory.recent_bytes, 4 * 1024);
    }

    /// What the current score text sounded stays, including names that no
    /// scan finds (a take picked by code, a name built at runtime). It
    /// stays while the score plays and after Stop, until its text is
    /// replaced or its last tab leaves. A preview of the same sample adds
    /// nothing.
    #[test]
    fn what_the_score_sounded_is_kept_after_stop_until_its_tab_leaves() {
        let score = "s(\"piano\").gain(0.1)";
        let (mut engine, sample) = playing_piano(score);
        engine.audition("piano", 0.5).expect("a preview");
        assert!(
            engine.played_by_score.is_empty(),
            "a preview is not the score"
        );

        tick_until_the_text_sounds(&mut engine, sample);

        assert!(
            engine.played_by_score.contains(&sample),
            "{:?}",
            engine.played_by_score
        );
        assert!(
            protected_afresh(&mut engine).contains(&sample),
            "no text names it, and it is kept"
        );

        assert!(engine.stop(Duration::from_millis(500)).is_some());
        assert!(
            protected_afresh(&mut engine).contains(&sample),
            "kept after Stop"
        );
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from(score)],
            tabs_closed: 1,
            ..LiveMaterial::default()
        });
        assert!(
            protected_afresh(&mut engine).contains(&sample),
            "another tab left"
        );
        engine.set_live_material(&LiveMaterial {
            tabs_closed: 2,
            ..LiveMaterial::default()
        });
        assert!(engine.played_by_score.is_empty());
        assert!(!protected_afresh(&mut engine).contains(&sample));
    }

    /// The score's tab closed while it plays keeps what it sounded until
    /// Stop, and Stop lets it go.
    #[test]
    fn a_tab_closed_while_its_score_plays_keeps_its_sounds_until_stop() {
        let (mut engine, sample) = playing_piano("s(\"piano\").gain(0.1)");
        tick_until_the_text_sounds(&mut engine, sample);
        engine.set_live_material(&LiveMaterial {
            tabs_closed: 1,
            ..LiveMaterial::default()
        });
        assert!(protected_afresh(&mut engine).contains(&sample));

        assert!(engine.stop(Duration::from_millis(500)).is_some());

        assert!(engine.played_by_score.is_empty());
        assert!(!protected_afresh(&mut engine).contains(&sample));
    }

    /// A snippet previewed under the set sounds as the score while it
    /// plays, so nothing is dropped from under it; the score put back
    /// returns to what the set itself had sounded, and the preview playing
    /// on to its cutover adds nothing more.
    #[test]
    fn a_snippet_previewed_under_the_set_is_taken_back_out_with_it() {
        let score = "s(\"piano\").gain(0.1)";
        let (mut engine, sample) = playing_piano(score);
        tick_until_the_text_sounds(&mut engine, sample);
        assert!(engine.played_by_score.contains(&sample));

        engine.mark_next_install_preview(true);
        let previewed = engine.evaluate("s(\"piano\")", false).expect("the preview");
        // What the snippet sounds besides the set's own.
        let snippet = SampleId(999);
        engine.played_by_score.insert(snippet);
        assert!(
            protected_afresh(&mut engine).contains(&snippet),
            "kept while it plays"
        );

        let put_back = engine.evaluate(score, false).expect("the score put back");
        assert_eq!(
            engine.played_by_score,
            std::collections::HashSet::from([sample])
        );
        assert!(!protected_afresh(&mut engine).contains(&snippet));
        assert!(previewed.generation < put_back.generation);
        assert_eq!(
            engine.played_from_generation, put_back.generation,
            "the preview's own generation is no longer the score's"
        );

        // An armed launch carries the mark to its line, and Stop keeps the
        // set as it stood.
        engine.mark_next_install_preview(true);
        engine
            .arm_launch("s(\"piano\")", false, 1.0, false)
            .expect("arm");
        assert!(
            engine
                .pending_launch
                .as_ref()
                .is_some_and(|pending| pending.preview)
        );
        assert!(!engine.next_install_preview);
        assert!(engine.stop(Duration::from_millis(500)).is_some());
        assert!(engine.played_before_preview.is_none());
        assert_eq!(
            engine.played_by_score,
            std::collections::HashSet::from([sample])
        );
    }

    /// Stopped while a snippet previewed under the set plays, the studio
    /// keeps what the set sounded and lets the snippet's own sounds go.
    #[test]
    fn a_stop_during_a_snippet_preview_keeps_the_sets_sounds_alone() {
        let (mut engine, sample) = playing_piano("s(\"piano\").gain(0.1)");
        tick_until_the_text_sounds(&mut engine, sample);
        engine.mark_next_install_preview(true);
        engine.evaluate("s(\"piano\")", false).expect("the preview");
        engine.played_by_score.insert(SampleId(999));

        assert!(engine.stop(Duration::from_millis(500)).is_some());

        assert_eq!(
            engine.played_by_score,
            std::collections::HashSet::from([sample])
        );
    }

    /// Protection is worked out once per change, not per turn. A sound that
    /// lands after its tab was sent is kept: nothing is dropped before the
    /// next due pass catches up.
    #[test]
    fn protection_follows_sounds_that_land_after_their_tab() {
        let mut engine = engine_with_five_sounds(4 * 1024);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\").n(\"<0 1>\")")],
            recent: Vec::new(),
            tabs_closed: 0,
            setups_select_variants: false,
            ..LiveMaterial::default()
        });
        let start = Instant::now();
        assert!(engine.refresh_protection(start));
        let protected =
            |engine: &StudioEngine| engine.protection.as_ref().expect("worked out").ids.clone();
        assert!(protected(&engine).contains(&SampleId(70)));
        let generation = engine.protection_generation;
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\").n(\"<0 1>\")")],
            recent: Vec::new(),
            tabs_closed: 0,
            setups_select_variants: false,
            ..LiveMaterial::default()
        });
        assert_eq!(
            engine.protection_generation, generation,
            "the same material sent again changes nothing"
        );
        assert!(engine.refresh_protection(start), "and needs no new pass");

        // A take the tab plays lands afterwards, under a new id.
        let library = engine.session.sample_library().cloned().expect("library");
        library
            .register_trusted_custom(
                r#"{"_base":"https://samples.invalid/","kick":["kick.wav","kick2.wav"]}"#,
                None,
            )
            .expect("bank map");
        library.remember_ready_for_test("https://samples.invalid/kick2.wav", SampleId(80));
        engine.retain_sample_for_test(
            SampleId(80),
            DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm"),
        );

        let soon = start + Duration::from_millis(10);
        assert!(!engine.refresh_protection(soon), "not every turn");
        engine.enforce_sample_memory(soon);
        assert_eq!(
            engine.retained_sample_count(),
            6,
            "nothing is dropped on a stale answer, over the ceiling or not"
        );

        assert!(engine.refresh_protection(start + PROTECTION_REFRESH));
        assert!(protected(&engine).contains(&SampleId(80)));
    }

    /// The idle sweep is a stopped studio's, above all: the last preview's
    /// clock runs out while nothing plays.
    #[test]
    fn the_idle_sweep_runs_on_the_stopped_path() {
        for parked in [false, true] {
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some("silent".into()),
                preview_budget_bytes: 0,
                unused_sample_idle: Duration::from_secs(1),
                ..StudioConfig::default()
            })
            .expect("engine");
            if parked {
                play_silently(&mut engine);
            }
            let pcm = DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
            for id in [SampleId(61), SampleId(62)] {
                if let Some(device) = engine.audio_device() {
                    device.install_sample(id, pcm.clone()).expect("install");
                }
                engine.retain_sample_for_test(id, pcm.clone());
            }
            if parked {
                assert!(
                    engine
                        .audio_device()
                        .unwrap()
                        .hold_silent_clock_for_test(true)
                );
                assert!(!engine.stop(Duration::ZERO).unwrap().acknowledged);
                engine.last_preview_at = Instant::now() - Duration::from_secs(5);
                engine.idle_turn(|_| Ok(()));
                assert_eq!(
                    engine.retained_sample_count(),
                    2,
                    "the stop ramp still needs PCM"
                );
                assert!(
                    engine
                        .audio_device()
                        .unwrap()
                        .hold_silent_clock_for_test(false)
                );
                assert!(
                    engine
                        .stop(Duration::from_millis(500))
                        .unwrap()
                        .acknowledged
                );
                engine.last_preview_at = Instant::now();
            }
            engine.idle_turn(|_| Ok(()));
            assert_eq!(engine.retained_sample_count(), 2, "not idle long enough");
            assert!(pcm.pcm_owners_for_test() > 1, "the engine holds the PCM");

            engine.last_preview_at = Instant::now() - Duration::from_secs(5);
            engine.idle_turn(|_| Ok(()));
            assert_eq!(engine.retained_sample_count(), 0, "swept while stopped");
            let deadline = Instant::now() + Duration::from_secs(5);
            while pcm.pcm_owners_for_test() > 1 {
                assert!(Instant::now() < deadline, "the output kept swept PCM");
                engine.idle_turn(|_| Ok(()));
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(engine.piano_output.is_some(), parked);
        }
    }

    /// Memory the engine lets go returns to the system once the engine is
    /// idle: a moment after the release, not at once, and not again
    /// immediately for the next release. After a long idle, the rest of
    /// the released memory returns too.
    #[test]
    fn memory_let_go_is_handed_back_once_idle() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 0,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let start = Instant::now();
        engine.last_free_memory = start;
        assert_eq!(engine.free_memory_due, None, "nothing let go yet");

        let pcm = DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        engine.retain_sample_for_test(SampleId(41), pcm);
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(engine.retained_sample_count(), 0, "the sweep retired it");
        let due = engine
            .free_memory_due
            .expect("a retired sample owes a hand-back");

        engine.free_memory_when_due(due - Duration::from_millis(1));
        assert_eq!(engine.free_memory_due, Some(due), "not before its moment");
        let spaced = start + FREE_MEMORY_SPACING;
        if due < spaced {
            engine.free_memory_when_due(due);
            assert_eq!(
                engine.free_memory_due,
                Some(due),
                "not sooner than the spacing after the last hand-back"
            );
        }
        let paid = spaced.max(due);
        engine.free_memory_when_due(paid);
        assert_eq!(engine.free_memory_due, None, "paid");
        assert_eq!(engine.last_free_memory, paid);

        // Nothing owed, but idle for the interval: what the engine did not
        // see let go is handed back too.
        let quiet = paid + FREE_MEMORY_INTERVAL;
        engine.free_memory_when_due(quiet - Duration::from_millis(1));
        assert_eq!(engine.last_free_memory, paid);
        engine.free_memory_when_due(quiet);
        assert_eq!(engine.last_free_memory, quiet);
    }

    /// With nothing open there is no audio side to speak of. Once an
    /// output opens its DSP is there from the start, and its rings grow by
    /// a slot an event rather than all at once.
    #[test]
    fn an_open_output_reports_what_its_audio_holds() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            ..StudioConfig::default()
        })
        .expect("studio");
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(crate::PianoSound::Triangle, 130);
        assert_eq!(engine.snapshot().audio_memory, None);

        engine
            .session
            .set_pattern(rustel_core::silence())
            .expect("native pattern");
        play_silently(&mut engine);
        let opened = engine.snapshot().audio_memory.expect("an output is open");
        assert!(opened.backend > 0, "{opened:?}");
        assert!(opened.other > 0, "the master's analysis tap: {opened:?}");
        let analysis = engine.audio_device().expect("open").memory().analysis;
        let backlog = engine
            .live
            .as_ref()
            .map_or(0, |live| live.producer.backlog_bytes());
        assert_eq!(
            opened.other,
            analysis + backlog,
            "the taps and the backlog, not the engine's own trace table"
        );
        assert_eq!(
            (
                opened.event_ring,
                opened.input,
                opened.record,
                opened.reverbs
            ),
            (0, 0, 0, 0)
        );

        let slot = std::mem::size_of::<rustel_audio::QueuedAudioEvent>();
        engine.piano_note_on(0, 60, 100).unwrap();
        let one = engine.snapshot().audio_memory.expect("still open");
        assert_eq!(one.event_ring, slot, "one press, one slot");
        engine.piano_note_on(4, 64, 100).unwrap();
        let two = engine.snapshot().audio_memory.expect("still open");
        assert_eq!(two.event_ring, 2 * slot);
        assert_eq!(two.backend, opened.backend, "measured once, when prepared");
    }

    /// An evicted id goes back to the library only once its baton has
    /// landed and two more callbacks have completed - one whole block ran
    /// with the slot empty, so every voice on it retired.
    #[test]
    fn a_retiring_id_waits_for_two_callbacks_after_its_baton() {
        let mut engine = silent_engine_for_output_selection();
        let library = engine.sample_library().expect("library");
        let id = SampleId(600);
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        assert!(
            engine
                .live
                .as_ref()
                .unwrap()
                .device
                .install_sample(id, pcm())
                .is_ok(),
            "bank install"
        );
        engine.retain_sample_for_test(id, pcm());
        library.forget_id_for_test(id);

        engine.retire_sample(id, 0);
        assert_eq!(engine.retiring_count(), 1);
        assert_eq!(library.free_id_count(), 0, "not yet: the baton has to land");
        let pushed = engine
            .retiring
            .iter()
            .find_map(|retiring| match retiring.baton {
                Baton::Pushed { callbacks } => Some(callbacks),
                _ => None,
            })
            .expect("the baton was pushed");

        let device = &engine.live.as_ref().unwrap().device;
        settle_callbacks(device, pushed, BATON_SETTLE_CALLBACKS);
        engine.release_retired_samples();
        assert_eq!(engine.retiring_count(), 0, "settled and quiet: released");
        assert_eq!(library.free_id_count(), 1, "the id is the library's again");
    }

    /// A baton the ring refused is owed, and an owed id is never released:
    /// the bank still holds its PCM. The retry lands it, and only then does
    /// the settle start counting.
    #[test]
    fn an_owed_baton_is_never_released_until_it_lands() {
        for parked in [false, true] {
            let mut engine = silent_engine_for_output_selection();
            if parked {
                assert!(
                    engine
                        .stop(Duration::from_millis(500))
                        .unwrap()
                        .acknowledged
                );
            }
            let library = engine.sample_library().expect("library");
            let id = SampleId(601);
            let pcm = DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
            assert!(
                engine
                    .audio_device()
                    .unwrap()
                    .install_sample(id, pcm.clone())
                    .is_ok(),
                "bank install"
            );
            let device = engine.audio_device().unwrap();
            settle_callbacks(device, device.callbacks(), BATON_SETTLE_CALLBACKS);
            assert!(device.hold_silent_clock_for_test(true));
            library.forget_id_for_test(id);

            engine.retain_sample_for_test(id, pcm.clone());
            fill_install_ring_for_test(engine.audio_device().unwrap());
            engine.retire_sample(id, 0);
            assert!(
                engine.owes_uninstall(id),
                "the install ring refused the baton"
            );
            if parked {
                engine.release_every_retiring_sample();
            }

            engine.release_retired_samples();
            assert!(engine.owes_uninstall(id));
            assert_eq!(library.free_id_count(), 0);
            assert_eq!(pcm.pcm_owners_for_test(), 2, "the bank still holds the PCM");
            assert!(
                engine
                    .audio_device()
                    .unwrap()
                    .hold_silent_clock_for_test(false)
            );

            // The retry lands it once the ring has room, and the settle follows.
            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.owes_uninstall(id) {
                assert!(Instant::now() < deadline, "the retry never landed");
                engine.retry_pending_uninstalls();
                std::thread::sleep(Duration::from_millis(2));
            }
            let pushed = engine
                .retiring
                .iter()
                .find_map(|retiring| match retiring.baton {
                    Baton::Pushed { callbacks } => Some(callbacks),
                    _ => None,
                })
                .expect("pushed after the retry");
            settle_callbacks(
                engine.audio_device().unwrap(),
                pushed,
                BATON_SETTLE_CALLBACKS,
            );
            engine.release_retired_samples();
            assert_eq!(library.free_id_count(), 1);
            assert_eq!(pcm.pcm_owners_for_test(), 1, "the bank released the PCM");
        }
    }

    /// Stop clears queued use horizons, but the parked bank must process
    /// retiring ids before reuse. Retained ids remain available to resume.
    #[test]
    fn stop_releases_every_retiring_id_and_keeps_retained_ones() {
        let mut engine = silent_engine_for_output_selection();
        let library = engine.sample_library().expect("library");
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        let retiring = SampleId(602);
        let kept = SampleId(603);
        for id in [retiring, kept] {
            assert!(
                engine
                    .live
                    .as_ref()
                    .unwrap()
                    .device
                    .install_sample(id, pcm())
                    .is_ok(),
                "bank install"
            );
            engine.retain_sample_for_test(id, pcm());
        }
        library.forget_id_for_test(retiring);
        engine.retire_sample(retiring, u64::MAX);
        engine.audition_horizon_frame = u64::MAX;
        engine.release_retired_samples();
        assert_eq!(
            library.free_id_count(),
            0,
            "held by the horizon while playing"
        );

        assert!(engine.stop(Duration::from_millis(500)).is_some());
        let device = engine.piano_output.as_ref().expect("parked output");
        settle_callbacks(device, device.callbacks(), BATON_SETTLE_CALLBACKS);
        engine.idle_turn(|_| Ok(()));
        assert_eq!(engine.retiring_count(), 0);
        assert_eq!(library.free_id_count(), 1, "the retiring id is free");
        assert_eq!(
            engine.retained_sample_count(),
            1,
            "the retained one is kept"
        );
    }

    /// With no device there is nothing that can read an id: an eviction
    /// while stopped releases at once.
    #[test]
    fn an_idle_engine_releases_a_retiring_id_at_once() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            ..StudioConfig::default()
        })
        .expect("engine");
        assert!(
            engine.live.is_none(),
            "nothing plays until the first launch"
        );
        let library = engine.sample_library().expect("library");
        let id = SampleId(604);
        engine.retain_sample_for_test(
            id,
            DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm"),
        );
        library.forget_id_for_test(id);
        engine.retire_sample(id, 0);
        assert_eq!(engine.retiring_count(), 0);
        assert_eq!(library.free_id_count(), 1);
    }

    /// A preview run reaches seconds ahead, and the callback holds an event
    /// that far out in front of everything pushed after it: nothing retires
    /// until the frontier is past it.
    #[test]
    fn a_retiring_id_waits_for_the_audition_horizon() {
        let mut engine = silent_engine_for_output_selection();
        let library = engine.sample_library().expect("library");
        let id = SampleId(605);
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        assert!(
            engine
                .live
                .as_ref()
                .unwrap()
                .device
                .install_sample(id, pcm())
                .is_ok(),
            "bank install"
        );
        engine.retain_sample_for_test(id, pcm());
        library.forget_id_for_test(id);
        // A preview far ahead of the device's clock.
        let far = engine.live.as_ref().unwrap().device.clock_frames() + 48_000 * 60;
        engine.audition_horizon_frame = far;

        engine.retire_sample(id, 0);
        let pushed = engine
            .retiring
            .iter()
            .find_map(|retiring| match retiring.baton {
                Baton::Pushed { callbacks } => Some(callbacks),
                _ => None,
            })
            .expect("pushed");
        settle_callbacks(
            &engine.live.as_ref().unwrap().device,
            pushed,
            BATON_SETTLE_CALLBACKS,
        );
        engine.release_retired_samples();
        assert_eq!(
            library.free_id_count(),
            0,
            "settled, but a preview still names the slot a minute ahead"
        );
        engine.audition_horizon_frame = 0;
        engine.release_retired_samples();
        assert_eq!(library.free_id_count(), 1);
    }
}
