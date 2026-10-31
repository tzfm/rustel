use super::*;

impl LoadQueue {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(LoadQueueState::default()),
            wake: Condvar::new(),
        })
    }

    /// Queue a job. Returns `false` when the line is closed - the caller
    /// must not leave a claim that will never be worked.
    pub(super) fn push(&self, job: LoadJob, priority: LoadPriority) -> bool {
        let mut state = self.state.lock().expect("sample load queue");
        if state.closed {
            return false;
        }
        match priority {
            LoadPriority::Now => state.now.push_back(job),
            LoadPriority::Bet => state.bets.push_back(job),
        }
        self.wake.notify_one();
        true
    }

    /// A bet that turned out to be needed now moves up the line.
    pub(super) fn promote(&self, url: &str) {
        let mut state = self.state.lock().expect("sample load queue");
        if let Some(at) = state.bets.iter().position(|job| &*job.url == url)
            && let Some(job) = state.bets.remove(at)
        {
            state.now.push_back(job);
            self.wake.notify_one();
        }
    }

    fn take(state: &mut LoadQueueState) -> Option<LoadJob> {
        state.now.pop_front().or_else(|| state.bets.pop_back())
    }

    /// The next job, waiting for one; `None` once the line is closed and
    /// empty, which is the thread's cue to go.
    pub(super) fn pop(&self) -> Option<LoadJob> {
        let mut state = self.state.lock().expect("sample load queue");
        loop {
            if let Some(job) = Self::take(&mut state) {
                return Some(job);
            }
            if state.closed {
                return None;
            }
            state = self.wake.wait(state).expect("sample load queue");
        }
    }

    #[cfg(any(test, all(feature = "test-support", feature = "device-audio")))]
    pub(super) fn try_pop(&self) -> Option<LoadJob> {
        Self::take(&mut self.state.lock().expect("sample load queue"))
    }

    /// Nothing more will be queued.
    pub(super) fn close(&self) {
        let mut state = self.state.lock().expect("sample load queue");
        state.closed = true;
        self.wake.notify_all();
    }
}

impl Default for ReadySamples {
    fn default() -> Self {
        let mut identities = [DecodedIdentity::Unknown; SAMPLE_BANK_CAPACITY];
        identities[BUNDLED_BD_SAMPLE_ID.0 as usize] =
            DecodedIdentity::Current(BUNDLED_BD_SAMPLE_IDENTITY);
        Self {
            samples: Vec::new(),
            identities,
        }
    }
}

impl ReadySamples {
    fn identity(&self, id: SampleId) -> Option<u64> {
        match self.identities.get(id.0 as usize)? {
            DecodedIdentity::Current(identity) => Some(*identity),
            DecodedIdentity::Unknown | DecodedIdentity::Forgotten => None,
        }
    }

    fn accept_identity(&mut self, id: SampleId, identity: u64) -> bool {
        let Some(slot) = self.identities.get_mut(id.0 as usize) else {
            return false;
        };
        match *slot {
            DecodedIdentity::Forgotten => false,
            DecodedIdentity::Current(current) if current > identity => false,
            _ => {
                *slot = DecodedIdentity::Current(identity);
                true
            }
        }
    }

    pub(super) fn push(&mut self, (id, decoded): (SampleId, DecodedSample)) {
        // Decode identities increase monotonically; clones and recovery keep
        // theirs. An old retry must not replace a newer decode even after the
        // newer queue entry has already been taken for installation.
        if self.accept_identity(id, decoded.identity()) {
            self.samples.push((id, decoded));
        }
    }
}

/// Hand back an id whose decode never reached anyone: reserved for a url
/// that then failed to fetch or decode. Its table said Loading throughout,
/// so no resolution, event, or bank ever saw it, and its identity slot is
/// still unknown; the retry after the rest reserves afresh.
fn release_unpublished_id(shared: &Shared, id: SampleId) {
    shared
        .free_ids
        .lock()
        .expect("free sample ids")
        .push_back(id);
}

/// Start `url` loading unless it is already on its way, in, or freshly
/// failed. `priority` says where it joins the line, and whether a failure
/// is worth another try: a real ask after the rest is; a bet is not.
pub(super) fn ensure_loading_shared(
    shared: &Shared,
    url: &Arc<str>,
    decode_rate: DecodeRate,
    priority: LoadPriority,
) -> SampleResolution {
    let settled = |state: Option<&UrlState>| match state {
        Some(UrlState::Ready { id, duration_secs }) => Some(SampleResolution::Found {
            id: *id,
            transpose: 0.0,
            duration_secs: *duration_secs,
            loop_secs: None,
            envelope_peak: 1.0,
            soundfont: false,
        }),
        Some(UrlState::Loading) => Some(SampleResolution::Loading),
        Some(UrlState::Failed { at })
            if priority == LoadPriority::Bet || at.elapsed() < FAILED_RETRY_AFTER =>
        {
            Some(SampleResolution::Failed)
        }
        Some(UrlState::Failed { .. }) | None => None,
    };
    {
        let by_url = shared.by_url.read().expect("sample url table");
        if let Some(resolution) = settled(by_url.get(url)) {
            // Bet on earlier, needed now: it moves up the line.
            if priority == LoadPriority::Now && matches!(resolution, SampleResolution::Loading) {
                shared.jobs.promote(url);
            }
            return resolution;
        }
    }
    let mut by_url = shared.by_url.write().expect("sample url table");
    if let Some(resolution) = settled(by_url.get(url)) {
        return resolution;
    }
    let ids = match reserve_sample_ids(shared, 1) {
        Ok(ids) => ids,
        Err(error) => {
            // A full bank is a failure like any other: it is reported once,
            // then rests, and the next real ask after the rest tries again.
            // By then the studio may have released a preview's id.
            by_url.insert(url.clone(), UrlState::Failed { at: Instant::now() });
            drop(by_url);
            shared
                .failures
                .lock()
                .expect("sample failures")
                .push(format!("{url}: {error}").into());
            return SampleResolution::Failed;
        }
    };
    let id = ids[0];
    by_url.insert(url.clone(), UrlState::Loading);
    drop(by_url);
    shared.jobs.push(
        LoadJob {
            url: url.clone(),
            kind: LoadKind::Install { id, decode_rate },
        },
        priority,
    );
    SampleResolution::Loading
}

/// Whether this URL already has a host-cache file, or is a local path that
/// is its own cache.
///
/// An empty file does not count: a failed or interrupted publish can leave
/// a zero-byte entry that would otherwise poison every later ask into
/// "already here", and a pack's row would never finish the last file.
fn host_cache_holds(dir: &Path, url: &str) -> bool {
    cache_entry_bytes(dir, url).is_some_and(|bytes| bytes > 0)
}

/// How many bytes a host-cache entry holds for `url`, when there is a
/// non-empty regular file. Local `file://` paths are already on disk and
/// report their own length.
pub(super) fn cache_entry_bytes(dir: &Path, url: &str) -> Option<u64> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::metadata(path).ok().map(|meta| meta.len());
    }
    let file = open_regular_cache_entry(&cache_path(dir, url))
        .ok()
        .flatten()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        // A zero-byte publish is not a sample: drop it so the next ask
        // fetches for real rather than treating emptiness as success.
        evict_host_cache_entry(dir, url);
        return None;
    }
    Some(len)
}

/// One soundfont-loader worker: a refused font does not stop later jobs.
pub(super) fn run_font_loader(
    font_shared: Weak<Shared>,
    font_jobs: Arc<FontQueue>,
    font_dir: PathBuf,
    font_base: String,
) {
    while let Some(font) = font_jobs.pop() {
        let Some(font_shared) = font_shared.upgrade() else {
            return;
        };
        let url = format!("{font_base}/{font}.js");
        let budget = sample_fetch::FetchBudget::for_one_fetch_with_cancellation(
            font_shared.publication.cancellation(),
        );
        let outcome = load_font(&font_dir, &url, &font_shared, &budget);
        let mut fonts = font_shared.fonts.write().expect("font table");
        match outcome {
            Ok(zones) => {
                fonts.insert(font.clone(), FontState::Ready(Arc::new(zones)));
                drop(fonts);
                font_shared.note_settled();
            }
            Err(error) => {
                fonts.insert(font.clone(), FontState::Failed { at: Instant::now() });
                drop(fonts);
                font_shared.note_settled();
                font_shared
                    .failures
                    .lock()
                    .expect("sample failures")
                    .push(format!("{url}: {error}").into());
            }
        }
    }
}

/// One sample-loader worker: pops jobs until the library closes the line.
fn run_sample_loader(shared: Weak<Shared>, jobs: Arc<LoadQueue>, dir: PathBuf) {
    while let Some(LoadJob { url, kind }) = jobs.pop() {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        shared
            .loading
            .lock()
            .expect("loading sample")
            .insert(Arc::clone(&url));
        let budget = sample_fetch::FetchBudget::for_one_fetch_with_cancellation(
            shared.publication.cancellation(),
        );
        let LoadKind::Install { id, decode_rate } = kind else {
            let outcome = fetch_cached_with_budget(&dir, &url, &budget, &shared.publication);
            shared
                .caching
                .lock()
                .expect("disk-cache claims")
                .remove(&url);
            if let Err(error) = outcome {
                shared
                    .failures
                    .lock()
                    .expect("sample failures")
                    .push(format!("{url}: {error}").into());
            }
            shared.loading.lock().expect("loading sample").remove(&url);
            continue;
        };
        let score_access = shared
            .score_sources
            .read()
            .expect("score sample sources")
            .get(&url)
            .cloned();
        let context_rate = match decode_rate {
            DecodeRate::Context => Some(shared.render_rate.load(Ordering::Acquire)),
            DecodeRate::Native => None,
        };
        let outcome = match score_access {
            // Decoding is the validation: the score cache commits only bytes
            // that decode, and the host cache evicts bytes that do not, so
            // neither serves an undecodable entry again.
            Some(access) => fetch_score_source_cached_with_budget(
                &shared.score_cache,
                &url,
                &access,
                ScoreCacheKind::Audio,
                rustel_audio::sample_pcm_ceiling(),
                CacheFetchScope {
                    budget: &budget,
                    publication: &shared.publication,
                },
                |bytes| decode_guarded(&url, bytes, context_rate),
            ),
            None => fetch_cached_decoded_with_budget(
                &dir,
                &url,
                &budget,
                &shared.publication,
                |bytes| decode_guarded(&url, bytes, context_rate),
            ),
        };
        match outcome {
            Ok(decoded) => {
                let duration = decoded.frames() as f64 / f64::from(decoded.sample_rate());
                // The only moment the PCM is in hand: from here the body
                // goes to the device and is not kept.
                shared
                    .shapes
                    .lock()
                    .expect("sample shapes")
                    .remember(url.clone(), shape_of(&decoded));
                // Queue the PCM first: `wait_until_idle` treats a Ready url
                // as settled, so publish Ready only after the decoded
                // sample reaches the install queue.
                shared
                    .ready
                    .lock()
                    .expect("ready samples")
                    .push((id, decoded));
                shared.by_url.write().expect("sample url table").insert(
                    url.clone(),
                    UrlState::Ready {
                        id,
                        duration_secs: duration,
                    },
                );
                shared.note_settled();
            }
            Err(error) => {
                let mut by_url = shared.by_url.write().expect("sample url table");
                by_url.insert(url.clone(), UrlState::Failed { at: Instant::now() });
                drop(by_url);
                shared.note_settled();
                // Nothing ever saw this id: the table said Loading
                // throughout. The retry after the rest reserves afresh.
                release_unpublished_id(&shared, id);
                shared
                    .failures
                    .lock()
                    .expect("sample failures")
                    .push(format!("{url}: {error}").into());
            }
        }
        shared.loading.lock().expect("loading sample").remove(&url);
    }
}

/// Queue a fetch that writes the host cache and then forgets the bytes.
/// No live-bank id, no decode, no Ready row: a later play loads from disk.
///
/// `true` when a fetch was put in the line; `false` when the file is on
/// disk already, being loaded for a note, or claimed by an earlier pass -
/// which is what lets a pack's row say how much of it was already here.
pub(super) fn ensure_cached_shared(shared: &Shared, url: &Arc<str>) -> bool {
    if host_cache_holds(&shared.host_cache, url) {
        return false;
    }
    {
        let by_url = shared.by_url.read().expect("sample url table");
        if matches!(
            by_url.get(url),
            Some(UrlState::Loading | UrlState::Ready { .. })
        ) {
            return false;
        }
    }
    {
        let mut caching = shared.caching.lock().expect("disk-cache claims");
        if !caching.insert(Arc::clone(url)) {
            return false;
        }
    }
    // Claim first, then push. If the queue is closed because the library is
    // shutting down, drop the claim, so a pack's row does not count a fetch
    // that never runs.
    if !shared.jobs.push(
        LoadJob {
            url: Arc::clone(url),
            kind: LoadKind::Cache,
        },
        LoadPriority::Bet,
    ) {
        shared
            .caching
            .lock()
            .expect("disk-cache claims")
            .remove(url);
        return false;
    }
    true
}

/// Cache a soundfont file the way [`ensure_cached_shared`] caches a bank
/// sample: the `.js` lands on disk, and none of its zones take a bank slot.
pub(super) fn queue_font_cache(shared: &Shared, font: &Arc<str>) -> bool {
    if shared.font_base.is_empty() {
        return false;
    }
    {
        let table = shared.fonts.read().expect("font table");
        if matches!(
            table.get(font),
            Some(FontState::Loading | FontState::Ready(_))
        ) {
            return false;
        }
    }
    let url: Arc<str> = Arc::from(font_file_url(&shared.font_base, font));
    ensure_cached_shared(shared, &url)
}

/// The file a General MIDI font is fetched from: `{base}/{font}.js`.
pub(super) fn font_file_url(font_base: &str, font: &str) -> String {
    format!("{font_base}/{font}.js")
}

/// Refuse the first URL of a bank that the fetch boundary,
/// [`sample_fetch::remote_url_allowed`], refuses, with the boundary's own
/// reason word for word, so the refusal says what was wrong.
/// Locally-scanned folders never pass through here; they are registered
/// directly with `file://` URLs under the operator's own choice of root.
pub(super) fn check_bank_urls(bank: &Bank) -> Result<(), String> {
    match bank {
        Bank::Array(urls) => urls
            .iter()
            .try_for_each(|url| sample_fetch::remote_url_allowed(url)),
        Bank::Notes(notes) => notes
            .iter()
            .flat_map(|(_, urls)| urls)
            .try_for_each(|url| sample_fetch::remote_url_allowed(url)),
    }
}

impl SampleLibrary {
    /// Load and verify every pinned default manifest before returning.
    ///
    /// A successful return means the default bank map is ready. Network,
    /// deadline, pin, and parse failures are returned to the caller. Session's
    /// live producer uses a separate crate-private asynchronous constructor so
    /// it never waits for manifest I/O.
    pub fn load_default() -> Result<Self, String> {
        Self::load_default_from(PINNED_BANKS, PINNED_GM_FONTS, false, cache_dir())
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn load_default_with_manifest_cache_for_tests(
        manifest_cache_dir: PathBuf,
    ) -> Result<Self, String> {
        Self::load_default_from(PINNED_BANKS, PINNED_GM_FONTS, false, manifest_cache_dir)
    }

    /// Build the inline default library immediately and load its pinned
    /// manifests on the bounded worker used by Session.
    pub(crate) fn load_default_async_for_session() -> Result<Self, String> {
        Self::load_default_from(PINNED_BANKS, PINNED_GM_FONTS, true, cache_dir())
    }

    pub(super) fn load_default_from(
        pinned_banks: &str,
        pinned_fonts: &str,
        asynchronous: bool,
        manifest_cache_dir: PathBuf,
    ) -> Result<Self, String> {
        let pinned: PinnedFile = serde_json::from_str(pinned_banks)
            .map_err(|error| format!("pinned sample-banks.json: {error}"))?;
        let mut banks = inline_banks_from(&pinned);
        if asynchronous {
            seed_cached_default_manifests(&mut banks, &pinned.sources, &manifest_cache_dir);
        }

        let pinned_fonts: PinnedFonts = serde_json::from_str(pinned_fonts)
            .map_err(|error| format!("pinned gm-fonts.json: {error}"))?;
        let gm: HashMap<String, Vec<Arc<str>>> = pinned_fonts
            .fonts
            .into_iter()
            .map(|(name, fonts)| {
                (
                    name,
                    fonts
                        .into_iter()
                        .map(|font| Arc::from(font.as_str()))
                        .collect(),
                )
            })
            .collect();
        let font_base = pinned_fonts.base;

        let library = Self::with_background_loaders(banks, Vec::new(), gm, font_base)?;
        let work = ManifestWork::Defaults {
            sources: pinned.sources,
            cache_dir: manifest_cache_dir,
        };
        if asynchronous {
            library.enqueue_manifest_work_async(work)?;
        } else {
            library.enqueue_manifest_work_blocking(work)?;
        }
        Ok(library)
    }

    /// Assemble a library around the background loader threads. Split out of
    /// [`load_default`] so tests can build an EMPTY library (no pinned data,
    /// no network) and still exercise registration through the real loaders.
    pub(super) fn with_background_loaders(
        banks: HashMap<String, Bank>,
        aliases: Vec<(String, String)>,
        gm: HashMap<String, Vec<Arc<str>>>,
        font_base: String,
    ) -> Result<Self, String> {
        Self::with_background_loaders_at(
            banks,
            aliases,
            gm,
            font_base,
            cache_dir(),
            Loading::InBackground,
        )
    }

    pub(super) fn with_background_loaders_at(
        mut banks: HashMap<String, Bank>,
        aliases: Vec<(String, String)>,
        gm: HashMap<String, Vec<Arc<str>>>,
        font_base: String,
        dir: PathBuf,
        loading: Loading,
    ) -> Result<Self, String> {
        let jobs = LoadQueue::new();
        let font_jobs = FontQueue::new();
        let (manifest_queue, manifest_jobs) = manifest_queue::manifest_queue();
        let publication = Arc::new(PublicationGate::new());
        let shared = Arc::new(Shared {
            by_url: RwLock::new(HashMap::new()),
            speech: Mutex::new(HashMap::new()),
            score_sources: RwLock::new(HashMap::new()),
            ready: Mutex::new(ReadySamples::default()),
            shapes: Mutex::new(SampleShapes::default()),
            source_tables: Mutex::new(SourceTables::default()),
            bank_imports: RwLock::new(HashMap::new()),
            set_banks: RwLock::new(BTreeMap::new()),
            custom_file_names: RwLock::new(HashMap::new()),
            global_file_names: RwLock::new(HashMap::new()),
            // Id 0 is the bundled bd; the library allocates from 1.
            next_id: AtomicU32::new(1),
            free_ids: Mutex::new(VecDeque::new()),
            jobs: Arc::clone(&jobs),
            loading: Mutex::new(HashSet::new()),
            caching: Mutex::new(HashSet::new()),
            host_cache: dir.clone(),
            font_base: font_base.clone(),
            fonts: RwLock::new(HashMap::new()),
            renames: RwLock::new(HashMap::new()),
            source_renames: RwLock::new(HashMap::new()),
            auto_aliases: RwLock::new(HashMap::new()),
            global_slots: Mutex::new(Vec::new()),
            global_generation: std::sync::atomic::AtomicU64::new(0),
            global_source_of: RwLock::new(HashMap::new()),
            import_policy: Mutex::new(None),
            font_jobs: Arc::clone(&font_jobs),
            failures: Mutex::new(Vec::new()),
            direct_diagnostic_logging: AtomicBool::new(
                rustel_voice::default_direct_diagnostic_logging(),
            ),
            #[cfg(feature = "video")]
            videos: Mutex::new(HashMap::new()),
            score_cache: ScoreCache::new(dir.clone()),
            manifest_pending: AtomicUsize::new(0),
            publication,
            render_rate: AtomicU32::new(crate::session::DEFAULT_SAMPLE_RATE),
            settled_epoch: AtomicU64::new(0),
        });
        // A worker must not keep `Shared` alive: worker -> Shared -> queue
        // would otherwise form a cycle and leak one thread per discarded
        // library. It holds the queue alone, upgrades only while handling a
        // job, and goes when dropping `Shared` closes the line.
        // A test that reads the job queue is racing these threads for the
        // same job: a loader pops one to work on it, so whether the
        // assertion sees it depends on which thread got there first.
        if loading == Loading::InBackground {
            for index in 0..SAMPLE_LOAD_WORKERS {
                let worker_shared = Arc::downgrade(&shared);
                let jobs = Arc::clone(&jobs);
                let dir = dir.clone();
                std::thread::Builder::new()
                    .name(format!("sample-loader-{index}"))
                    .spawn(move || run_sample_loader(worker_shared, jobs, dir))
                    .map_err(|error| format!("spawn sample loader: {error}"))?;
            }
        }

        let font_shared = Arc::downgrade(&shared);
        let font_dir = cache_dir();
        std::thread::Builder::new()
            .name("soundfont-loader".into())
            .spawn(move || run_font_loader(font_shared, font_jobs, font_dir, font_base))
            .map_err(|error| format!("spawn soundfont loader: {error}"))?;

        expand_bank_aliases(&mut banks, &aliases);
        expand_case_insensitive(&mut banks);

        let banks = Arc::new(RwLock::new(banks));
        let custom = Arc::new(RwLock::new(HashMap::new()));
        let global = Arc::new(RwLock::new(HashMap::new()));
        let gm = Arc::new(gm);
        let manifest_context = ManifestContext {
            banks: Arc::clone(&banks),
            custom: Arc::clone(&custom),
            global: Arc::clone(&global),
            gm: Arc::clone(&gm),
            shared: Arc::clone(&shared),
        };
        std::thread::Builder::new()
            .name("sample-manifest-loader".into())
            .spawn(move || run_manifest_worker(manifest_context, manifest_jobs))
            .map_err(|error| format!("spawn sample manifest loader: {error}"))?;

        Ok(Self {
            banks,
            custom,
            global,
            gm,
            shared,
            manifest_queue,
            manifest_order: Mutex::new(()),
            bus_names: rustel_voice::BusNames::default(),
        })
    }

    /// Font-zone lookup for gm_* names: variant by `n`, zone by midi range,
    /// exactly `registerSoundfonts`/`findZone`.
    pub(super) fn resolve_soundfont(
        &self,
        fonts: &[Arc<str>],
        n: f64,
        midi: f64,
        priority: LoadPriority,
    ) -> SampleResolution {
        // An empty font list cannot resolve a sound.
        let Some(font) = fonts.get(sound_index(n, fonts.len())) else {
            return SampleResolution::Failed;
        };
        // A failure rests, then a real ask tries again; a bet does not. A
        // font that failed because the bank was full is the case that
        // matters: by the time the rest is over the studio may have handed
        // ids back, and a font marked failed for good would never find out.
        let rested = |state: Option<&FontState>| match state {
            None => true,
            Some(FontState::Failed { at }) => {
                priority == LoadPriority::Now && at.elapsed() >= FAILED_RETRY_AFTER
            }
            Some(FontState::Loading | FontState::Ready(_)) => false,
        };
        {
            let table = self.shared.fonts.read().expect("font table");
            match table.get(font) {
                Some(FontState::Ready(zones)) => {
                    // `zone.keyRangeLow <= pitch && zone.keyRangeHigh + 1 >= pitch`
                    let Some(zone) = zones
                        .iter()
                        .find(|zone| zone.key_lo <= midi && zone.key_hi + 1.0 >= midi)
                    else {
                        return SampleResolution::Failed;
                    };
                    return SampleResolution::Found {
                        id: zone.id,
                        // playbackRate = 2^((100·midi − baseDetune)/1200); the
                        // caller applies 2^(transpose/12).
                        transpose: (100.0 * midi - zone.base_detune) / 100.0,
                        duration_secs: zone.duration_secs,
                        loop_secs: zone.loop_secs,
                        // getParamADSR(..., 0, 0.3, …) - the soundfont peak.
                        envelope_peak: 0.3,
                        soundfont: true,
                    };
                }
                Some(FontState::Loading) => {
                    if priority == LoadPriority::Now {
                        self.shared.font_jobs.promote(font);
                    }
                    return SampleResolution::Loading;
                }
                state @ Some(FontState::Failed { .. }) if !rested(state) => {
                    return SampleResolution::Failed;
                }
                _ => {}
            }
        }
        queue_font(&self.shared, font, priority);
        SampleResolution::Loading
    }

    /// Decoded samples awaiting installation into a playing backend. The
    /// session drains this into the live device (between producer steps) or
    /// into an offline backend before rendering.
    pub fn take_ready(&self) -> Vec<(SampleId, DecodedSample)> {
        std::mem::take(&mut self.shared.ready.lock().expect("ready samples").samples)
    }

    /// Decoded samples waiting to be installed that `known` does not already
    /// account for, cloned and left where they are.
    ///
    /// An owner with nowhere to install them still owes their memory to its
    /// policy: the queue keeps whatever the loaders finish until a device
    /// drains it.
    pub fn peek_ready_unless(
        &self,
        known: impl Fn(SampleId) -> bool,
    ) -> Vec<(SampleId, DecodedSample)> {
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .samples
            .iter()
            .filter(|(id, _)| !known(*id))
            .cloned()
            .collect()
    }

    /// The current decoded payload for a slot still awaiting installation,
    /// cloned without draining it. A payload already taken for installation
    /// is available from the caller's retained copy instead.
    pub fn peek_ready_sample(&self, id: SampleId) -> Option<DecodedSample> {
        let ready = self.shared.ready.lock().expect("ready samples");
        let identity = ready.identity(id)?;
        ready
            .samples
            .iter()
            .rev()
            .find(|(queued_id, decoded)| *queued_id == id && decoded.identity() == identity)
            .map(|(_, decoded)| decoded.clone())
    }

    /// Exact immutable payload currently intended for this sample slot.
    /// Published with decoded PCM, before its URL/font becomes Ready, and kept
    /// after [`Self::take_ready`] even if device installation is deferred. This
    /// is neither an installation acknowledgement nor a playback receipt.
    pub fn decoded_identity(&self, id: SampleId) -> Option<u64> {
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .identity(id)
    }

    /// Hand a decoded sample back after a failed install so a later drain
    /// retries it (the device's install ring was momentarily full). Forgotten
    /// slots and retries superseded by a newer decoded body are discarded.
    pub fn requeue_ready(&self, id: SampleId, decoded: DecodedSample) {
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .push((id, decoded));
    }

    /// Atomically put older retry/recovery PCM before publications completed
    /// since the caller last drained the queue, dropping an older item when a
    /// newer same-ID payload is already known, even if it has left the queue.
    /// Loader threads append under the same mutex, so stale recovery PCM cannot
    /// overwrite a publication that won the race to this queue.
    pub fn requeue_ready_batch_before_newer(&self, older: Vec<(SampleId, DecodedSample)>) {
        if older.is_empty() {
            return;
        }
        let mut ready = self.shared.ready.lock().expect("ready samples");
        let mut seen = [false; SAMPLE_BANK_CAPACITY];
        for (id, _) in ready.samples.iter() {
            if let Some(seen) = seen.get_mut(id.0 as usize) {
                *seen = true;
            }
        }
        // Walk backwards so only the newest item per id in the older batch is
        // kept. Anything already in `ready` was published after that batch and
        // supersedes it completely. Besides preserving last-writer-wins, this
        // prevents repeated recycle attempts from growing duplicate PCM.
        let mut prepend = Vec::with_capacity(older.len().min(SAMPLE_BANK_CAPACITY));
        for (id, decoded) in older.into_iter().rev() {
            let Some(seen) = seen.get_mut(id.0 as usize) else {
                continue;
            };
            if !*seen && ready.accept_identity(id, decoded.identity()) {
                *seen = true;
                prepend.push((id, decoded));
            }
        }
        prepend.reverse();
        prepend.append(&mut ready.samples);
        ready.samples = prepend;
    }

    /// One line per NEW failure since the last call (loud-once logging).
    pub fn take_failures(&self) -> Vec<String> {
        self.take_failures_with_maps()
            .into_iter()
            .map(|failure| failure.message)
            .collect()
    }

    /// [`Self::take_failures`], each with the maps it leaves failed.
    pub fn take_failures_with_maps(&self) -> Vec<SampleFailure> {
        std::mem::take(&mut *self.shared.failures.lock().expect("sample failures"))
    }

    /// Suppress loader-side informational stderr writes for full-screen
    /// clients. Error delivery remains available through [`Self::take_failures`].
    pub fn set_direct_diagnostic_logging(&self, enabled: bool) {
        self.shared
            .direct_diagnostic_logging
            .store(enabled, Ordering::Release);
    }

    /// Files the loader still has in its line, both the ones a score is
    /// waiting on and the bets placed ahead of it.
    ///
    /// What a progress bar reads. It is a queue depth rather than a
    /// fraction: nothing here knows how many files a pre-cache will turn
    /// out to be until the manifests are in, and a bar that jumps backwards
    /// as it learns is worse than one that simply counts down.
    pub fn pending_loads(&self) -> usize {
        let queued = {
            let state = self.shared.jobs.state.lock().expect("sample load queue");
            state.now.len().saturating_add(state.bets.len())
        };
        let caching = self.shared.caching.lock().expect("disk-cache claims").len();
        queued.max(caching)
    }

    /// One of the files a loader has in hand right now - fetching or
    /// decoding - named as a person would: the last segment of its url,
    /// with the percent-escapes undone. `None` when every worker is idle.
    pub fn loading_now(&self) -> Option<String> {
        let loading = self.shared.loading.lock().expect("loading sample");
        loading.iter().next().map(|url| file_name_of_url(url))
    }

    /// Manifests still being fetched. While this is above zero a name that
    /// is not known yet may simply not have arrived.
    pub fn manifests_pending(&self) -> usize {
        self.shared.manifest_pending.load(Ordering::Acquire)
    }

    /// A counter that moves whenever a sound that was loading may now answer
    /// differently: a sample or soundfont finished (or failed), or a
    /// manifest arrived. Read it BEFORE asking for sounds; if it has not
    /// moved since, every "still loading" answer then is still the answer.
    pub fn settled_epoch(&self) -> u64 {
        self.shared.settled_epoch.load(Ordering::Acquire)
    }

    /// Start loading, as bets, the files that the variants of each name
    /// select. For an ordinary bank these are the files that those `n` select.
    /// For a General MIDI name they are the fonts. For a note-keyed bank they
    /// are the same indices in its flattened file list. A name that can play
    /// any variant, because the text names no index, loads its first file
    /// only. The note about to play still resolves its own file. Returns the
    /// number of files requested, including files already loaded or in
    /// progress.
    ///
    /// A tab behind a pad is loaded this way before it plays, so its first
    /// press finds the variants that its text names.
    ///
    /// The loader takes bets newest first, so the first variant of each name,
    /// which is the one a bare name plays, is queued after the other variants
    /// of every name.
    pub fn load_variants_ahead<'a>(
        &self,
        sounds: impl IntoIterator<Item = (&'a str, &'a crate::sounds::Variants)>,
    ) -> usize {
        let found: Vec<(DecodeRate, Warm)> = sounds
            .into_iter()
            .filter(|(name, _)| !rustel_voice::is_native_synth_sound(name))
            .filter_map(|(name, variants)| {
                // Any index is not a request for every file. A bare name
                // plays the first, and that is the one loaded ahead.
                let variants = match variants {
                    crate::sounds::Variants::All => crate::sounds::Variants::first(),
                    only => only.clone(),
                };
                let found = self.look_up(name, |named| match named {
                    Named::Bank(Bank::Array(urls)) => {
                        Warm::Urls(picked(urls, &variants).cloned().collect())
                    }
                    Named::Bank(Bank::Notes(notes)) => {
                        let urls: Vec<Arc<str>> = notes
                            .iter()
                            .flat_map(|(_, urls)| urls.iter().cloned())
                            .collect();
                        Warm::Urls(picked(&urls, &variants).cloned().collect())
                    }
                    Named::Font(fonts) => Warm::Fonts(picked(fonts, &variants).cloned().collect()),
                })?;
                Some((DecodeRate::for_sound(name), found))
            })
            .collect();
        let mut asked = 0usize;
        // Every name's others first, then every name's first.
        for firsts in [false, true] {
            for (decode_rate, found) in &found {
                let (files, fonts) = match found {
                    Warm::Urls(urls) => (split_first(urls, firsts), &[][..]),
                    Warm::Pitched(urls) => (if firsts { &urls[..] } else { &[][..] }, &[][..]),
                    Warm::Fonts(fonts) => (&[][..], split_first(fonts, firsts)),
                };
                for url in in_queue_order(files, LoadPriority::Bet) {
                    let _ =
                        ensure_loading_shared(&self.shared, url, *decode_rate, LoadPriority::Bet);
                }
                for font in in_queue_order(fonts, LoadPriority::Bet) {
                    queue_font(&self.shared, font, LoadPriority::Bet);
                }
                asked += files.len() + fonts.len();
            }
        }
        asked
    }

    /// Forget the decode of `ids` so a later ask fetches and decodes again,
    /// or, for a `talkit` phrase, renders it again.
    ///
    /// Nothing here holds PCM: the loader publishes each decode once,
    /// through [`Self::take_ready`], and afterwards these tables keep only
    /// the id and payload identity. A host reclaiming that PCM must say so, or
    /// `Ready` goes on promising a sample nobody can play - no loader ever
    /// publishes it a second time, so the sound previews as "did not load
    /// in time" and a score that names it plays silence until the process
    /// is restarted.
    ///
    /// A font is forgotten whole: its zones are one decode over one run of
    /// ids, and half of one plays nothing anybody asked for. Every id
    /// invalidated is returned, the untouched zones of a part-dropped font
    /// included, so the caller can retire those with the rest.
    ///
    /// The decode is what is forgotten, not the bytes: a re-ask reads the
    /// sample cache on disk and only reaches the network if it has been
    /// cleared.
    pub fn forget_decoded(&self, ids: &HashSet<SampleId>) -> Vec<SampleId> {
        if ids.is_empty() {
            return Vec::new();
        }
        let mut forgotten: HashSet<SampleId> = HashSet::new();
        {
            let mut fonts = self.shared.fonts.write().expect("font table");
            fonts.retain(|_, state| {
                let FontState::Ready(zones) = state else {
                    return true;
                };
                if !zones.iter().any(|zone| ids.contains(&zone.id)) {
                    return true;
                }
                forgotten.extend(zones.iter().map(|zone| zone.id));
                false
            });
        }
        // Spoken words are forgotten the same way: the next ask renders them
        // again.
        let forget_ready = |state: &mut UrlState, forgotten: &mut HashSet<SampleId>| {
            let UrlState::Ready { id, .. } = state else {
                return true;
            };
            if !ids.contains(id) {
                return true;
            }
            forgotten.insert(*id);
            false
        };
        self.shared
            .by_url
            .write()
            .expect("sample url table")
            .retain(|_, state| forget_ready(state, &mut forgotten));
        self.shared
            .speech
            .lock()
            .expect("speech table")
            .retain(|_, state| forget_ready(state, &mut forgotten));
        {
            let mut ready = self.shared.ready.lock().expect("ready samples");
            for id in &forgotten {
                if let Some(slot) = ready.identities.get_mut(id.0 as usize) {
                    // A retained retry cannot resurrect this one. The id stays
                    // out of circulation until the studio hands it back through
                    // `release_ids`, once nothing can read it.
                    *slot = DecodedIdentity::Forgotten;
                }
            }
            ready.samples.retain(|(id, _)| !forgotten.contains(id));
        }
        forgotten.into_iter().collect()
    }

    /// Take back ids the caller can vouch nothing reads any more: forgotten
    /// here through [`Self::forget_decoded`], cleared from every bank they
    /// were installed in, and past the last frame any queued event or
    /// sounding voice could name them at. The next reservation takes them
    /// before it moves the counter, so a browsing session stops draining
    /// the bank one preview at a time. The tombstone `forget_decoded` left
    /// goes with them: a decode at a reissued id starts from an unknown
    /// identity, like a fresh slot. An id never forgotten - still promised
    /// by a table, or the bundled bd - is refused; a second release is a
    /// no-op.
    pub fn release_ids(&self, ids: impl IntoIterator<Item = SampleId>) {
        let mut ready = self.shared.ready.lock().expect("ready samples");
        let mut free = self.shared.free_ids.lock().expect("free sample ids");
        for id in ids {
            let Some(slot) = ready.identities.get_mut(id.0 as usize) else {
                continue;
            };
            if !matches!(*slot, DecodedIdentity::Forgotten) {
                continue;
            }
            *slot = DecodedIdentity::Unknown;
            free.push_back(id);
        }
    }

    /// How many ids are waiting to be reissued, for a test to watch.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn free_id_count(&self) -> usize {
        self.shared.free_ids.lock().expect("free sample ids").len()
    }

    /// Convert decoded PCM to `rate` from here on.
    ///
    /// `decodeAudioData` resamples every file it decodes to the context's rate,
    /// so playback then only interpolates for `speed`. Owners call this with
    /// the render or device rate before a score can ask for a sound. Samples
    /// already decoded keep the rate they were converted to; playback derives
    /// its read increment from each buffer's own rate, so they stay in tune
    /// either way, and only the conversion filter is the older one.
    pub fn set_render_rate(&self, rate: u32) {
        if rate > 0 {
            self.shared.render_rate.store(rate, Ordering::Release);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn render_rate_for_test(&self) -> u32 {
        self.shared.render_rate.load(Ordering::Acquire)
    }

    /// Start loading the files a sound name asks for, before anything asks
    /// to play it.
    ///
    /// `"bd"` warms the first file, `"bd:3"` only that variant, and a bare
    /// `gm_*` name still warms every font it holds. A live set cannot wait for
    /// the network at the moment of the first hit - the artist hears the
    /// gap as a dropped beat, which is exactly what `preload` exists to
    /// prevent.
    ///
    /// This method consults the maps published when it is called and returns
    /// the exact number of files it started (already-loaded ones included,
    /// since asking again is free). It does not retain an unknown name behind
    /// an asynchronous Session registration; Session's internal path carries
    /// those preloads in the owning manifest job instead.
    pub fn prefetch(&self, spec: &str) -> usize {
        self.prefetch_with(spec, LoadPriority::Now)
    }

    /// Warm a sound at a chosen place in the loader's line. A pre-cache is
    /// a bet: it goes behind whatever a score is about to play.
    pub fn prefetch_with(&self, spec: &str, priority: LoadPriority) -> usize {
        match self.manifest_context().prefetch_known(spec, priority) {
            PrefetchStatus::Requested(files) => files,
            PrefetchStatus::Deferred | PrefetchStatus::Unknown => 0,
        }
    }

    /// Fetch every file a sound name can produce onto disk, without seating
    /// it in the live sample bank.
    ///
    /// `Advanced ▸ cache whole library` is this: thousands of files, a
    /// gigabyte, and none of them need to occupy a decoded slot until a
    /// note actually asks. Prefetching them the ordinary way filled the
    /// 2048-slot bank and then refused the rest as capacity exceeded.
    pub fn cache_to_disk(&self, spec: &str) -> usize {
        match self.manifest_context().prefetch_known_with_intent(
            spec,
            LoadPriority::Bet,
            PrefetchIntent::CacheDisk,
        ) {
            PrefetchStatus::Requested(files) => files,
            PrefetchStatus::Deferred | PrefetchStatus::Unknown => 0,
        }
    }

    /// Block until every url this library has started loading is settled or
    /// the deadline passes. Prefetch/offline convenience; the live path
    /// never waits.
    pub fn wait_until_idle(&self, deadline: std::time::Duration) {
        self.wait_until_idle_cancellable(deadline, None);
    }

    /// The offline wait with an optional host stop flag. Stopping this wait
    /// leaves the library and its background loaders available to the host.
    pub(crate) fn wait_until_idle_cancellable(
        &self,
        deadline: std::time::Duration,
        cancelled: Option<&AtomicBool>,
    ) {
        let started = std::time::Instant::now();
        // A plugin a score named loads on the plugin thread. The wait has
        // the same deadline and the same stop flag as the samples.
        #[cfg(feature = "vst")]
        if let Some(plugins) = crate::vst::started() {
            plugins.wait_idle_within(deadline, || {
                cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed))
            });
        }
        loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return;
            }
            // A manifest job starts its owned preloads before release-publishing
            // zero. Reading that counter first means an observed zero happens
            // before these table reads; a load cannot appear between an old
            // table snapshot and a new zero and be mistaken for idle.
            let pending = self.shared.manifest_pending.load(Ordering::Acquire) != 0
                || {
                    let by_url = self.shared.by_url.read().expect("sample url table");
                    by_url
                        .values()
                        .any(|state| matches!(state, UrlState::Loading))
                }
                || {
                    let fonts = self.shared.fonts.read().expect("font table");
                    fonts
                        .values()
                        .any(|state| matches!(state, FontState::Loading))
                }
                || {
                    !self
                        .shared
                        .caching
                        .lock()
                        .expect("disk-cache claims")
                        .is_empty()
                };
            if !pending || started.elapsed() >= deadline {
                return;
            }
            #[cfg(test)]
            wait_tests::entering_wait();
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    #[cfg(test)]
    mod empty_font_tests {
        //! Empty General MIDI font lists have no playable variants.

        use super::*;

        #[test]
        fn an_empty_font_list_counts_no_variants_and_resolves_to_nothing() {
            let (manifest_queue, _jobs) = manifest_queue::manifest_queue();
            let library = SampleLibrary {
                banks: Arc::new(RwLock::new(HashMap::new())),
                custom: Arc::new(RwLock::new(HashMap::new())),
                global: Arc::new(RwLock::new(HashMap::new())),
                gm: Arc::new(HashMap::from([(
                    "gm_empty".to_owned(),
                    Vec::<Arc<str>>::new(),
                )])),
                shared: Arc::new(crate::samples::codec_tests::sample_test_shared(1)),
                manifest_queue,
                manifest_order: Mutex::new(()),
                bus_names: rustel_voice::BusNames::default(),
            };
            assert_eq!(
                library.variants_of("gm_empty"),
                None,
                "an empty list has no variants to count"
            );
            assert!(
                matches!(
                    library.resolve("gm_empty", 1.0, 60.0),
                    SampleResolution::Failed
                ),
                "resolving against no fonts fails instead of panicking"
            );
            assert_eq!(sound_index(3.0, 0), 0, "the wrap itself is div-safe");
        }
    }

    #[cfg(test)]
    pub(in crate::samples) mod live_warm_tests {
        use super::*;

        /// A library with its queues but no workers; the test holds the manifest
        /// worker's end of the line.
        pub(in crate::samples) fn library() -> (SampleLibrary, ManifestJobs) {
            let (manifest_queue, jobs) = manifest_queue::manifest_queue();
            let library = SampleLibrary {
                banks: Arc::new(RwLock::new(HashMap::new())),
                custom: Arc::new(RwLock::new(HashMap::new())),
                global: Arc::new(RwLock::new(HashMap::new())),
                gm: Arc::new(HashMap::from([(
                    "gm_test".to_owned(),
                    ["font0", "font1", "font2", "font3"]
                        .map(Arc::<str>::from)
                        .to_vec(),
                )])),
                shared: Arc::new(crate::samples::codec_tests::sample_test_shared(1)),
                manifest_queue,
                manifest_order: Mutex::new(()),
                bus_names: rustel_voice::BusNames::default(),
            };
            (library, jobs)
        }

        #[test]
        fn live_warm_requests_one_gm_variant_but_explicit_preload_keeps_all() {
            let (library, _receiver) = library();
            assert_eq!(
                library
                    .warm_score_sounds_async(&["gm_test".into()], &ScoreSampleAccess::denied())
                    .unwrap(),
                PrefetchStatus::Requested(1)
            );
            assert_eq!(library.shared.fonts.read().unwrap().len(), 1);
            assert_eq!(&*library.shared.font_jobs.try_pop().unwrap(), "font0");

            // Asking explicitly still covers all variants, including an already
            // queued/decoded one in its public count, exactly like ordinary prefetch.
            assert_eq!(library.prefetch("gm_test"), 4);
            assert_eq!(library.shared.fonts.read().unwrap().len(), 4);
            let remaining: Vec<_> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
                .map(|font| font.to_string())
                .collect();
            assert_eq!(remaining, ["font1", "font2", "font3"]);
        }

        #[test]
        fn gm_preload_and_live_warm_use_numeric_sound_indices() {
            for (index, expected) in [
                ("2", "font2"),
                ("-1", "font3"),
                ("1.5", "font2"),
                ("-0.5", "font0"),
                ("6", "font2"),
            ] {
                for intent in [PrefetchIntent::Explicit, PrefetchIntent::LiveWarm] {
                    let (library, _receiver) = library();
                    let spec = format!("gm_test:{index}");
                    let result = match intent {
                        PrefetchIntent::Explicit => {
                            PrefetchStatus::Requested(library.prefetch(&spec))
                        }
                        PrefetchIntent::LiveWarm => library
                            .warm_score_sounds_async(&[spec], &ScoreSampleAccess::denied())
                            .unwrap(),
                        PrefetchIntent::CacheDisk => unreachable!("not in this loop"),
                    };
                    assert_eq!(result, PrefetchStatus::Requested(1));
                    assert_eq!(&*library.shared.font_jobs.try_pop().unwrap(), expected);
                    assert!(library.shared.font_jobs.try_pop().is_none());
                }
            }
        }

        #[test]
        fn actual_gm_resolution_promotes_a_warmed_variant_without_duplicate_jobs() {
            let (library, _receiver) = library();
            library
                .warm_score_sounds_async(
                    &["gm_test:0".into(), "gm_test:1".into(), "gm_test:3".into()],
                    &ScoreSampleAccess::denied(),
                )
                .unwrap();
            assert!(matches!(
                SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
                SampleResolution::Loading
            ));
            for _ in 0..2 {
                assert!(matches!(
                    SampleLookup::resolve(&library, "gm_test", 0.0, 60.0),
                    SampleResolution::Loading
                ));
            }
            let order: Vec<_> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
                .map(|font| font.to_string())
                .collect();
            assert_eq!(order, ["font2", "font0", "font3", "font1"]);
        }

        #[test]
        fn selected_gm_retry_keeps_cooldown_and_precedes_speculative_fonts() {
            let (library, _receiver) = library();
            let failed: Arc<str> = Arc::from("font2");
            library
                .shared
                .fonts
                .write()
                .unwrap()
                .insert(failed.clone(), FontState::Failed { at: Instant::now() });
            assert_eq!(library.prefetch("gm_test:1.5"), 1);
            assert!(
                library.shared.font_jobs.try_pop().is_none(),
                "fresh failure rests"
            );
            assert!(matches!(
                SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
                SampleResolution::Failed
            ));

            library.shared.fonts.write().unwrap().insert(
                failed.clone(),
                FontState::Failed {
                    at: Instant::now() - FAILED_RETRY_AFTER,
                },
            );
            library
                .warm_score_sounds_async(
                    &["gm_test:2".into(), "gm_test:3".into()],
                    &ScoreSampleAccess::denied(),
                )
                .unwrap();
            assert!(
                matches!(
                    library.shared.fonts.read().unwrap().get(&failed),
                    Some(FontState::Failed { .. })
                ),
                "live speculation does not retry even a rested failure"
            );

            assert_eq!(library.prefetch("gm_test:1.5"), 1);
            assert!(matches!(
                SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
                SampleResolution::Loading
            ));
            let order: Vec<_> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
                .map(|font| font.to_string())
                .collect();
            assert_eq!(
                order,
                ["font2", "font3"],
                "retry is unique and precedes bets"
            );
            assert_eq!(
                library.shared.fonts.read().unwrap().len(),
                2,
                "selected retry did not load other variants"
            );
        }

        #[test]
        fn queued_gm_retry_reuses_only_forgotten_and_released_font_slots() {
            let (library, _receiver) = library();
            let font = br#"var fixture={zones:[{originalPitch:6000,keyRangeLow:0,keyRangeHigh:63,sampleRate:48000,sample:'AAAAAA=='},{originalPitch:6000,keyRangeLow:64,keyRangeHigh:127,sampleRate:48000,sample:'AAAAAA=='}]};"#;
            let old_zones = decode_font(font, &library.shared).unwrap();
            let old_ids: Vec<_> = old_zones.iter().map(|zone| zone.id).collect();
            library
                .shared
                .fonts
                .write()
                .unwrap()
                .insert(Arc::from("font0"), FontState::Ready(Arc::new(old_zones)));
            let _ = library.take_ready();
            library
                .shared
                .next_id
                .store(SAMPLE_BANK_CAPACITY as u32, Ordering::Relaxed);

            // A published font still owns these ids. Releasing before forgetting it
            // must not make the new font decode over a potentially sounding voice.
            library.release_ids(old_ids.iter().copied());
            assert_eq!(library.free_id_count(), 0);
            assert!(
                decode_font(font, &library.shared)
                    .err()
                    .unwrap()
                    .contains("sample bank capacity")
            );
            library.shared.fonts.write().unwrap().insert(
                Arc::from("font2"),
                FontState::Failed {
                    at: Instant::now() - FAILED_RETRY_AFTER,
                },
            );

            let mut forgotten = library.forget_decoded(&HashSet::from([old_ids[0]]));
            forgotten.sort_by_key(|id| id.0);
            assert_eq!(
                forgotten, old_ids,
                "one retired zone retires its whole font"
            );
            assert_eq!(
                library.free_id_count(),
                0,
                "forgetting alone does not permit reuse"
            );
            library.release_ids(forgotten.iter().copied());
            library.release_ids(forgotten);
            assert_eq!(library.free_id_count(), 2, "duplicate release is inert");

            library
                .warm_score_sounds_async(&["gm_test:3".into()], &ScoreSampleAccess::denied())
                .unwrap();
            assert!(matches!(
                SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
                SampleResolution::Loading
            ));
            let retry = library.shared.font_jobs.try_pop().unwrap();
            assert_eq!(
                &*retry, "font2",
                "rested retry precedes the speculative variant"
            );
            let zones = decode_font(font, &library.shared).unwrap();
            assert_eq!(
                zones.iter().map(|zone| zone.id).collect::<Vec<_>>(),
                old_ids
            );
            library
                .shared
                .fonts
                .write()
                .unwrap()
                .insert(retry, FontState::Ready(Arc::new(zones)));
            assert!(matches!(
                SampleLookup::resolve(&library, "gm_test", 2.0, 60.0),
                SampleResolution::Found { id, .. } if id == old_ids[0]
            ));
            assert_eq!(
                library.shared.next_id.load(Ordering::Relaxed),
                SAMPLE_BANK_CAPACITY as u32
            );
            assert_eq!(library.free_id_count(), 0);
            assert_eq!(&*library.shared.font_jobs.try_pop().unwrap(), "font3");
            assert!(library.shared.font_jobs.try_pop().is_none());
        }

        #[test]
        fn live_warm_of_a_bare_name_loads_the_first_file_of_an_overriding_bank() {
            let (library, _receiver) = library();
            library.global.write().unwrap().insert(
                "gm_test".into(),
                Bank::Array(vec![
                    Arc::from("https://example.com/a.wav"),
                    Arc::from("https://example.com/b.wav"),
                ]),
            );
            assert_eq!(
                library
                    .warm_score_sounds_async(&["gm_test".into()], &ScoreSampleAccess::denied())
                    .unwrap(),
                PrefetchStatus::Requested(1)
            );
            assert!(library.shared.fonts.read().unwrap().is_empty());
            let state = library.shared.jobs.state.lock().unwrap();
            assert!(state.now.is_empty());
            assert_eq!(state.bets.len(), 1);
            assert_eq!(&*state.bets[0].url, "https://example.com/a.wav");
        }

        #[test]
        fn live_warm_waits_for_an_overriding_manifest_before_selecting_gm_or_bank() {
            let (library, receiver) = library();
            let context = library.manifest_context();
            let worker = std::thread::spawn(move || run_manifest_worker(context, receiver));
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Custom);
            library
            .enqueue_manifest_work_async(ManifestWork::Custom {
                effects: vec![(
                    r#"{"gm_test":["https://example.com/new-a.wav","https://example.com/new-b.wav"]}"#
                        .into(),
                    None,
                )],
                preloads: Vec::new(),
                intent: PrefetchIntent::Explicit,
                access: ManifestAccess::Trusted,
                continue_on_error: false,
                layer: BankLayer::Score,
            })
            .unwrap();
            reached.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(
                library
                    .warm_score_sounds_async(&["gm_test".into()], &ScoreSampleAccess::denied())
                    .unwrap(),
                PrefetchStatus::Deferred
            );
            assert!(library.shared.fonts.read().unwrap().is_empty());
            assert!(library.shared.by_url.read().unwrap().is_empty());
            release.send(()).unwrap();
            // FIFO completion proves the live warm has finished without waiting for
            // the deliberately worker-less audio queues to decode anything.
            library
                .enqueue_manifest_work_blocking(ManifestWork::Custom {
                    effects: Vec::new(),
                    preloads: Vec::new(),
                    intent: PrefetchIntent::Explicit,
                    access: ManifestAccess::Trusted,
                    continue_on_error: false,
                    layer: BankLayer::Score,
                })
                .unwrap();
            assert!(library.shared.fonts.read().unwrap().is_empty());
            let urls = library.shared.by_url.read().unwrap();
            assert_eq!(urls.len(), 1);
            assert!(urls.contains_key("https://example.com/new-a.wav"));
            drop(urls);
            let state = library.shared.jobs.state.lock().unwrap();
            assert!(state.now.is_empty(), "deferred warm lost its Bet intent");
            assert_eq!(state.bets.len(), 1);
            drop(state);
            drop(library);
            worker.join().unwrap();
        }

        #[test]
        fn dropping_a_library_closes_its_idle_font_queue() {
            let (library, _receiver) = library();
            let jobs = Arc::clone(&library.shared.font_jobs);
            drop(library);
            assert!(jobs.pop().is_none());
        }

        #[cfg(feature = "device-audio")]
        #[test]
        fn live_session_warms_evaluated_gm_variants_before_source_only_bets() {
            for (source, from_cycle, expected) in [
                ("n(2).s('gm_test')", 0.0, vec!["font2", "font0"]),
                ("s('gm_test:3')", 0.0, vec!["font3"]),
                (
                    r#"n("<0 1 2 3>").s("gm_test").slow(4)"#,
                    8.0,
                    vec!["font2", "font0"],
                ),
            ] {
                let (library, _receiver) = library();
                let library = Arc::new(library);
                let mut session = crate::Session::new().unwrap();
                session.set_direct_diagnostic_logging(false);
                session.set_sample_library_for_test(Arc::clone(&library));
                session.evaluate(source).unwrap();
                session
                    .warm_live_samples_checked(
                        &crate::sounds::in_score(source),
                        from_cycle,
                        Duration::from_millis(50),
                    )
                    .1
                    .unwrap();
                let jobs: Vec<_> = std::iter::from_fn(|| library.shared.font_jobs.try_pop())
                    .map(|font| font.to_string())
                    .collect();
                assert_eq!(jobs, expected, "{source} at cycle {from_cycle}");
            }
        }

        #[cfg(feature = "device-audio")]
        #[test]
        fn live_session_queries_dynamic_sound_names_even_without_literal_bets() {
            let (library, _receiver) = library();
            let library = Arc::new(library);
            let mut session = crate::Session::new().unwrap();
            session.set_direct_diagnostic_logging(false);
            session.set_sample_library_for_test(Arc::clone(&library));
            session
                .evaluate("const name = 'gm_' + 'test'; n(2).s(name)")
                .unwrap();
            let (window, warmed) =
                session.warm_live_samples_checked(&[], 0.0, Duration::from_millis(50));
            warmed.unwrap();
            assert_eq!(
                window,
                [(
                    "gm_test".to_owned(),
                    crate::sounds::Variants::Only([2].into())
                )],
                "the window answers the name the score built"
            );
            assert_eq!(&*library.shared.font_jobs.try_pop().unwrap(), "font2");
            assert!(library.shared.font_jobs.try_pop().is_none());
        }

        /// Speculative warming pairs each bank name with each sound name in the
        /// source. Many pairs do not exist, so these misses produce no warning.
        /// An explicit `preload()` reports missing names because the user requested them.
        #[test]
        fn a_warms_miss_is_quiet_and_an_explicit_preloads_miss_is_not() {
            let (library, _receiver) = library();

            assert_eq!(
                library
                    .warm_score_sounds_async(
                        &["korgminipops_sawtooth".into(), "AkaiLinn_kik:1".into()],
                        &ScoreSampleAccess::denied(),
                    )
                    .unwrap(),
                PrefetchStatus::Requested(0),
                "neither pairing exists"
            );
            assert!(
                library.take_failures().is_empty(),
                "a bet that missed is how the bet works, not a fault in it"
            );

            // The same library, the same absent name, asked for on purpose.
            library
                .register_trusted_batch_async_for_test(
                    Vec::new(),
                    &["korgminipops_sawtooth".into()],
                )
                .expect("the batch is admitted");
            let said = library.take_failures();
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(
                said[0].contains("korgminipops_sawtooth"),
                "and it names what was not found: {said:?}"
            );

            // What the warm CAN find it still loads, quietly.
            assert_eq!(
                library
                    .warm_score_sounds_async(&["gm_test".into()], &ScoreSampleAccess::denied())
                    .unwrap(),
                PrefetchStatus::Requested(1)
            );
            assert!(library.take_failures().is_empty());
        }
    }

    #[cfg(test)]
    mod soundfont_controls_tests {
        //! A General MIDI soundfont zone plays at its note's pitch, from its start:
        //! the sampler's own controls do not reach it.

        use super::*;

        const RATE: u32 = 48_000;

        /// A two-second 440 Hz tone recorded as a4 (`baseDetune` 6900), as the one
        /// zone of `gm_test`'s first font, over the whole key range, with no loop.
        fn library_with_a_tone_zone() -> SampleLibrary {
            let (library, _jobs) = crate::samples::variant_tests::library();
            library.shared.fonts.write().unwrap().insert(
                Arc::from("font0"),
                FontState::Ready(Arc::new(vec![FontZone {
                    key_lo: 0.0,
                    key_hi: 127.0,
                    base_detune: 6900.0,
                    id: SampleId(40),
                    duration_secs: 2.0,
                    loop_secs: None,
                }])),
            );
            library
        }

        /// One second of the left channel of `gm_test` playing a half-second e5 at
        /// each of `onsets` (seconds), with `controls` added to every hap.
        fn render(
            library: &SampleLibrary,
            onsets: &[f64],
            controls: &serde_json::Value,
        ) -> Vec<f32> {
            let mut value = serde_json::json!({ "s": "gm_test", "note": "e5" });
            let object = value.as_object_mut().expect("object");
            for (key, control) in controls.as_object().expect("controls") {
                object.insert(key.clone(), control.clone());
            }
            let events: Vec<_> = onsets
                .iter()
                .zip(1..)
                .map(|(&onset, id)| {
                    rustel_voice::resolve_voice_with_samples(
                        &value, id, 0.5, onset, RATE, 0.5, library,
                    )
                    .unwrap_or_else(|error| panic!("{value}: {error}"))
                })
                .collect();
            let tone = (0..2 * RATE)
                .map(|frame| {
                    0.5 * (std::f32::consts::TAU * 440.0 * frame as f32 / RATE as f32).sin()
                })
                .collect();
            let mut backend = rustel_audio::ScalarBackend::new();
            assert!(
                backend
                    .install_sample(
                        SampleId(40),
                        Box::new(DecodedSample::from_parts(RATE, 1, tone).expect("tone"))
                    )
                    .is_ok()
            );
            let pcm =
                rustel_audio::render_pcm(&mut backend, RATE, RATE as usize, &events).expect("pcm");
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|frame| frame[0])
                .collect()
        }

        /// Frequency in Hz from the rising zero crossings of `left[from..to]`.
        fn pitch(left: &[f32], from: usize, to: usize) -> f64 {
            let crossings: Vec<f64> = (from..to - 1)
                .filter(|&frame| left[frame] <= 0.0 && left[frame + 1] > 0.0)
                .map(|frame| {
                    let (a, b) = (f64::from(left[frame]), f64::from(left[frame + 1]));
                    frame as f64 + a / (a - b)
                })
                .collect();
            assert!(crossings.len() > 2, "no tone between {from} and {to}");
            let span = crossings[crossings.len() - 1] - crossings[0];
            (crossings.len() - 1) as f64 * f64::from(RATE) / span
        }

        #[test]
        fn a_soundfont_note_plays_at_its_pitch_whatever_the_sampler_controls_say() {
            let library = library_with_a_tone_zone();
            let e5 = 440.0 * 2f64.powf(7.0 / 12.0);
            let plain = render(&library, &[0.0], &serde_json::json!({}));
            let heard = pitch(&plain, 2_400, 19_200);
            assert!(
                (heard / e5 - 1.0).abs() < 0.002,
                "e5 sounds at {heard:.2} Hz, not {e5:.2} Hz"
            );
            for controls in [
                serde_json::json!({ "speed": 0.3 }),
                serde_json::json!({ "speed": -1 }),
                serde_json::json!({ "speed": 0 }),
                serde_json::json!({ "unit": "c", "speed": 2 }),
                serde_json::json!({ "begin": 0.4, "end": 0.8 }),
                serde_json::json!({ "nudge": 0.01 }),
            ] {
                let played = render(&library, &[0.0], &controls);
                let heard = pitch(&played, 2_400, 19_200);
                assert!(
                    (heard / e5 - 1.0).abs() < 0.002,
                    "{controls}: e5 sounds at {heard:.2} Hz, not {e5:.2} Hz"
                );
                assert!(
                    played == plain,
                    "{controls} changed the soundfont note, which plays like the plain one"
                );
            }
        }

        /// A later note in the same `cut` group leaves an earlier soundfont note
        /// sounding.
        #[test]
        fn a_soundfont_note_is_not_cut_by_the_next() {
            let library = library_with_a_tone_zone();
            let onsets = [0.0, 0.25];
            let plain = render(&library, &onsets, &serde_json::json!({}));
            let cut = render(&library, &onsets, &serde_json::json!({ "cut": 1 }));
            assert!(cut == plain, "cut(1) cut the first soundfont note short");
        }
    }

    #[cfg(test)]
    pub(in crate::samples) mod codec_tests {
        use super::*;

        /// The size limit applies during the read, not after it. `/dev/zero`
        /// never ends, so a read without a bound would never return.
        #[cfg(unix)]
        #[test]
        fn an_endless_local_sample_is_refused_rather_than_read() {
            // `/dev/zero` reports a length of zero and never ends, so the stat
            // says nothing and the bounded read is what refuses it. That is the
            // case the bound exists for.
            let error =
                fetch_located("file:///dev/zero").expect_err("an endless file must be refused");
            assert!(
                error.contains("the most one sound can hold"),
                "the refusal did not name the size limit: {error}"
            );
            // And it says the ceiling in the units a reader thinks in, so a
            // sample that is genuinely too big can be measured against it.
            assert!(
                error.contains(&rustel_audio::format_sample_bytes(
                    rustel_audio::sample_pcm_ceiling()
                )),
                "{error}"
            );

            // A real file that is plainly too big is refused off its own
            // length, before anything is read: it can say its own size, which
            // the endless one above cannot.
            let directory = tempfile::tempdir().expect("temp dir");
            let big = directory.path().join("too big.wav");
            let ceiling = rustel_audio::sample_pcm_ceiling();
            rustel_audio::set_sample_pcm_ceiling(rustel_audio::MIN_SAMPLE_PCM_BYTES);
            std::fs::write(&big, vec![0u8; rustel_audio::MIN_SAMPLE_PCM_BYTES + 1])
                .expect("fixture");
            let error = fetch_located(&format!("file://{}", big.display()))
                .expect_err("a file past the ceiling must be refused");
            assert!(error.contains("past the"), "{error}");
            assert!(
                error.contains(&rustel_audio::format_sample_bytes(
                    rustel_audio::MIN_SAMPLE_PCM_BYTES + 1
                )),
                "the refusal names the file's own size: {error}"
            );
            // And one inside the ceiling still arrives whole.
            let small = directory.path().join("fine.wav");
            std::fs::write(&small, vec![7u8; 4096]).expect("fixture");
            let bytes = fetch_located(&format!("file://{}", small.display())).expect("read");
            assert_eq!(bytes.len(), 4096);
            rustel_audio::set_sample_pcm_ceiling(ceiling);
        }

        /// The bound must not break ordinary local samples.
        #[test]
        fn an_ordinary_local_sample_still_reads_whole() {
            let path =
                std::env::temp_dir().join(format!("rustel-sample-{}.wav", std::process::id()));
            let contents: Vec<u8> = (0..4096u32).map(|byte| byte as u8).collect();
            std::fs::write(&path, &contents).expect("write the fixture");
            let read =
                fetch_located(&format!("file://{}", path.display())).expect("read the fixture");
            let _ = std::fs::remove_file(&path);
            assert_eq!(read, contents, "the local sample did not come back whole");
        }

        /// A folder that links back to itself must not be walked twice.
        ///
        /// `path.is_dir()` follows a symlink, so a cycle produced the same sample
        /// over and over: 41 entries from one file, which silently shifts every
        /// `n()` index in the bank. The scan reads the entry's own file type and
        /// never follows one.
        #[cfg(unix)]
        #[test]
        fn a_directory_symlink_cycle_does_not_duplicate_a_bank() {
            let root = std::env::temp_dir().join(format!("rustel-cycle-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("kick")).expect("dirs");
            std::fs::write(root.join("kick/1.wav"), b"RIFF").expect("sample");
            let root = root.canonicalize().expect("canonical");
            std::os::unix::fs::symlink(&root, root.join("kick/loop")).expect("symlink");

            let banks = scan_sample_folder(&root).expect("scan");
            let files: usize = banks.values().map(Vec::len).sum();
            assert_eq!(files, 1, "the cycle was walked: {banks:?}");
            assert_eq!(banks["kick"], vec!["kick/1.wav".to_owned()]);
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn folder_scan_working_set_is_bounded_before_retention() {
            let root =
                std::env::temp_dir().join(format!("rustel-scan-work-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("aaa/bbb/ccc")).expect("fixture folders");
            std::fs::write(root.join("aaa/bbb/ccc/tone.wav"), b"RIFF").expect("fixture sample");
            let root = root.canonicalize().expect("canonical root");
            let first = root.join("aaa");
            let second = first.join("bbb");
            let third = second.join("ccc");
            let relative = third
                .join("tone.wav")
                .strip_prefix(&root)
                .expect("relative sample")
                .to_string_lossy()
                .into_owned();
            let stack_bytes = sample_scan_stack_slots(4)
                .and_then(|slots| slots.checked_mul(std::mem::size_of::<PendingSampleDirectory>()))
                .expect("stack accounting");
            let path_pair = |left: &Path, right: &Path| {
                sample_scan_path_bytes(left)
                    .and_then(|bytes| bytes.checked_add(sample_scan_path_bytes(right)?))
                    .expect("path accounting")
            };
            let peak_paths = path_pair(&root, &first)
                .max(path_pair(&first, &second))
                .max(path_pair(&second, &third));
            let peak_entry = sample_scan_path_bytes(&third)
                .and_then(|bytes| bytes.checked_add(sample_scan_entry_bytes("ccc", &relative)?))
                .expect("entry accounting");
            let required = stack_bytes
                .checked_add(peak_paths.max(peak_entry))
                .expect("working-set accounting");
            let exact = SampleScanLimits {
                examined_entries: 4,
                manifest_entries: 1,
                manifest_bytes: 1_024,
                working_bytes: required,
            };

            let banks = scan_sample_folder_with_limits(&root, exact)
                .expect("the exact scanner working-set limit must succeed");
            assert_eq!(banks.values().map(Vec::len).sum::<usize>(), 1);

            let under = SampleScanLimits {
                working_bytes: required - 1,
                ..exact
            };
            let error = scan_sample_folder_with_limits(&root, under)
                .expect_err("one byte below the scanner working set must refuse");
            assert!(matches!(
                error,
                SampleFolderScanError::Limit {
                    resource: "scanner working-set bytes",
                    limit
                } if limit == required - 1
            ));
            let _ = std::fs::remove_dir_all(root);
        }

        /// A 16-bit mono WAV of `frames` samples at `rate`, for the decode tests.
        pub(in crate::samples) fn wav_bytes(rate: u32, frames: usize) -> Vec<u8> {
            let mut pcm = Vec::with_capacity(frames * 2);
            for frame in 0..frames {
                let phase = std::f64::consts::TAU * 1000.0 * frame as f64 / f64::from(rate);
                pcm.extend_from_slice(&((phase.sin() * 16000.0) as i16).to_le_bytes());
            }
            let mut wav = Vec::with_capacity(44 + pcm.len());
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&((36 + pcm.len()) as u32).to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16u32.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&rate.to_le_bytes());
            wav.extend_from_slice(&(rate * 2).to_le_bytes());
            wav.extend_from_slice(&2u16.to_le_bytes());
            wav.extend_from_slice(&16u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
            wav.extend_from_slice(&pcm);
            wav
        }

        /// A sample arrives at the context's rate, the way `decodeAudioData` hands
        /// one back, so playback interpolates for `speed` and nothing else.
        #[test]
        fn a_sample_decodes_to_the_context_rate() {
            let bytes = wav_bytes(44_100, 4410);
            let decoded = decode_guarded("kit/hit.wav", &bytes, Some(48_000)).expect("decode");
            assert_eq!(decoded.sample_rate(), 48_000);
            assert_eq!(decoded.frames(), 4800);
            // Same rate in and out is a copy, not a trip through the kernel.
            let same = decode_guarded("kit/hit.wav", &wav_bytes(48_000, 4800), Some(48_000))
                .expect("decode");
            assert_eq!(same.sample_rate(), 48_000);
            assert_eq!(same.frames(), 4800);
        }

        /// A wavetable keeps the file's rate: the worklet slices the buffer into
        /// frames, so converting first would cut every frame at the wrong stride
        /// and play a waveform the table does not contain.
        #[test]
        fn a_wavetable_keeps_the_files_own_rate() {
            let bytes = wav_bytes(88_200, 8192);
            let decoded = decode_guarded("wt/table.wav", &bytes, None).expect("decode");
            assert_eq!(decoded.sample_rate(), 88_200);
            assert_eq!(decoded.frames(), 8192);
        }

        #[test]
        fn only_wt_named_banks_decode_at_the_files_rate() {
            assert_eq!(DecodeRate::for_sound("wt_digital"), DecodeRate::Native);
            assert_eq!(DecodeRate::for_sound("wt_vgame"), DecodeRate::Native);
            assert_eq!(DecodeRate::for_sound("bd"), DecodeRate::Context);
            assert_eq!(DecodeRate::for_sound("piano"), DecodeRate::Context);
            // Not a prefix match on "wt" alone.
            assert_eq!(DecodeRate::for_sound("wtf"), DecodeRate::Context);
        }

        #[test]
        fn the_file_extension_picks_the_decoder() {
            assert_eq!(codec_for("file:///kit/kick.wav"), Codec::Wav);
            assert_eq!(codec_for("https://example.com/piano/C4.mp3"), Codec::Mp3);
            // A folder of phone recordings is the reason ogg matters at all.
            assert_eq!(
                codec_for("file:///s/artist/audio_2025-10-08.ogg"),
                Codec::Ogg
            );
            assert_eq!(codec_for("file:///s/artist/voice.oga"), Codec::Ogg);
            assert_eq!(codec_for("file:///s/ARTIST/SHOUT.OGG"), Codec::Ogg);
        }

        #[test]
        fn a_query_string_is_not_part_of_the_filename() {
            // A cache-busted bank URL still names its codec in the path.
            assert_eq!(codec_for("https://example.com/hit.ogg?v=2"), Codec::Ogg);
            assert_eq!(codec_for("https://example.com/hit.mp3#frag"), Codec::Mp3);
            // Without the split this reads the extension as "com/hit" and falls
            // back to wav, which decodes to "not a RIFF file".
            assert_eq!(codec_for("https://example.com/hit?name=a.wav"), Codec::Wav);
        }

        /// A local file URL has no Windows verbatim prefix (`\\?\C:\...`). The
        /// `?` of that prefix would read as a query string and hide the file
        /// extension from `codec_for`.
        #[test]
        fn a_local_file_url_never_carries_the_windows_verbatim_prefix() {
            assert_eq!(
                without_verbatim_prefix(r"\\?\C:\Users\me\Downloads\a_sample.mp3"),
                r"C:\Users\me\Downloads\a_sample.mp3"
            );
            assert_eq!(
                without_verbatim_prefix(r"\\?\UNC\server\share\kit\kick.ogg"),
                r"\\server\share\kit\kick.ogg"
            );
            assert_eq!(
                without_verbatim_prefix("/home/me/kit/kick.wav"),
                "/home/me/kit/kick.wav"
            );
            assert_eq!(
                without_verbatim_prefix(r"C:\plain\already.wav"),
                r"C:\plain\already.wav"
            );
            let url = local_file_url(Path::new(r"\\?\C:\Users\me\Downloads\a_sample.mp3"));
            assert_eq!(url, r"file://C:\Users\me\Downloads\a_sample.mp3");
            assert_eq!(codec_for(&url), Codec::Mp3);
            assert_eq!(
                codec_for(&local_file_url(Path::new(r"\\?\C:\s\voice.oga"))),
                Codec::Ogg
            );
        }

        /// The same, through the walk itself: a folder registered by its
        /// canonical path yields URLs whose extension the decoder can read, and
        /// that `fetch_located` still opens.
        #[test]
        fn a_walked_folder_spells_urls_the_decoder_and_the_reader_both_take() {
            let directory = tempfile::tempdir().expect("temp dir");
            let kit = directory.path().join("kit");
            std::fs::create_dir_all(&kit).expect("bank folder");
            std::fs::write(kit.join("hit.mp3"), b"ID3mp3bytes").expect("sample");
            let root = directory.path().canonicalize().expect("canonical root");
            let banks = folder_banks(&root, &[]).expect("walk");
            let (_, Bank::Array(urls)) = banks.iter().find(|(name, _)| name == "kit").expect("kit")
            else {
                panic!("a folder of audio is an array bank");
            };
            assert_eq!(urls.len(), 1);
            let url = urls[0].as_ref();
            assert!(!url.contains(r"\\?\"), "{url}");
            assert_eq!(codec_for(url), Codec::Mp3, "{url}");
            assert_eq!(fetch_located(url).expect("readable"), b"ID3mp3bytes");
        }

        /// Direct files become variants of the selected folder, while child
        /// folders remain their own banks.
        #[test]
        fn folder_banks_groups_direct_files_under_the_selected_folder() {
            let directory = tempfile::tempdir().expect("temp dir");
            std::fs::write(directory.path().join("take 1.mp3"), b"one").expect("sample");
            std::fs::write(directory.path().join("take 2.mp3"), b"two").expect("sample");
            let kit = directory.path().join("kit");
            std::fs::create_dir_all(&kit).expect("bank folder");
            std::fs::write(kit.join("snare.mp3"), b"snarebytes").expect("sample");
            let root = directory.path().canonicalize().expect("canonical root");
            let banks = folder_banks(&root, &[]).expect("walk");
            let root_name = root.file_name().unwrap().to_string_lossy();
            let (_, direct_bank @ Bank::Array(direct)) = banks
                .iter()
                .find(|(name, _)| name == root_name.as_ref())
                .expect("selected folder bank")
            else {
                panic!("direct files are an array bank");
            };
            assert_eq!(direct.len(), 2, "both takes are variants of one bank");
            assert_eq!(
                local_variant_names(direct_bank),
                ["take 1", "take 2"],
                "the browser keeps the filesystem stems in variant order"
            );
            assert!(banks.iter().any(|(name, _)| name == "kit"));
        }

        #[test]
        fn a_note_keyed_banks_waveform_uses_the_same_default_note_as_its_preview() {
            let library = SampleLibrary::empty_without_loading();
            let low: Arc<str> = Arc::from("https://samples.example/piano-c2.wav");
            let middle: Arc<str> = Arc::from("https://samples.example/piano-c4.wav");
            library.banks.write().expect("banks").insert(
                "piano".to_owned(),
                Bank::Notes(vec![
                    (36.0, vec![low.clone()]),
                    (60.0, vec![middle.clone()]),
                ]),
            );
            library
                .shared
                .by_url
                .write()
                .expect("sample table")
                .extend([
                    (
                        low.clone(),
                        UrlState::Ready {
                            id: SampleId(7),
                            duration_secs: 1.25,
                        },
                    ),
                    (
                        middle.clone(),
                        UrlState::Ready {
                            id: SampleId(8),
                            duration_secs: 2.5,
                        },
                    ),
                ]);
            library
                .shared
                .shapes
                .lock()
                .expect("sample shapes")
                .remember(low, Arc::from(&[36u8, 18][..]));
            library
                .shared
                .shapes
                .lock()
                .expect("sample shapes")
                .remember(middle, Arc::from(&[60u8, 30][..]));

            let SampleResolution::Found { id, .. } =
                SampleLookup::resolve(&library, "piano", 0.0, 36.0)
            else {
                panic!("preview sample resolves");
            };
            assert_eq!(id, SampleId(7));
            let (shape, duration) = library.sound_shape("piano:0").expect("preview shape");
            assert_eq!(&*shape, &[36, 18], "the picture belongs to the sound heard");
            assert_eq!(duration, 1.25);
        }

        #[test]
        fn a_local_variant_shows_its_index_and_filename_without_the_extension() {
            assert_eq!(
                local_sample_file_stem("file:///music/sessions/vox1.wav").as_deref(),
                Some("vox1")
            );
            let entry = SoundEntry {
                name: "sessions".into(),
                variants: 2,
                variant_names: vec!["take-001".into(), "vox1".into()],
                origin: SoundOrigin::Global,
                category: SoundCategory::Mine,
                location: None,
                import: Some("/music/sessions".into()),
            };
            assert_eq!(entry.variant_label(1), "sessions:1 (vox1)");
            assert!(!entry.variant_label(1).contains(".wav"));
        }

        /// Decode a real Ogg file, when one is pointed at. Works for either codec.
        ///
        /// Only one tiny Vorbis file is committed (`samples/testdata`), and the
        /// Vorbis guard tests decode it on every run; this test stays opt-in for
        /// qualifying real banks:
        /// `RUSTEL_TEST_OGG=/path/to/file.ogg cargo test -p rustel-runtime`.
        /// Point it at a folder's worth in a loop to qualify a whole sample bank.
        #[test]
        fn a_real_ogg_decodes_to_audio() {
            let Ok(path) = std::env::var("RUSTEL_TEST_OGG") else {
                return;
            };
            let bytes = std::fs::read(&path).expect("read the ogg named by RUSTEL_TEST_OGG");
            // The loader never calls a decoder directly, so check the guarded
            // entry point first: reaching the next line at all is the assertion,
            // because an unguarded panic would end the test here the way it would
            // end the loader thread.
            let guarded = decode_guarded(&path, &bytes, Some(48_000));
            assert!(
                guarded.is_ok(),
                "the loader would refuse this file: {}",
                guarded.err().unwrap_or_default()
            );
            let decoded = decode_ogg(&bytes).expect("decode ogg");
            assert!(decoded.frames() > 0, "decoded to no frames");
            assert!(
                (8_000..=192_000).contains(&decoded.sample_rate()),
                "implausible sample rate {}",
                decoded.sample_rate()
            );
            // A file that decodes to digital silence is a decoder that ran without
            // producing audio, which is the failure this catches: it looks like
            // success everywhere else and is inaudible only at the speaker.
            let peak = decoded
                .pcm()
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            assert!(peak > 0.001, "decoded to digital silence: peak {peak}");
        }

        pub(in crate::samples) fn sample_test_shared(next_id: u32) -> Shared {
            Shared {
                by_url: RwLock::new(HashMap::new()),
                speech: Mutex::new(HashMap::new()),
                score_sources: RwLock::new(HashMap::new()),
                ready: Mutex::new(ReadySamples::default()),
                shapes: Mutex::new(SampleShapes::default()),
                next_id: AtomicU32::new(next_id),
                free_ids: Mutex::new(VecDeque::new()),
                jobs: LoadQueue::new(),
                loading: Mutex::new(HashSet::new()),
                caching: Mutex::new(HashSet::new()),
                host_cache: cache_dir(),
                font_base: String::new(),
                source_tables: Mutex::new(SourceTables::default()),
                bank_imports: RwLock::new(HashMap::new()),
                set_banks: RwLock::new(BTreeMap::new()),
                custom_file_names: RwLock::new(HashMap::new()),
                global_file_names: RwLock::new(HashMap::new()),
                fonts: RwLock::new(HashMap::new()),
                renames: RwLock::new(HashMap::new()),
                source_renames: RwLock::new(HashMap::new()),
                auto_aliases: RwLock::new(HashMap::new()),
                global_slots: Mutex::new(Vec::new()),
                global_generation: std::sync::atomic::AtomicU64::new(0),
                global_source_of: RwLock::new(HashMap::new()),
                import_policy: Mutex::new(None),
                font_jobs: FontQueue::new(),
                failures: Mutex::new(Vec::new()),
                direct_diagnostic_logging: AtomicBool::new(true),
                #[cfg(feature = "video")]
                videos: Mutex::new(HashMap::new()),
                score_cache: ScoreCache::new(cache_dir()),
                manifest_pending: AtomicUsize::new(0),
                publication: Arc::new(PublicationGate::new()),
                render_rate: AtomicU32::new(crate::session::DEFAULT_SAMPLE_RATE),
                settled_epoch: AtomicU64::new(0),
            }
        }

        #[test]
        fn sample_id_reservation_stops_at_the_audio_bank_capacity_without_wrapping() {
            let shared = sample_test_shared((SAMPLE_BANK_CAPACITY - 1) as u32);
            assert_eq!(
                reserve_sample_ids(&shared, 1),
                Ok(vec![SampleId((SAMPLE_BANK_CAPACITY - 1) as u32)])
            );
            assert!(reserve_sample_ids(&shared, 1).is_err());
            assert_eq!(
                shared.next_id.load(Ordering::Relaxed),
                SAMPLE_BANK_CAPACITY as u32
            );
        }

        #[test]
        fn cache_to_disk_does_not_reserve_live_bank_slots() {
            let library = SampleLibrary::empty_without_loading();
            let url: Arc<str> = Arc::from("https://example.test/tone.wav");
            library
                .banks
                .write()
                .expect("banks")
                .insert("tone".to_owned(), Bank::Array(vec![url.clone()]));
            library
                .shared
                .next_id
                .store(SAMPLE_BANK_CAPACITY as u32, Ordering::Relaxed);

            assert_eq!(library.cache_to_disk("tone"), 1);
            assert_eq!(
                library.shared.next_id.load(Ordering::Relaxed),
                SAMPLE_BANK_CAPACITY as u32,
                "disk cache must not take a live-bank id"
            );
            assert!(
                library
                    .shared
                    .by_url
                    .read()
                    .expect("url table")
                    .get(&url)
                    .is_none(),
                "disk cache must not publish Loading or Ready"
            );
            let job = library.shared.jobs.try_pop().expect("cache job queued");
            assert_eq!(&*job.url, &*url);
            assert!(
                matches!(job.kind, LoadKind::Cache),
                "cache job must not ask the decoder to seat PCM"
            );
            assert!(
                library
                    .shared
                    .caching
                    .lock()
                    .expect("disk-cache claims")
                    .contains(&url)
            );
            assert_eq!(library.cache_to_disk("tone"), 1);
            assert!(
                library.shared.jobs.try_pop().is_none(),
                "a second pass must not enqueue the same url twice"
            );
        }

        #[test]
        fn cache_to_disk_queues_font_files_instead_of_decoding_zones() {
            let library = SampleLibrary::with_background_loaders_at(
                HashMap::new(),
                Vec::new(),
                HashMap::from([(
                    "gm_test".to_owned(),
                    vec![Arc::from("font0"), Arc::from("font1")],
                )]),
                "https://example.test/sound".to_owned(),
                cache_dir(),
                Loading::NotStarted,
            )
            .expect("library");
            library
                .shared
                .next_id
                .store(SAMPLE_BANK_CAPACITY as u32, Ordering::Relaxed);

            assert_eq!(library.cache_to_disk("gm_test"), 2);
            assert_eq!(
                library.shared.next_id.load(Ordering::Relaxed),
                SAMPLE_BANK_CAPACITY as u32
            );
            assert!(
                library.shared.font_jobs.try_pop().is_none(),
                "font decode must not run during a disk cache"
            );
            let mut queued: Vec<_> = std::iter::from_fn(|| library.shared.jobs.try_pop())
                .map(|job| {
                    assert!(matches!(job.kind, LoadKind::Cache));
                    job.url.to_string()
                })
                .collect();
            queued.sort();
            assert_eq!(
                queued,
                [
                    "https://example.test/sound/font0.js".to_owned(),
                    "https://example.test/sound/font1.js".to_owned()
                ]
            );
            assert!(library.shared.fonts.read().expect("fonts").is_empty());
        }

        /// `join_url` puts the missing `Membranophones/` parent back on VCSL
        /// entries that start with `Struck Membranophones/` (encoded or not), and
        /// rewrites no other entry.
        #[test]
        fn vcsl_tom_mallet_paths_get_their_membranophones_parent() {
            let base = "https://strudel.b-cdn.net/VCSL/";
            assert_eq!(
                join_url(
                    base,
                    "Struck%20Membranophones/Tom%201/Mallet/TomH_HitM_v2_rr1_Mid.wav"
                ),
                "https://strudel.b-cdn.net/VCSL/Membranophones/Struck%20Membranophones/Tom%201/Mallet/TomH_HitM_v2_rr1_Mid.wav"
            );
            assert_eq!(
                join_url(
                    base,
                    "Membranophones/Struck%20Membranophones/Tom%202/Stick/x.wav"
                ),
                "https://strudel.b-cdn.net/VCSL/Membranophones/Struck%20Membranophones/Tom%202/Stick/x.wav",
                "paths that already have the parent stay put"
            );
            assert_eq!(
                join_url(
                    "https://strudel.b-cdn.net/piano/",
                    "Struck%20Membranophones/x.wav"
                ),
                "https://strudel.b-cdn.net/piano/Struck%20Membranophones/x.wav",
                "only VCSL is rewritten"
            );
        }

        /// The shipped packs as the Sources page lists them: every pin that
        /// brings files, the inline banks under their folder's name, the fonts
        /// last, and no alias-only pin, which has nothing to count or cache.
        #[test]
        fn the_shipped_packs_are_the_pins_that_bring_files() {
            let sources = SampleLibrary::default_sources();
            let names: Vec<&str> = sources.iter().map(|source| source.name.as_str()).collect();
            assert!(names.contains(&"piano"), "{names:?}");
            assert!(names.contains(&"Dirt-Samples"), "{names:?}");
            assert_eq!(names.last(), Some(&"gm soundfonts"));
            assert!(
                !names.contains(&"tidal-drum-machines-alias"),
                "an alias map brings no files: {names:?}"
            );
            for source in sources {
                assert!(
                    source.url.starts_with("https://"),
                    "a shipped pack lives on the network: {}",
                    source.url
                );
                assert!(!source.base.is_empty());
            }
            assert_eq!(
                default_sources_from("not json", "{}"),
                Vec::<DefaultSource>::new()
            );
        }

        #[test]
        fn a_verified_manifest_cache_seeds_the_catalogue_synchronously() {
            let cache = tempfile::tempdir().expect("manifest cache");
            let url = "https://example.test/instant.json";
            let bytes = br#"{"Instant":["kick.wav","snare.wav"]}"#;
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            let sha256 = hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            std::fs::write(cache_path(cache.path(), url), bytes).expect("cached manifest");
            let source = PinnedSource {
                name: "instant".into(),
                url: url.into(),
                base: Some("https://example.test/audio/".into()),
                sha256,
                category: Some(SoundCategory::Other),
            };
            let mut banks = HashMap::new();

            seed_cached_default_manifests(&mut banks, &[source], cache.path());

            assert!(banks.contains_key("Instant"));
            assert!(
                banks.contains_key("instant"),
                "case-insensitive lookup is ready too"
            );
        }

        /// Dirt-Samples is inline in the pin file, not a fetched manifest.
        /// Its row must still read how many sounds and files it holds, and
        /// survive a refresh-all that clears remote pack banks first.
        #[test]
        fn inline_dirt_samples_are_counted_and_restored_on_refresh() {
            let dirt = SampleLibrary::default_sources()
                .iter()
                .find(|source| source.name == "Dirt-Samples")
                .cloned()
                .expect("Dirt-Samples is a shipped pack");
            let library = SampleLibrary::empty_without_loading();
            let (sounds, files) = library.default_source_holds(&dirt);
            assert!(sounds > 0, "the pin file lists inline banks");
            assert!(files > 0, "each bank names wav files");
            {
                let mut banks = library.banks.write().expect("banks");
                banks.clear();
            }
            let (sounds, files) = library.default_source_holds(&dirt);
            assert!(sounds > 0, "counts fall back to the pin file");
            assert!(files > 0);
            library
                .refresh_default_sources()
                .expect("refresh enqueues remote manifests");
            let (sounds, files) = library.default_source_holds(&dirt);
            assert!(sounds > 0, "inline banks are put back after refresh");
            assert!(files > 0);
        }

        /// A pack's files are counted once each however many names reach
        /// them, only the ones under its base are its own, and a second
        /// caching pass queues nothing - the row shows the rest as here.
        #[test]
        fn a_shipped_pack_counts_its_distinct_files_and_queues_them_once() {
            let library = SampleLibrary::empty_without_loading();
            let kick: Arc<str> = Arc::from("https://example.test/drums/kick.wav");
            let snare: Arc<str> = Arc::from("https://example.test/drums/snare.wav");
            {
                let mut banks = library.banks.write().expect("banks");
                banks.insert(
                    "Drums_bd".to_owned(),
                    Bank::Array(vec![kick.clone(), snare.clone()]),
                );
                // The case-insensitive copy: the same files, another name.
                banks.insert(
                    "drums_bd".to_owned(),
                    Bank::Array(vec![kick.clone(), snare.clone()]),
                );
                banks.insert(
                    "elsewhere".to_owned(),
                    Bank::Array(vec![Arc::from("https://other.test/x.wav")]),
                );
            }
            let pack = DefaultSource {
                name: "drums".to_owned(),
                url: "https://example.test/drums.json".to_owned(),
                base: "https://example.test/drums".to_owned(),
            };
            assert_eq!(library.default_source_holds(&pack), (1, 2));
            assert_eq!(library.pending_cache_under(&pack.base), 0);

            let asked = library.cache_default_source(&pack);
            assert_eq!(
                asked,
                CacheRequest {
                    sounds: 1,
                    files: 2,
                    queued: 2
                }
            );
            assert_eq!(library.pending_cache_under(&pack.base), 2);
            assert_eq!(
                library.pending_cache_under("https://other.test/"),
                0,
                "another pack's files are not this one's count"
            );
            let again = library.cache_default_source(&pack);
            assert_eq!(again.queued, 0, "claimed files are not queued twice");
            assert_eq!(again.files, 2, "but they are still the pack's files");
            let queued = std::iter::from_fn(|| library.shared.jobs.try_pop()).count();
            assert_eq!(queued, 2);
        }

        /// Only a lowercase lookup copy with the same ordered files is
        /// excluded from the sound count; distinct names remain sounds.
        #[test]
        fn a_shipped_pack_only_deduplicates_matching_lowercase_copies() {
            let library = SampleLibrary::empty_without_loading();
            let kick: Arc<str> = Arc::from("https://example.test/drums/kick.wav");
            let snare: Arc<str> = Arc::from("https://example.test/drums/snare.wav");
            {
                let mut banks = library.banks.write().expect("banks");
                for (name, files) in [
                    ("Kit", vec![kick.clone(), snare.clone()]),
                    ("KIT", vec![kick.clone()]),
                    ("kit", vec![kick.clone()]),
                    ("Other", vec![kick.clone(), snare.clone()]),
                    ("other", vec![snare.clone(), kick.clone()]),
                    ("alias", vec![kick.clone()]),
                ] {
                    banks.insert(name.to_owned(), Bank::Array(files));
                }
            }
            let pack = DefaultSource {
                name: "drums".to_owned(),
                url: "https://example.test/drums.json".to_owned(),
                base: "https://example.test/drums".to_owned(),
            };
            assert_eq!(library.default_source_holds(&pack), (5, 2));
        }

        /// The first file assigns a keyed bank to its pack, skipping empty keys.
        #[test]
        fn a_shipped_pack_classifies_keyed_banks_by_the_first_file() {
            let library = SampleLibrary::empty_without_loading();
            let kick: Arc<str> = Arc::from("https://example.test/drums/kick.wav");
            let elsewhere: Arc<str> = Arc::from("https://other.test/x.wav");
            {
                let mut banks = library.banks.write().expect("banks");
                for (name, bank) in [
                    (
                        "Kit",
                        Bank::Notes(vec![(60.0, vec![]), (62.0, vec![kick.clone()])]),
                    ),
                    ("kit", Bank::Array(vec![kick.clone()])),
                    ("Foreign", Bank::Array(vec![elsewhere.clone()])),
                    ("foreign", Bank::Array(vec![kick.clone()])),
                    ("empty", Bank::Notes(vec![(60.0, vec![])])),
                    (
                        "outside",
                        Bank::Notes(vec![(60.0, vec![elsewhere]), (62.0, vec![kick.clone()])]),
                    ),
                ] {
                    banks.insert(name.to_owned(), bank);
                }
            }
            let pack = DefaultSource {
                name: "drums".to_owned(),
                url: "https://example.test/drums.json".to_owned(),
                base: "https://example.test/drums".to_owned(),
            };
            assert_eq!(
                library.default_source_files(&pack),
                (2, HashSet::from([kick]))
            );
        }

        /// A user URL pack counts and queues the way a shipped pack does, and
        /// a local folder queues nothing - it is already on this machine.
        #[test]
        fn a_user_url_pack_counts_its_files_and_a_local_folder_does_not_queue() {
            let library = SampleLibrary::empty_without_loading();
            let kick: Arc<str> = Arc::from("https://github.test/kit/kick.wav");
            let snare: Arc<str> = Arc::from("https://github.test/kit/snare.wav");
            {
                let mut slots = library.shared.global_slots.lock().expect("slots");
                slots.push(GlobalSlot {
                    row: 0,
                    spec: "github:me/kit".to_owned(),
                    kind: GlobalKind::Pack,
                    banks: HashMap::from([(
                        "kick".to_owned(),
                        Bank::Array(vec![kick.clone(), snare.clone()]),
                    )]),
                    state: GlobalSourceState::Ready { banks: 1 },
                    rewalk: false,
                });
            }
            library.global.write().expect("global").insert(
                "kick".to_owned(),
                Bank::Array(vec![kick.clone(), snare.clone()]),
            );

            assert!(!source_is_local("github:me/kit"));
            assert!(!source_is_local("https://example.test/pack.json"));
            assert!(library.is_imported_sound("kick"));
            assert!(!library.is_imported_sound("missing"));

            assert_eq!(library.import_source_holds("github:me/kit"), (1, 2));
            let asked = library.cache_import_source("github:me/kit");
            assert_eq!(
                asked,
                CacheRequest {
                    sounds: 1,
                    files: 2,
                    queued: 2
                }
            );
            assert_eq!(library.pending_cache_for_import("github:me/kit"), 2);
            let again = library.cache_import_source("github:me/kit");
            assert_eq!(again.queued, 0, "claimed files are not queued twice");

            let local = if cfg!(windows) {
                "C:\\Users\\me\\drums"
            } else {
                "/tmp/drums"
            };
            assert!(source_is_local(local));
            let local_asked = library.cache_import_source(local);
            assert_eq!(local_asked.queued, 0, "a folder on disk is not downloaded");
        }

        /// The soundfonts are one pack: the files are the `.js` under the
        /// font base, once each across every instrument that shares them.
        #[test]
        fn the_soundfonts_are_one_shipped_pack() {
            let library = SampleLibrary::with_background_loaders_at(
                HashMap::new(),
                Vec::new(),
                HashMap::from([
                    (
                        "gm_piano".to_owned(),
                        vec![Arc::from("font0"), Arc::from("font1")],
                    ),
                    ("gm_organ".to_owned(), vec![Arc::from("font1")]),
                ]),
                "https://example.test/sound".to_owned(),
                cache_dir(),
                Loading::NotStarted,
            )
            .expect("library");
            let fonts = DefaultSource {
                name: "gm soundfonts".to_owned(),
                url: "https://example.test/sound".to_owned(),
                base: "https://example.test/sound".to_owned(),
            };
            assert_eq!(library.default_source_holds(&fonts), (2, 2));
            let asked = library.cache_default_source(&fonts);
            assert_eq!(
                asked,
                CacheRequest {
                    sounds: 2,
                    files: 2,
                    queued: 2
                }
            );
            assert_eq!(library.pending_cache_under(&fonts.base), 2);
            assert!(library.loading_under(&fonts.base).is_none());
            library
                .shared
                .loading
                .lock()
                .expect("loading")
                .insert(Arc::from("https://example.test/sound/font1.js"));
            assert_eq!(
                library.loading_under(&fonts.base).as_deref(),
                Some("font1.js")
            );
            assert!(
                library
                    .loading_under("https://example.test/drums/")
                    .is_none()
            );
            // Several workers can hold claimed files at once; each still keeps
            // its claim live after the queue is drained.
            while library.shared.jobs.try_pop().is_some() {}
            {
                let mut loading = library.shared.loading.lock().expect("loading");
                loading.clear();
                loading.insert(Arc::from("https://example.test/sound/font0.js"));
                loading.insert(Arc::from("https://example.test/sound/font1.js"));
            }
            assert_eq!(library.pending_cache_under(&fonts.base), 2);
            assert!(library.loading_now().is_some());
        }

        pub(in crate::samples) fn two_zone_test_font() -> String {
            // Base64 of 4 s16le frames (raw `sample` zone payload).
            let raw = base64_encode(&[0u8, 0, 255, 127, 0, 128, 0, 0]);
            format!(
                "console.log('load _tone_test');\nvar _tone_test={{\n\tzones:[\n\t\t{{\n\t\t\tmidi:33\n\t\t\t,originalPitch:3100\n\t\t\t//_tone.comment\n\t\t\t,keyRangeLow:0\n\t\t\t,keyRangeHigh:28\n\t\t\t,loopStart:2\n\t\t\t,loopEnd:3\n\t\t\t,coarseTune:1\n\t\t\t,fineTune:-25\n\t\t\t,sampleRate:22050\n\t\t\t,ahdsr:true\n\t\t\t,sample:'{raw}'\n\t\t}}\n\t\t,{{\n\t\t\tmidi:45\n\t\t\t,originalPitch:4500\n\t\t\t,keyRangeLow:29\n\t\t\t,keyRangeHigh:127\n\t\t\t,loopStart:0\n\t\t\t,loopEnd:0\n\t\t\t,sampleRate:22050\n\t\t\t,sample:'{raw}'\n\t\t}}]\n}};\n"
            )
        }

        #[test]
        fn a_font_that_cannot_fit_publishes_no_orphaned_pcm_prefix() {
            let shared = sample_test_shared((SAMPLE_BANK_CAPACITY - 1) as u32);
            let error = match decode_font(two_zone_test_font().as_bytes(), &shared) {
                Err(error) => error,
                Ok(_) => panic!("two zones cannot fit in one remaining slot"),
            };
            assert!(error.contains("sample bank capacity"), "{error}");
            assert!(shared.ready.lock().expect("ready").samples.is_empty());
            assert_eq!(
                shared.next_id.load(Ordering::Relaxed),
                (SAMPLE_BANK_CAPACITY - 1) as u32
            );
        }

        /// A font's zones each keep an id of their own, so a font decoded after
        /// a release takes what is free before it takes anything fresh.
        #[test]
        fn decode_font_gives_each_zone_its_own_id_from_the_free_list() {
            let shared = sample_test_shared(100);
            shared
                .free_ids
                .lock()
                .expect("free ids")
                .extend([SampleId(3), SampleId(7)]);
            let zones = decode_font(two_zone_test_font().as_bytes(), &shared).expect("font");
            let ids: Vec<SampleId> = zones.iter().map(|zone| zone.id).collect();
            assert_eq!(ids, vec![SampleId(3), SampleId(7)], "released ids first");
            assert_eq!(
                shared.next_id.load(Ordering::Relaxed),
                100,
                "the counter did not move for reissued ids"
            );
            let published: Vec<SampleId> = shared
                .ready
                .lock()
                .expect("ready")
                .samples
                .iter()
                .map(|(id, _)| *id)
                .collect();
            assert_eq!(
                published, ids,
                "every zone's PCM is published under its own id"
            );
        }

        /// 0253_Acoustic_Guitar writes `originalPitch:4200-140`: JavaScript,
        /// which the browser evaluates and JSON cannot. Every gm_acoustic_guitar
        /// preview failed to load on it.
        #[test]
        fn a_font_zone_written_as_arithmetic_still_decodes() {
            assert_eq!(
                fold_number_arithmetic("{\"originalPitch\":4200-140}"),
                "{\"originalPitch\":4060}"
            );
            assert_eq!(
                fold_number_arithmetic("{\"a\":6000+50-25,\"b\":-20}"),
                "{\"a\":6025,\"b\":-20}"
            );
            assert_eq!(
                fold_number_arithmetic("{\"file\":\"ab12+3-4cd\",\"x\":1}"),
                "{\"file\":\"ab12+3-4cd\",\"x\":1}",
                "a base64 payload is not arithmetic"
            );
            let font = two_zone_test_font().replace("originalPitch:3100", "originalPitch:3240-140");
            let shared = sample_test_shared(1);
            let zones = decode_font(font.as_bytes(), &shared).expect("decode");
            assert_eq!(zones[0].base_detune, 3100.0 - 100.0 - (-25.0));
        }

        /// Decode real webaudiofontdata files from disk: a developer's check
        /// against the fonts the CDN actually serves. `RUSTEL_FONT_PROBE` is a
        /// colon-separated list of `.js` paths.
        #[test]
        #[ignore]
        fn probe_real_fonts_from_disk() {
            let Ok(paths) = std::env::var("RUSTEL_FONT_PROBE") else {
                return;
            };
            for path in paths.split(':').filter(|path| !path.is_empty()) {
                let bytes = std::fs::read(path).expect("font file");
                let shared = sample_test_shared(1);
                match decode_font(&bytes, &shared) {
                    Ok(zones) => eprintln!("{path}: {} zones", zones.len()),
                    Err(error) => panic!("{path}: {error}"),
                }
            }
        }

        #[test]
        fn decode_font_parses_webaudiofontdata_zones_and_loop_rule() {
            let font = two_zone_test_font();
            let shared = sample_test_shared(1);
            let zones = decode_font(font.as_bytes(), &shared).expect("decode");
            assert_eq!(zones.len(), 2);
            assert_eq!(zones[0].id, SampleId(1));
            assert_eq!(zones[1].id, SampleId(2));
            assert_eq!(shared.next_id.load(Ordering::Relaxed), 3);
            // baseDetune = originalPitch − 100·coarseTune − fineTune.
            assert_eq!(zones[0].base_detune, 3100.0 - 100.0 - (-25.0));
            // `loop = loopStart > 1 && loopStart < loopEnd`, seconds of the
            // zone's own timeline.
            assert_eq!(zones[0].loop_secs, Some((2.0 / 22_050.0, 3.0 / 22_050.0)));
            assert_eq!(zones[1].loop_secs, None, "loopStart 0 does not loop");
            assert_eq!(zones[0].key_lo, 0.0);
            assert_eq!(zones[0].key_hi, 28.0);
            // WebAudioFontPlayer divides s16 by 65536, not 32768.
            let ready = shared.ready.lock().expect("ready");
            assert_eq!(ready.samples.len(), 2);
            for (id, decoded) in &ready.samples {
                assert_eq!(ready.identity(*id), Some(decoded.identity()));
            }
            let pcm = ready.samples[0].1.pcm();
            assert_eq!(pcm[0], 0.0);
            assert!((pcm[1] - 32767.0 / 65536.0).abs() < 1e-6);
            assert!((pcm[2] + 0.5).abs() < 1e-6);
        }

        pub(in crate::samples) fn base64_encode(bytes: &[u8]) -> String {
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let b = [
                    chunk[0],
                    *chunk.get(1).unwrap_or(&0),
                    *chunk.get(2).unwrap_or(&0),
                ];
                let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                out.push(ALPHABET[(n >> 18) as usize & 63] as char);
                out.push(ALPHABET[(n >> 12) as usize & 63] as char);
                out.push(if chunk.len() > 1 {
                    ALPHABET[(n >> 6) as usize & 63] as char
                } else {
                    '='
                });
                out.push(if chunk.len() > 2 {
                    ALPHABET[n as usize & 63] as char
                } else {
                    '='
                });
            }
            out
        }

        /// A bank answers to its own name in any case, so `.bank('Rolandmc303')`
        /// finds `RolandMC303`.
        #[test]
        fn a_bank_answers_to_its_name_in_any_case() {
            let mut banks = HashMap::new();
            banks.insert(
                "RolandMC303_bd".to_string(),
                Bank::Array(vec![std::sync::Arc::from("mc303/bd.wav")]),
            );
            banks.insert(
                "lowercase_already".to_string(),
                Bank::Array(vec![std::sync::Arc::from("plain/a.wav")]),
            );
            let before = banks.len();
            expand_case_insensitive(&mut banks);

            assert!(
                banks.contains_key("rolandmc303_bd"),
                "the name lowercased answers too"
            );
            assert!(
                banks.contains_key("RolandMC303_bd"),
                "and the catalogue's own spelling is kept"
            );
            assert_eq!(
                banks.len(),
                before + 1,
                "a name already lowercase gains no duplicate"
            );

            // A bank that really does differ only by case keeps whichever it
            // registered first rather than being quietly replaced.
            let mut collide = HashMap::new();
            collide.insert(
                "abc".to_string(),
                Bank::Array(vec![std::sync::Arc::from("first.wav")]),
            );
            collide.insert(
                "ABC".to_string(),
                Bank::Array(vec![std::sync::Arc::from("second.wav")]),
            );
            expand_case_insensitive(&mut collide);
            assert!(matches!(
                collide.get("abc"),
                Some(Bank::Array(urls)) if urls[0].as_ref() == "first.wav"
            ));
        }

        #[test]
        fn imported_and_score_banks_answer_case_insensitively_without_catalogue_duplicates() {
            let library = SampleLibrary::empty_without_loading();
            library.global.write().expect("global banks").insert(
                "MyImportedKit".into(),
                Bank::Array(vec![Arc::from("file:///imported.wav")]),
            );
            library.custom.write().expect("custom banks").insert(
                "MyScoreKit".into(),
                Bank::Array(vec![Arc::from("https://example.test/score.wav")]),
            );

            assert!(library.knows("myimportedkit"));
            assert!(library.knows("MYIMPORTEDKIT"));
            assert!(library.knows("myscorekit"));
            assert!(library.knows("MYSCOREKIT"));
            assert_eq!(
                library
                    .catalogue()
                    .iter()
                    .filter(|entry| entry.name.eq_ignore_ascii_case("MyImportedKit"))
                    .count(),
                1,
                "lookup tolerance does not add a second browser row"
            );
        }

        #[test]
        fn the_catalogue_hides_a_lowercase_lookup_alias() {
            let library = SampleLibrary::empty_without_loading();
            let bank = Bank::Array(vec![Arc::from("https://example.test/kit.wav")]);
            {
                let mut banks = library.banks.write().expect("banks");
                banks.insert("MyKit".into(), bank.clone());
                banks.insert("mykit".into(), bank);
            }

            let shown: Vec<_> = library
                .catalogue()
                .into_iter()
                .filter(|entry| entry.name.eq_ignore_ascii_case("MyKit"))
                .collect();
            assert_eq!(shown.len(), 1);
            assert_eq!(shown[0].name, "MyKit");
            assert!(library.knows("mykit"), "the hidden spelling still resolves");
        }

        #[test]
        fn alias_bank_expansion_matches_the_alias_bank_map() {
            let mut banks = HashMap::new();
            banks.insert(
                "RolandTR909_bd".to_string(),
                Bank::Array(vec![std::sync::Arc::from("tr909/bd.wav")]),
            );
            banks.insert(
                "plain".to_string(),
                Bank::Array(vec![std::sync::Arc::from("plain/a.wav")]),
            );
            // The alias JSON maps CANONICAL -> alias ("RolandTR909": "TR909");
            // the registered alias key is lowercased.
            expand_bank_aliases(
                &mut banks,
                &[("RolandTR909".to_string(), "TR909".to_string())],
            );
            assert!(banks.contains_key("tr909_bd"), "lowercased alias key");
            assert!(banks.contains_key("RolandTR909_bd"), "original key kept");
            assert!(
                !banks.keys().any(|k| k != "plain" && k.starts_with("plain")),
                "suffix-less sounds are not aliased"
            );
        }
    }
}
