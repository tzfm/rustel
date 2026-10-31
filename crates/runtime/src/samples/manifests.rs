use super::*;

impl ManifestContext {
    fn failure(&self, failure: impl Into<SampleFailure>) {
        self.shared
            .failures
            .lock()
            .expect("sample failures")
            .push(failure.into());
    }

    fn register_custom(
        &self,
        map_json: &str,
        base: Option<&str>,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        budget.check()?;
        let value: serde_json::Value = serde_json::from_str(map_json)
            .map_err(|error| format!("samples() map does not parse: {error}"))?;
        budget.check()?;
        self.register_custom_value(&value, base, None, 0, budget)
    }

    fn register_score_custom(
        &self,
        map_json: &str,
        base: Option<&str>,
        access: &ScoreSampleAccess,
        budget: &sample_fetch::FetchBudget,
        layer: BankLayer,
    ) -> Result<(), String> {
        budget.check()?;
        if access.is_denied() {
            return Err(
                "score-level samples() cannot access files or the network without a host grant"
                    .to_owned(),
            );
        }
        if map_json.len() > MAX_SCORE_SAMPLE_MAP_BYTES {
            return Err(format!(
                "samples() map exceeds the {MAX_SCORE_SAMPLE_MAP_BYTES} byte limit"
            ));
        }
        let value: serde_json::Value = serde_json::from_str(map_json)
            .map_err(|error| format!("samples() map does not parse: {error}"))?;
        budget.check()?;
        self.register_score_custom_value(&value, base, None, access, 0, budget, None, layer)
    }

    /// `import` is the string the score wrote - `github:user/repo` - when
    /// this map came of one, for the banks it brings to be filed under.
    #[allow(clippy::too_many_arguments)]
    fn register_score_custom_value(
        &self,
        value: &serde_json::Value,
        base: Option<&str>,
        fallback_base: Option<&str>,
        access: &ScoreSampleAccess,
        depth: usize,
        budget: &sample_fetch::FetchBudget,
        import: Option<&str>,
        layer: BankLayer,
    ) -> Result<(), String> {
        budget.check()?;
        if depth > 2 {
            return Err("samples() map recursion is too deep".to_owned());
        }
        match value {
            serde_json::Value::String(source) => {
                let written = source.as_str();
                let url = match read_source(source, GITHUB_SAMPLE_MANIFEST) {
                    SampleSource::LocalFolder(rest) => {
                        return self.register_score_local_folder(
                            rest.trim(),
                            access,
                            budget,
                            Some(written),
                            layer,
                        );
                    }
                    SampleSource::Url(url) => url,
                };
                let (url, fetch_access) = access.approve_remote(&url)?;
                // Parsing IS the validation: an error page served with status
                // 200 must not become this URL's permanent manifest.
                let fetched: serde_json::Value = fetch_score_source_cached_with_budget(
                    &self.shared.score_cache,
                    &url,
                    &fetch_access,
                    ScoreCacheKind::Manifest,
                    MAX_SCORE_SAMPLE_MAP_BYTES,
                    CacheFetchScope {
                        budget,
                        publication: &self.shared.publication,
                    },
                    |bytes| {
                        serde_json::from_str(
                            std::str::from_utf8(bytes)
                                .map_err(|error| format!("{url}: not UTF-8: {error}"))?,
                        )
                        .map_err(|error| format!("{url}: not JSON: {error}"))
                    },
                )?;
                budget.check()?;
                // The directory the manifest came from is only a fallback base.
                // A fetched map can name its own `_base`: the shabda maps are
                // relative to one, and it is not their own directory. The base
                // the score wrote outranks both. The explicit base and the
                // fallback are separate parameters, because one `Option` cannot
                // tell an explicit base from a derived one.
                //
                // This does not widen access. `join_url` returns an entry that
                // is a whole URL unchanged, so a map can already point a sound
                // at any host. Each address still goes through `approve_remote`
                // under the score's policy.
                self.register_score_custom_value(
                    &fetched,
                    base,
                    Some(&base_url(&url)),
                    access,
                    depth + 1,
                    budget,
                    Some(import.unwrap_or(written)),
                    layer,
                )
            }
            serde_json::Value::Object(map) => {
                if map.len() > SAMPLE_BANK_CAPACITY - 1 {
                    return Err(format!(
                        "samples() map exceeds the {}-entry limit",
                        SAMPLE_BANK_CAPACITY - 1
                    ));
                }
                // What the score wrote, then what the map says of itself,
                // then where the map was fetched from.
                let map_base = map.get("_base").and_then(serde_json::Value::as_str);
                let effective_base = base
                    .filter(|base| !base.is_empty())
                    .or(map_base.filter(|base| !base.is_empty()))
                    .or(fallback_base)
                    .unwrap_or("");
                let effective_base = expand_base(effective_base);
                let mut staged = Vec::new();
                staged
                    .try_reserve(map.len())
                    .map_err(|_| "not enough host memory for sample banks".to_owned())?;
                let mut sources = Vec::new();
                let mut unsupported = Vec::new();
                unsupported
                    .try_reserve(map.len())
                    .map_err(|_| "not enough host memory for sample diagnostics".to_owned())?;
                for (name, entry) in map {
                    budget.check()?;
                    if name == "_base" {
                        continue;
                    }
                    let entry_base = entry
                        .get("_base")
                        .and_then(serde_json::Value::as_str)
                        .map(expand_base);
                    let base = entry_base.as_deref().unwrap_or(&effective_base);
                    match parse_score_bank(entry, base, access, &mut sources)? {
                        Some(bank) => staged.push((name.clone(), bank)),
                        None => unsupported
                            .push(format!("samples() entry {name:?} has an unsupported shape")),
                    }
                }
                // A source that answered with a well-formed map and no
                // sounds in it. Shabda does exactly this when a word finds
                // nothing: `{"_base": "…"}` on its own, HTTP 200. Registering
                // that as a success and saying nothing leaves you looking for
                // a sound the browser never had.
                if staged.is_empty()
                    && unsupported.is_empty()
                    && let Some(import) = import
                {
                    unsupported.push(format!("samples({import:?}) brought no sounds"));
                }
                self.publish_score_custom(sources, staged, unsupported, budget, import, layer)
            }
            other => Err(format!(
                "samples() expects a map or source string, got {other}"
            )),
        }
    }

    fn publish_score_custom(
        &self,
        sources: Vec<(Arc<str>, ScoreFetchAccess)>,
        staged: Vec<(String, Bank)>,
        unsupported: Vec<String>,
        budget: &sample_fetch::FetchBudget,
        import: Option<&str>,
        layer: BankLayer,
    ) -> Result<(), String> {
        budget.check()?;
        let limit = MAX_SCORE_SAMPLE_FILES;
        let _publication = self
            .shared
            .publication
            .enter(PublicationKind::Custom, budget)?;
        let mut source_table = self
            .shared
            .score_sources
            .write()
            .expect("score sample sources");
        // The grant table above is shared: a URL is reachable or it is not,
        // whoever asked. Only the names differ by layer.
        let additional_sources = sources
            .iter()
            .enumerate()
            .filter(|(index, (url, _))| {
                !source_table.contains_key(url.as_ref())
                    && !sources[..*index].iter().any(|(earlier, _)| earlier == url)
            })
            .count();
        if source_table.len().saturating_add(additional_sources) > limit {
            return Err(format!(
                "score sample registrations exceed the {limit}-source session limit"
            ));
        }
        if let BankLayer::Global { generation } = layer {
            // Publish URL restrictions before banks, as below.
            source_table.extend(sources);
            drop(source_table);
            // A pack fills its own row and the imported layer is read again
            // from every row, so a later row still wins a name whichever
            // kind it is and however long its list took to arrive. None of
            // the score-layer bookkeeping below applies: an imported name
            // is never the set folder's and never a score import's.
            if fill_global_slot(&self.shared, import, generation, staged) {
                rebuild_global(&self.shared, &self.global);
            }
            self.shared
                .failures
                .lock()
                .expect("sample failures")
                .extend(unsupported.into_iter().map(SampleFailure::from));
            return Ok(());
        }
        let mut custom = self.custom.write().expect("custom banks");
        let additional_banks = staged
            .iter()
            .enumerate()
            .filter(|(index, (name, _))| {
                !custom.contains_key(name.as_str())
                    && !staged[..*index].iter().any(|(earlier, _)| earlier == name)
            })
            .count();
        if custom.len().saturating_add(additional_banks) > limit {
            return Err(format!(
                "score sample registrations exceed the {limit}-bank session limit"
            ));
        }

        // Publish URL restrictions before banks. A concurrent resolver can
        // therefore never observe a score URL without the grant attached to
        // it. The publication gate makes both updates precede cancellation.
        source_table.extend(sources);
        drop(source_table);
        {
            // Which import each bank came of, for the browser; a bank an
            // inline map redefines is the map's now, not the import's.
            let mut imports = self.shared.bank_imports.write().expect("bank imports");
            for (name, _) in &staged {
                match import {
                    Some(import) => {
                        imports.insert(name.clone(), Arc::from(import));
                    }
                    None => {
                        imports.remove(name);
                    }
                }
            }
        }
        {
            // The same for a name the open set's folder had taken up: the
            // score has redefined it, so it is the score's now - the browser
            // says whose it is, and the next set leaves it alone rather than
            // putting the set's old bank back over it.
            let mut of_set = self.shared.set_banks.write().expect("set banks");
            for (name, _) in &staged {
                of_set.remove(name);
            }
        }
        custom.extend(staged);
        *self
            .shared
            .custom_file_names
            .write()
            .expect("custom filenames") = local_names::index(custom.values());
        drop(custom);
        self.shared
            .failures
            .lock()
            .expect("sample failures")
            .extend(unsupported.into_iter().map(SampleFailure::from));
        Ok(())
    }

    #[allow(
        clippy::print_stderr,
        reason = "registration records are the direct diagnostic channel"
    )]
    fn register_score_local_folder(
        &self,
        requested: &str,
        access: &ScoreSampleAccess,
        budget: &sample_fetch::FetchBudget,
        import: Option<&str>,
        layer: BankLayer,
    ) -> Result<(), String> {
        budget.check()?;
        let allowed_root = access.local_root.as_ref().ok_or_else(|| {
            access.note_local_refused();
            "samples('local:') requires a host-selected local sample root (--allow-local-samples, or the Studio Permissions page)".to_owned()
        })?;
        let allowed_path = allowed_root.path();
        let requested = Path::new(requested);
        if requested.is_absolute()
            || requested.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err("samples('local:') path must stay beneath the permitted root".to_owned());
        }
        let folder = allowed_path
            .join(requested)
            .canonicalize()
            .map_err(|error| {
                format!(
                    "local samples: cannot read {}: {error}",
                    allowed_path.join(requested).display()
                )
            })?;
        if !folder.starts_with(allowed_path) || !folder.is_dir() {
            return Err(format!(
                "local samples folder {} is outside the permitted root {}",
                folder.display(),
                allowed_path.display()
            ));
        }

        let scanned = scan_score_sample_folder(&folder)?;
        budget.check()?;
        let files = scanned.values().map(Vec::len).sum::<usize>();
        if files >= SAMPLE_BANK_CAPACITY {
            return Err(format!(
                "local sample folder has {files} files; limit is {}",
                SAMPLE_BANK_CAPACITY - 1
            ));
        }
        let mut sources = Vec::new();
        sources
            .try_reserve(files)
            .map_err(|_| "not enough host memory for local sample sources".to_owned())?;
        let mut staged = Vec::new();
        staged
            .try_reserve(scanned.len())
            .map_err(|_| "not enough host memory for local sample banks".to_owned())?;
        for (bank, paths) in &scanned {
            let mut urls = Vec::new();
            urls.try_reserve(paths.len())
                .map_err(|_| "not enough host memory for local sample URLs".to_owned())?;
            for relative in paths {
                budget.check()?;
                let path = folder
                    .join(relative)
                    .canonicalize()
                    .map_err(|error| format!("local samples: cannot read {relative}: {error}"))?;
                if !path.starts_with(allowed_path) || !path.is_file() {
                    return Err(format!(
                        "local sample {} is outside the permitted root {}",
                        path.display(),
                        allowed_path.display()
                    ));
                }
                let url = Url::from_file_path(&path)
                    .map_err(|_| format!("cannot represent {} as a file URL", path.display()))?;
                let url: Arc<str> = Arc::from(url.as_str());
                sources.push((
                    url.clone(),
                    ScoreFetchAccess::Local {
                        root: allowed_root.clone(),
                    },
                ));
                urls.push(url);
            }
            staged.push((bank.clone(), Bank::Array(urls)));
        }

        self.publish_score_custom(sources, staged, Vec::new(), budget, import, layer)?;
        let names = self.custom.read().expect("custom banks").len();
        if self
            .shared
            .direct_diagnostic_logging
            .load(Ordering::Acquire)
        {
            eprintln!(
                "{}",
                serde_json::json!({
                    "local_samples": {
                        "folder": folder.display().to_string(),
                        "banks": names,
                        "files": files,
                    }
                })
            );
        }
        Ok(())
    }

    #[allow(
        clippy::print_stderr,
        reason = "registration records are the direct diagnostic channel"
    )]
    fn register_custom_value(
        &self,
        value: &serde_json::Value,
        base: Option<&str>,
        fallback_base: Option<&str>,
        depth: usize,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        budget.check()?;
        if depth > 2 {
            return Err("samples() map recursion is too deep".to_owned());
        }
        match value {
            serde_json::Value::String(source) => {
                let url = match read_source(source, GITHUB_SAMPLE_MANIFEST) {
                    SampleSource::LocalFolder(rest) => {
                        return self.register_local_folder(rest.trim(), base, budget);
                    }
                    SampleSource::Url(url) => url,
                };
                sample_fetch::remote_url_allowed(&url)?;
                let dir = cache_dir();
                let bytes = match fetch_manifest_cached_with_budget(
                    &dir,
                    &url,
                    budget,
                    &self.shared.publication,
                ) {
                    Ok(bytes) => bytes,
                    Err(error) if is_local_sampler(&url) && budget.check().is_ok() => {
                        if self
                            .shared
                            .direct_diagnostic_logging
                            .load(Ordering::Acquire)
                        {
                            eprintln!(
                                "{}",
                                serde_json::json!({
                                    "local_samples": {
                                        "instead_of": url,
                                        "reason": error,
                                        "message": "no sample server answering; reading the folder directly",
                                    }
                                })
                            );
                        }
                        return self.register_local_folder("", base, budget);
                    }
                    Err(error) => return Err(error),
                };
                budget.check()?;
                // A body that is not JSON is evicted, so the next
                // registration fetches it again.
                let fetched: serde_json::Value = std::str::from_utf8(&bytes)
                    .map_err(|error| format!("{url}: not UTF-8: {error}"))
                    .and_then(|text| {
                        serde_json::from_str(text)
                            .map_err(|error| format!("{url}: not JSON: {error}"))
                    })
                    .inspect_err(|_| evict_host_cache_entry(&dir, &url))?;
                budget.check()?;
                // The same three-way order as the score path. The pinned
                // banks do not come through here - `load_default_manifests`
                // parses those itself, against the base each source names in
                // `sample-banks.json` - so this reaches trusted `samples()`
                // registrations only.
                self.register_custom_value(&fetched, base, Some(&base_url(&url)), depth + 1, budget)
            }
            serde_json::Value::Object(map) => {
                let map_base = map.get("_base").and_then(serde_json::Value::as_str);
                let effective_base = base
                    .filter(|base| !base.is_empty())
                    .or(map_base.filter(|base| !base.is_empty()))
                    .or(fallback_base)
                    .unwrap_or("");
                let effective_base = expand_base(effective_base);
                let mut parsed = Vec::new();
                parsed
                    .try_reserve(map.len())
                    .map_err(|_| "samples() map exceeds host memory".to_owned())?;
                for (name, entry) in map {
                    budget.check()?;
                    if name == "_base" {
                        continue;
                    }
                    let entry_base = entry
                        .get("_base")
                        .and_then(serde_json::Value::as_str)
                        .map(expand_base);
                    let base = entry_base.as_deref().unwrap_or(&effective_base);
                    match parse_bank(entry, base) {
                        Some(bank) => match check_bank_urls(&bank) {
                            Ok(()) => parsed.push((name.clone(), bank)),
                            Err(why) => {
                                self.failure(format!("samples() entry {name:?} was refused: {why}"))
                            }
                        },
                        None => self
                            .failure(format!("samples() entry {name:?} has an unsupported shape")),
                    }
                }
                let _publication = self
                    .shared
                    .publication
                    .enter(PublicationKind::Custom, budget)?;
                // Same reasoning as the score path: a well-formed map with
                // nothing in it is not a success worth staying quiet about.
                if parsed.is_empty() && !map.is_empty() {
                    self.failure("samples() source brought no sounds".to_owned());
                }
                let mut registered = self.custom.write().expect("custom banks");
                for (name, bank) in parsed {
                    registered.insert(name, bank);
                }
                *self
                    .shared
                    .custom_file_names
                    .write()
                    .expect("custom filenames") = local_names::index(registered.values());
                Ok(())
            }
            other => Err(format!(
                "samples() expects a map or source string, got {other}"
            )),
        }
    }

    #[allow(
        clippy::print_stderr,
        reason = "registration records are the direct diagnostic channel"
    )]
    fn register_local_folder(
        &self,
        requested: &str,
        base: Option<&str>,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        budget.check()?;
        let root = if !requested.is_empty() {
            PathBuf::from(requested)
        } else if let Some(dir) = std::env::var_os(product::LOCAL_SAMPLES_ENV) {
            PathBuf::from(dir)
        } else if let Some(dir) = base.filter(|base| !base.is_empty()) {
            PathBuf::from(dir)
        } else {
            std::env::current_dir().map_err(|error| format!("no working directory: {error}"))?
        };
        let root = root
            .canonicalize()
            .map_err(|error| format!("local samples: cannot read {}: {error}", root.display()))?;
        let scanned = scan_sample_folder(&root)?;
        budget.check()?;
        let files = scanned.values().map(Vec::len).sum::<usize>();
        let mut parsed = Vec::new();
        parsed
            .try_reserve_exact(scanned.len())
            .map_err(|_| "local sample map exceeds host memory".to_owned())?;
        for (bank, paths) in &scanned {
            let mut urls = Vec::new();
            urls.try_reserve_exact(paths.len())
                .map_err(|_| "local sample map exceeds host memory".to_owned())?;
            for path in paths {
                budget.check()?;
                urls.push(Arc::from(local_file_url(&root.join(path)).as_str()));
            }
            parsed.push((bank.clone(), Bank::Array(urls)));
        }
        let names = {
            let _publication = self
                .shared
                .publication
                .enter(PublicationKind::Local, budget)?;
            let mut custom = self.custom.write().expect("custom banks");
            for (name, bank) in parsed {
                custom.insert(name, bank);
            }
            *self
                .shared
                .custom_file_names
                .write()
                .expect("custom filenames") = local_names::index(custom.values());
            custom.len()
        };
        if self
            .shared
            .direct_diagnostic_logging
            .load(Ordering::Acquire)
        {
            eprintln!(
                "{}",
                serde_json::json!({
                    "local_samples": {
                        "folder": root.display().to_string(),
                        "banks": names,
                        "files": files,
                    }
                })
            );
        }
        Ok(())
    }

    fn look_up<T>(&self, name: &str, take: impl FnOnce(Named<'_>) -> T) -> Option<T> {
        look_up_named(
            &self.custom,
            &self.global,
            &self.gm,
            &self.banks,
            &self.shared,
            name,
            take,
        )
    }

    /// Whether some bank holds this name, i.e. whether `{bank}_{name}` exists.
    fn name_belongs_to_a_bank(&self, name: &str) -> bool {
        let suffix = format!("_{name}");
        let custom = self.custom.read().expect("custom banks");
        let global = self.global.read().expect("global banks");
        let banks = self.banks.read().expect("default banks");
        custom
            .keys()
            .chain(global.keys())
            .chain(banks.keys())
            .any(|key| key.ends_with(&suffix))
    }

    pub(super) fn prefetch_known(&self, spec: &str, priority: LoadPriority) -> PrefetchStatus {
        self.prefetch_known_with_intent(spec, priority, PrefetchIntent::Explicit)
    }

    pub(super) fn prefetch_known_with_intent(
        &self,
        spec: &str,
        priority: LoadPriority,
        intent: PrefetchIntent,
    ) -> PrefetchStatus {
        let spec = spec.trim();
        if spec.is_empty() {
            return PrefetchStatus::Unknown;
        }
        // A synthesised source has nothing to fetch, so there is nothing to
        // preload -- but calling it an "unknown sound" reads as a missing
        // sample and sends people looking for a bank that was never involved.
        // `s("sbd")` and `s("supersaw")` both reported that while playing
        // perfectly well.
        let name = spec.split_once(':').map_or(spec, |(name, _)| name);
        if rustel_voice::is_native_synth_sound(name) {
            return PrefetchStatus::Requested(0);
        }
        let (name, index) = match spec.split_once(':') {
            Some((name, index)) => (name, index.trim().parse::<usize>().ok()),
            None => (spec, None),
        };
        // GM variants use the same numeric index as an evaluated `n`, including
        // fractional rounding and negative wrap. An ordinary bank preload keeps
        // its own parsing. A live warm reads a bank's index like a GM index
        // and accepts a list, `name:0,3`, so a text with many variants of a
        // name makes one request per name.
        let chosen = spec.split_once(':').and_then(|(_, index)| {
            let number = |index: &str| index.trim().parse::<f64>().ok().filter(|n| n.is_finite());
            let numbers: Option<Vec<f64>> = if intent == PrefetchIntent::LiveWarm {
                index.split(',').map(number).collect()
            } else {
                number(index).map(|n| vec![n])
            };
            numbers.map(|numbers| {
                crate::sounds::Variants::Only(
                    numbers
                        .into_iter()
                        .map(crate::sounds::Variants::index)
                        .collect(),
                )
            })
        });
        // A soundfont variant is not a sample URL. It is a
        // `{font_base}/{font}.js` file that the font loader fetches. The sample
        // fetcher cannot load the bare font name, which has no scheme.
        let found = self.look_up(name, |named| match named {
            Named::Bank(Bank::Array(bank_urls)) => Warm::Urls(bank_urls.clone()),
            Named::Bank(Bank::Notes(notes)) => Warm::Pitched(
                notes
                    .iter()
                    .flat_map(|(_, note_urls)| note_urls.iter().cloned())
                    .collect(),
            ),
            Named::Font(fonts) => Warm::Fonts(fonts.to_vec()),
        });
        let mut urls = match found {
            Some(Warm::Urls(bank_urls) | Warm::Pitched(bank_urls)) => bank_urls,
            Some(Warm::Fonts(mut fonts)) => {
                if fonts.is_empty() {
                    return PrefetchStatus::Unknown;
                }
                if let Some(chosen) = chosen.or_else(|| {
                    (intent == PrefetchIntent::LiveWarm).then(crate::sounds::Variants::first)
                }) {
                    fonts = picked(&fonts, &chosen).cloned().collect();
                }
                for font in in_queue_order(&fonts, priority) {
                    if intent == PrefetchIntent::CacheDisk {
                        queue_font_cache(&self.shared, font);
                    } else {
                        queue_font(&self.shared, font, priority);
                    }
                }
                return PrefetchStatus::Requested(fonts.len());
            }
            None => Vec::new(),
        };
        if urls.is_empty() {
            // Native generators are valid sounds but have no bytes to warm.
            // Treat them as a resolved zero-file preload so the presentation
            // layer does not report a false "unknown sound" warning.
            if rustel_voice::is_native_synth_sound(name) {
                return PrefetchStatus::Requested(0);
            }
            // A name the score reaches through `.bank(…)` is not a sound on
            // its own: `s("basique").bank("wt_digital")` plays
            // `wt_digital_basique`, and the bank can come from a variable the
            // text scan cannot read. The query-driven warm resolves those
            // correctly; what is left here is a bare name that belongs to
            // SOME bank, and calling it unknown printed an error over a score
            // that plays perfectly. A real typo still belongs to no bank and
            // still reports.
            if self.name_belongs_to_a_bank(name) {
                return PrefetchStatus::Requested(0);
            }
            return PrefetchStatus::Unknown;
        }
        // A live warm reads `name:k,j` as the files `n = k` and `n = j`. A bare
        // name selects `n = 0`, also for a note-keyed bank: `s("piano")` is
        // warmed as `piano:0` and fetches one file, not every key. The note
        // about to play still resolves its own file. A player's
        // `preload("name:k")` reads one index and no list. A bare
        // `preload("name")` also selects `n = 0`. A disk-cache request is the
        // exception: a bare name selects every file, and no file is decoded.
        let chosen = match intent {
            PrefetchIntent::LiveWarm => Some(chosen.unwrap_or_else(crate::sounds::Variants::first)),
            PrefetchIntent::Explicit if spec.split_once(':').is_none() => {
                Some(crate::sounds::Variants::first())
            }
            PrefetchIntent::Explicit | PrefetchIntent::CacheDisk => index.map(|index| {
                crate::sounds::Variants::Only(std::collections::BTreeSet::from([index as i64]))
            }),
        };
        if let Some(chosen) = chosen {
            urls = picked(&urls, &chosen).cloned().collect();
        }
        let decode_rate = DecodeRate::for_sound(name);
        for url in in_queue_order(&urls, priority) {
            if intent == PrefetchIntent::CacheDisk {
                ensure_cached_shared(&self.shared, url);
            } else {
                let _ = ensure_loading_shared(&self.shared, url, decode_rate, priority);
            }
        }
        PrefetchStatus::Requested(urls.len())
    }

    fn prefetch_owned(
        &self,
        preloads: Vec<String>,
        intent: PrefetchIntent,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        for spec in preloads {
            // Starting a loader is itself publication. Holding this gate over
            // the table insertion and queue send makes Drop the precise line:
            // either the request started before cancellation, or it never
            // starts at all.
            let _publication = self
                .shared
                .publication
                .enter(PublicationKind::Preload, budget)?;
            if self.prefetch_known_with_intent(&spec, intent.priority(), intent)
                == PrefetchStatus::Unknown
                && intent.reports_unknown()
            {
                self.failure(format!("preload skipped unknown sound {spec:?}"));
            }
        }
        Ok(())
    }

    /// A map is in: a checker may now hold the score to the names it brought.
    fn note_source(&self, map: String) {
        self.shared.note_source_standing(&map, SourceState::Ready);
    }

    /// A map could not be read: a checker may say so where it was asked for.
    /// True when this is news, false when the map already stood failed for
    /// this reason.
    fn note_source_failed(&self, map: &str, error: &str) -> bool {
        let previous = self
            .shared
            .note_source_standing(map, SourceState::Failed(error.to_owned()));
        let news = !previous
            .is_some_and(|standing| standing.state == SourceState::Failed(error.to_owned()));
        // A Settings pack's map is its spec, quoted. Its row says so; what
        // it brought last time stays, which is kinder than silence.
        if let Ok(spec) = serde_json::from_str::<String>(map) {
            let mut slots = self
                .shared
                .global_slots
                .lock()
                .expect("global source slots");
            if let Some(slot) = slots
                .iter_mut()
                .find(|slot| slot.kind == GlobalKind::Pack && slot.spec == spec)
            {
                slot.state = GlobalSourceState::Failed(error.to_owned());
            }
        }
        news
    }

    fn finish_manifest(&self) {
        // Every load owned by the job starts before this release publication.
        // `wait_until_idle` reads the counter first, then the loader tables, so
        // it cannot observe zero against an older table snapshot.
        self.shared.manifest_pending.fetch_sub(1, Ordering::AcqRel);
        self.shared.note_settled();
    }

    /// A folder walk never reached this row: say why on it, unless the row
    /// has been replaced since the walk was asked for. The folder family's
    /// counterpart to [`Self::note_source_failed`], for the rows a batch
    /// stops in front of rather than the packs it fails one by one.
    fn note_folder_standing(&self, error: &str, spec: &str, generation: u64) {
        let mut slots = self
            .shared
            .global_slots
            .lock()
            .expect("global source slots");
        if self.shared.global_generation.load(Ordering::Acquire) != generation {
            return;
        }
        if let Some(slot) = slots
            .iter_mut()
            .find(|slot| slot.kind == GlobalKind::Folder && slot.spec == spec)
            && slot.state == GlobalSourceState::Loading
        {
            slot.state = GlobalSourceState::Failed(error.to_owned());
        }
    }
}

fn load_default_manifests(
    context: &ManifestContext,
    sources: &[PinnedSource],
    dir: &Path,
    budget: &sample_fetch::FetchBudget,
) -> Result<(), String> {
    let mut banks = HashMap::new();
    let mut aliases = Vec::new();
    let hash_of = |bytes: &[u8]| {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };

    for source in sources {
        budget.check()?;
        let path = cache_path(dir, &source.url);
        let cached = read_manifest_cache(&path).ok();
        budget.check()?;
        let verified = match cached {
            Some(bytes) if hash_of(&bytes) == source.sha256 => bytes,
            _ => {
                let bytes = fetch_manifest_located_with_budget(&source.url, budget)?;
                if hash_of(&bytes) != source.sha256 {
                    return Err(format!(
                        "bank \"{}\" ({}) does not match the installed manifest pin",
                        source.name, source.url
                    ));
                }
                budget.check()?;
                if publish_manifest_cache(&path, &bytes, &context.shared.publication, budget)
                    .is_err()
                {
                    budget.check()?;
                }
                bytes
            }
        };
        let json: serde_json::Value = serde_json::from_slice(&verified)
            .map_err(|error| format!("{}: {error}", source.url))?;
        let object = json
            .as_object()
            .ok_or_else(|| format!("{} is not a JSON object", source.url))?;
        match &source.base {
            None => {
                for (canonical, value) in object {
                    match value {
                        serde_json::Value::String(alias) => {
                            aliases.push((canonical.clone(), alias.clone()));
                        }
                        serde_json::Value::Array(list) => {
                            for alias in list {
                                if let Some(alias) = alias.as_str() {
                                    aliases.push((canonical.clone(), alias.to_owned()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(base) => {
                for (name, entry) in object {
                    budget.check()?;
                    if !name.starts_with('_')
                        && let Some(bank) = parse_bank(entry, base)
                    {
                        banks.insert(name.clone(), bank);
                    }
                }
            }
        }
    }
    budget.check()?;
    expand_bank_aliases(&mut banks, &aliases);
    expand_case_insensitive(&mut banks);
    let _publication = context
        .shared
        .publication
        .enter(PublicationKind::Defaults, budget)?;
    context.banks.write().expect("default banks").extend(banks);
    Ok(())
}

/// Seed the default bank map from already verified manifest-cache files.
///
/// The asynchronous worker still refreshes the full pinned set. This small
/// synchronous disk pass means a returning Studio session can draw the whole
/// sample catalogue in its first frame instead of waiting for the manifest
/// worker and the UI's next poll.
pub(super) fn seed_cached_default_manifests(
    banks: &mut HashMap<String, Bank>,
    sources: &[PinnedSource],
    dir: &Path,
) {
    let mut aliases = Vec::new();
    for source in sources {
        let path = cache_path(dir, &source.url);
        let Ok(bytes) = read_manifest_cache(&path) else {
            continue;
        };
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let hash = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if hash != source.sha256 {
            continue;
        }
        let Ok(serde_json::Value::Object(object)) = serde_json::from_slice(&bytes) else {
            continue;
        };
        match &source.base {
            None => {
                for (canonical, value) in object {
                    match value {
                        serde_json::Value::String(alias) => aliases.push((canonical, alias)),
                        serde_json::Value::Array(list) => {
                            for alias in list
                                .into_iter()
                                .filter_map(|value| value.as_str().map(str::to_owned))
                            {
                                aliases.push((canonical.clone(), alias));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(base) => {
                for (name, entry) in object {
                    if !name.starts_with('_')
                        && let Some(bank) = parse_bank(&entry, base)
                    {
                        banks.insert(name, bank);
                    }
                }
            }
        }
    }
    expand_bank_aliases(banks, &aliases);
    expand_case_insensitive(banks);
}

/// Walk a folder batch, one folder at a time.
///
/// The deadline is checked between folders. A walk in progress is not
/// aborted. When the budget is spent, each row still on `Loading` gets a
/// message that names the walk, so no row stays on `Loading` for the rest
/// of the session. The function then returns the budget error, the only
/// failure the caller receives.
fn run_folders_work(
    context: &ManifestContext,
    specs: &[String],
    generation: u64,
    budget: &sample_fetch::FetchBudget,
) -> Result<(), String> {
    let mut changed = false;
    let mut spent = None;
    'folders: for spec in specs {
        loop {
            // Each folder lands on its own, so a small kit beside a huge
            // library is browsable while the library is still being walked.
            if let Err(error) = budget.check() {
                spent = Some(error);
                break 'folders;
            }
            let Some(folder) = folder_of_spec(spec) else {
                continue 'folders;
            };
            let (banks, state) = walk_global_folder(&folder);
            match fill_global_folder_slot(&context.shared, spec, generation, banks, state) {
                FolderFill::Filled => {
                    changed = true;
                    rebuild_global(&context.shared, &context.global);
                    break;
                }
                FolderFill::Superseded => break,
                FolderFill::Stale => {}
            }
        }
    }
    // A batch whose budget was spent when it arrived walked nothing at all;
    // one spent between folders stopped in front of the rest. Either way,
    // every row still standing is told why.
    if let Some(error) = spent {
        for spec in specs {
            context.note_folder_standing(&error, spec, generation);
        }
        return Err(error);
    }
    let _ = changed;
    Ok(())
}

pub(super) fn run_manifest_worker(context: ManifestContext, jobs: ManifestJobs) {
    while let Some(job) = jobs.recv() {
        let budget = sample_fetch::FetchBudget::until(
            Instant::now() + job.timeout,
            context.shared.publication.cancellation(),
        );
        // A job that stops on a map's failure it has said before says
        // nothing about the stop either: one line per reason, not per retry.
        let mut said_before = false;
        // The maps a stop leaves failed, said with it.
        let mut stopped = Vec::new();
        let result = match job.work {
            ManifestWork::Defaults { sources, cache_dir } => budget
                .check()
                .and_then(|()| load_default_manifests(&context, &sources, &cache_dir, &budget)),
            // The folder arm checks its own budget, so a batch that arrives
            // spent still reaches it: it names the walk on every row it never
            // got to, where a pre-check here in the lobby would leave those
            // rows on `Loading` for the rest of the session.
            ManifestWork::Folders { specs, generation } => {
                run_folders_work(&context, &specs, generation, &budget)
            }
            ManifestWork::Custom {
                effects,
                preloads,
                intent,
                access,
                continue_on_error,
                layer,
            } => {
                let mut unreached = effects.into_iter();
                let result = budget.check().and_then(|()| {
                    for (map, base) in unreached.by_ref() {
                        let registered = budget.check().and_then(|()| match &access {
                            ManifestAccess::Trusted => {
                                context.register_custom(&map, base.as_deref(), &budget)
                            }
                            ManifestAccess::Score(access) => context.register_score_custom(
                                &map,
                                base.as_deref(),
                                access,
                                &budget,
                                layer,
                            ),
                        });
                        match registered {
                            Ok(()) => context.note_source(map),
                            Err(error) => {
                                let news = context.note_source_failed(&map, &error);
                                // Deadline and cancellation are structural
                                // outcomes; a later effect or preload may
                                // never launder them.
                                if let Err(spent) = budget.check() {
                                    said_before = !news;
                                    stopped.push(map);
                                    return Err(spent);
                                }
                                if !continue_on_error {
                                    stopped.push(map);
                                    return Err(error);
                                }
                                if news {
                                    context.failure(SampleFailure {
                                        message: error,
                                        maps: vec![map],
                                    });
                                }
                            }
                        }
                    }
                    context.prefetch_owned(preloads, intent, &budget)
                });
                // A spent budget stops the batch in front of its later
                // maps: each reads failed, as the folder arm's rows do,
                // never loading with no job behind it.
                if let Err(spent) = budget.check() {
                    for (map, _) in unreached {
                        context.note_source_failed(&map, &spent);
                        stopped.push(map);
                    }
                }
                result
            }
        };
        let completion = job.completion;
        if completion.is_none()
            && !said_before
            && let Err(error) = &result
        {
            context.failure(SampleFailure {
                message: error.clone(),
                maps: stopped,
            });
        }
        context.finish_manifest();
        if let Some(completion) = completion {
            let _ = completion.send(result);
        }
    }
}

pub(super) fn parse_bank(entry: &serde_json::Value, base: &str) -> Option<Bank> {
    match entry {
        serde_json::Value::String(file) => Some(Bank::Array(vec![Arc::from(join_url(base, file))])),
        serde_json::Value::Array(files) => {
            let urls: Vec<Arc<str>> = files
                .iter()
                .filter_map(|file| file.as_str())
                .map(|file| Arc::from(join_url(base, file)))
                .collect();
            (!urls.is_empty()).then_some(Bank::Array(urls))
        }
        serde_json::Value::Object(keys) => {
            let mut notes = Vec::new();
            for (key, files) in keys {
                if key.starts_with('_') {
                    continue;
                }
                let midi = rustel_core::util::note_to_midi(key, 3).ok()?;
                let urls: Vec<Arc<str>> = match files {
                    serde_json::Value::String(file) => vec![Arc::from(join_url(base, file))],
                    serde_json::Value::Array(files) => files
                        .iter()
                        .filter_map(|file| file.as_str())
                        .map(|file| Arc::from(join_url(base, file)))
                        .collect(),
                    _ => return None,
                };
                if midi.is_nan() || urls.is_empty() {
                    return None;
                }
                notes.push((midi, urls));
            }
            (!notes.is_empty()).then_some(Bank::Notes(notes))
        }
        _ => None,
    }
}

/// For every registered `Bank_suffix` sound whose bank part has an alias,
/// register the lowercased `alias_suffix` for the same bank. Alias
/// canonicals match case-insensitively; existing keys are never overwritten.
pub(super) fn expand_bank_aliases(banks: &mut HashMap<String, Bank>, aliases: &[(String, String)]) {
    let mut alias_lookup: HashMap<String, Vec<&str>> = HashMap::new();
    for (canonical, alias) in aliases {
        alias_lookup
            .entry(canonical.to_lowercase())
            .or_default()
            .push(alias.as_str());
    }
    let expanded: Vec<(String, Bank)> = banks
        .iter()
        .filter_map(|(key, bank)| {
            let (prefix, suffix) = key.split_once('_')?;
            let alias_names = alias_lookup.get(&prefix.to_lowercase())?;
            Some(
                alias_names
                    .iter()
                    .map(|alias| {
                        (
                            format!("{alias}_{suffix}").to_lowercase(),
                            Bank::clone(bank),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();
    for (key, bank) in expanded {
        banks.entry(key).or_insert(bank);
    }
}

impl SampleLibrary {
    /// Register a host-trusted map of banks whose files load lazily per
    /// trigger. This route may read local paths and arbitrary URLs; evaluated
    /// score data must use [`Self::register_score_custom`] instead.
    ///
    /// `map_json` is a JSON value. A string form fetches the map:
    /// `github:user/repo[/branch[/dir]]` resolves to raw.githubusercontent's
    /// `strudel.json`, `bubo:x` to `github:Bubobubobubobubo/dough-x`, and a
    /// plain URL is fetched with its own directory as the default base.
    /// The call blocks. On success the complete map is published and ready
    /// for lookup. Fetch, deadline, and parse errors are returned directly.
    ///
    /// A host-trusted `local:` folder is walked here, not on the manifest
    /// worker. Opening a Session queues the pinned default maps first, and
    /// the worker is busy with those fetches until they finish or their
    /// budget expires. A disk walk fetches nothing, so it does not wait
    /// behind them. The hermetic studio's `.samples` folder is such a case.
    pub fn register_trusted_custom(
        &self,
        map_json: &str,
        base: Option<&str>,
    ) -> Result<(), String> {
        validate_manifest_effects(std::iter::once((map_json, base)))?;
        if let Some(result) = self.register_trusted_local_folder_now(map_json, base) {
            return result;
        }
        let mut effects = Vec::new();
        effects
            .try_reserve_exact(1)
            .map_err(|_| "samples() registration exceeds host memory".to_owned())?;
        effects.push((
            copy_manifest_text(map_json)?,
            base.map(copy_manifest_text).transpose()?,
        ));
        self.enqueue_manifest_work_blocking(ManifestWork::Custom {
            effects,
            preloads: Vec::new(),
            intent: PrefetchIntent::Explicit,
            access: ManifestAccess::Trusted,
            continue_on_error: false,
            layer: BankLayer::Score,
        })
    }

    /// `Some` when `map_json` is a `local:` folder, already published or
    /// failed. `None` means this map still belongs on the worker.
    fn register_trusted_local_folder_now(
        &self,
        map_json: &str,
        base: Option<&str>,
    ) -> Option<Result<(), String>> {
        let value: serde_json::Value = serde_json::from_str(map_json).ok()?;
        let serde_json::Value::String(source) = value else {
            return None;
        };
        let SampleSource::LocalFolder(rest) = read_source(&source, GITHUB_SAMPLE_MANIFEST) else {
            return None;
        };
        let budget = self.manifest_budget(MANIFEST_BATCH_TIMEOUT);
        let context = self.manifest_context();
        Some(
            match context.register_local_folder(rest.trim(), base, &budget) {
                Ok(()) => {
                    context.note_source(map_json.to_owned());
                    Ok(())
                }
                Err(error) => {
                    context.note_source_failed(map_json, &error);
                    Err(error)
                }
            },
        )
    }

    /// Register a map requested by evaluated score text under an explicit
    /// host policy. Every source URL in the complete map is validated before
    /// any bank becomes visible, including URLs inside fetched maps.
    ///
    /// This public API remains blocking: success means the complete map is
    /// ready for lookup. Session uses the asynchronous batch API below.
    pub fn register_score_custom(
        &self,
        map_json: &str,
        base: Option<&str>,
        access: &ScoreSampleAccess,
    ) -> Result<(), String> {
        if access.is_denied() {
            return Err(
                "score-level samples() cannot access files or the network without a host grant"
                    .to_owned(),
            );
        }
        if map_json.len() > MAX_SCORE_SAMPLE_MAP_BYTES {
            return Err(format!(
                "samples() map exceeds the {MAX_SCORE_SAMPLE_MAP_BYTES} byte limit"
            ));
        }
        validate_manifest_effects(std::iter::once((map_json, base)))?;
        let mut effects = Vec::new();
        effects
            .try_reserve_exact(1)
            .map_err(|_| "samples() registration exceeds host memory".to_owned())?;
        effects.push((
            copy_manifest_text(map_json)?,
            base.map(copy_manifest_text).transpose()?,
        ));
        self.enqueue_manifest_work_blocking(ManifestWork::Custom {
            effects,
            preloads: Vec::new(),
            intent: PrefetchIntent::Explicit,
            access: ManifestAccess::Score(access.clone()),
            continue_on_error: false,
            layer: BankLayer::Score,
        })
    }

    /// Enqueue one accepted evaluation's score-authorized `samples(...)` and
    /// `preload(...)` effects as one ordered job and one aggregate deadline.
    ///
    /// Carrying the preloads with their registrations is load-bearing: a
    /// default map or earlier score may define the same sound name, but this
    /// job cannot warm it until its own last-writer-wins overrides publish.
    pub(crate) fn register_score_batch_async_for_session(
        &self,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
        access: &ScoreSampleAccess,
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async(
            effects,
            preload_names,
            ManifestAccess::Score(access.clone()),
        )
    }

    /// Speculate about sounds named by a live score without downloading every
    /// GM instrument variant. Resolve after earlier registrations publish, so
    /// a score or global bank that overrides a GM name keeps its own semantics.
    pub fn warm_score_sounds_async(
        &self,
        names: &[String],
        access: &ScoreSampleAccess,
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async_into_with_intent(
            BankLayer::Score,
            Vec::new(),
            names,
            ManifestAccess::Score(access.clone()),
            PrefetchIntent::LiveWarm,
        )
    }

    #[cfg(test)]
    pub(super) fn register_trusted_batch_async_for_test(
        &self,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async(effects, preload_names, ManifestAccess::Trusted)
    }

    fn register_batch_async(
        &self,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
        access: ManifestAccess,
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async_into(BankLayer::Score, effects, preload_names, access)
    }

    pub(super) fn register_batch_async_into(
        &self,
        layer: BankLayer,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
        access: ManifestAccess,
    ) -> Result<PrefetchStatus, String> {
        self.register_batch_async_into_with_intent(
            layer,
            effects,
            preload_names,
            access,
            PrefetchIntent::Explicit,
        )
    }

    pub(super) fn register_batch_async_into_with_intent(
        &self,
        layer: BankLayer,
        effects: Vec<(String, Option<String>)>,
        preload_names: &[String],
        access: ManifestAccess,
        intent: PrefetchIntent,
    ) -> Result<PrefetchStatus, String> {
        validate_manifest_effects(
            effects
                .iter()
                .map(|(map, base)| (map.as_str(), base.as_deref())),
        )?;
        if let ManifestAccess::Score(policy) = &access {
            for (map, _) in &effects {
                policy.preapprove_effect(map)?;
            }
        }
        let preloads = copy_preload_specs(preload_names)?;
        let order = self
            .manifest_order
            .lock()
            .map_err(|_| "sample manifest enqueue lock is unavailable".to_owned())?;

        // With no registration ahead of us, preload can start now and report
        // an accurate public-facing count. Otherwise it must be carried in the
        // FIFO: consulting today's map would preload an about-to-be-stale URL.
        if effects.is_empty() && self.shared.manifest_pending.load(Ordering::Acquire) == 0 {
            let context = self.manifest_context();
            let mut requested = 0usize;
            for spec in preloads {
                match context.prefetch_known_with_intent(&spec, intent.priority(), intent) {
                    PrefetchStatus::Requested(files) => requested = requested.saturating_add(files),
                    PrefetchStatus::Unknown if intent.reports_unknown() => {
                        context.failure(format!("preload skipped unknown sound {spec:?}"));
                    }
                    PrefetchStatus::Unknown => {}
                    PrefetchStatus::Deferred => {
                        unreachable!("prefetch_known returns only requested or unknown")
                    }
                }
            }
            drop(order);
            return Ok(PrefetchStatus::Requested(requested));
        }

        // Marked before the push, not after: the worker may have the map
        // in before this thread runs again, and a mark placed then would
        // overwrite its verdict.
        let maps = effects
            .iter()
            .map(|(map, _)| {
                self.mark_source_loading(map);
                map.clone()
            })
            .collect::<Vec<_>>();
        if let Err(error) = self.enqueue_manifest_work_async_locked(
            ManifestWork::Custom {
                effects,
                preloads,
                intent,
                access,
                continue_on_error: true,
                layer,
            },
            MANIFEST_BATCH_TIMEOUT,
        ) {
            // Nothing will answer for these maps: the refusal is their
            // failure.
            for map in maps {
                self.shared
                    .note_source_standing(&map, SourceState::Failed(error.clone()));
            }
            return Err(error);
        }
        drop(order);
        Ok(PrefetchStatus::Deferred)
    }

    pub(super) fn enqueue_manifest_work_async(&self, work: ManifestWork) -> Result<(), String> {
        self.enqueue_manifest_work_for(work, MANIFEST_BATCH_TIMEOUT)
    }

    pub(super) fn enqueue_manifest_work_for(
        &self,
        work: ManifestWork,
        timeout: Duration,
    ) -> Result<(), String> {
        let _order = self
            .manifest_order
            .lock()
            .expect("sample manifest enqueue order");
        self.enqueue_manifest_work_async_locked(work, timeout)
    }

    fn validate_manifest_work(work: &ManifestWork) -> Result<(), String> {
        if let ManifestWork::Custom { effects, .. } = work {
            validate_manifest_effects(
                effects
                    .iter()
                    .map(|(map, base)| (map.as_str(), base.as_deref())),
            )?;
        }
        Ok(())
    }

    fn manifest_budget(&self, timeout: Duration) -> sample_fetch::FetchBudget {
        sample_fetch::FetchBudget::until(
            Instant::now() + timeout,
            self.shared.publication.cancellation(),
        )
    }

    pub(super) fn enqueue_manifest_work_async_locked(
        &self,
        work: ManifestWork,
        timeout: Duration,
    ) -> Result<(), String> {
        Self::validate_manifest_work(&work)?;
        self.push_manifest_job(ManifestJob {
            work,
            timeout,
            completion: None,
        })
        .map_err(|refused| manifest_refusal(&refused))
    }

    /// Put `job` in the loader's line, counted as pending from before it is
    /// in: the worker may finish it before this returns.
    fn push_manifest_job(
        &self,
        job: ManifestJob,
    ) -> Result<(), mpsc::TrySendError<Box<ManifestJob>>> {
        self.shared.manifest_pending.fetch_add(1, Ordering::AcqRel);
        self.manifest_queue.push(Box::new(job)).inspect_err(|_| {
            self.shared.manifest_pending.fetch_sub(1, Ordering::AcqRel);
        })
    }

    pub(super) fn enqueue_manifest_work_blocking(&self, work: ManifestWork) -> Result<(), String> {
        Self::validate_manifest_work(&work)?;
        let (completion, completed) = mpsc::sync_channel(1);
        let mut job = ManifestJob {
            work,
            timeout: MANIFEST_BATCH_TIMEOUT,
            completion: Some(completion),
        };
        // A full line is waited out, for as long as the job may take once
        // the worker has it.
        let room_by = Instant::now() + MANIFEST_BATCH_TIMEOUT;
        loop {
            let pushed = {
                let _order = self
                    .manifest_order
                    .lock()
                    .expect("sample manifest enqueue order");
                self.push_manifest_job(job)
            };
            match pushed {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) if Instant::now() < room_by => {
                    job = *returned;
                    std::thread::sleep(MANIFEST_QUEUE_WAIT_SLICE);
                }
                Err(refused) => return Err(manifest_refusal(&refused)),
            }
        }
        // The worker answers every job it takes, within the job's timeout.
        completed
            .recv()
            .unwrap_or_else(|_| Err("sample manifest loader stopped".to_owned()))
    }

    pub(super) fn manifest_context(&self) -> ManifestContext {
        ManifestContext {
            banks: Arc::clone(&self.banks),
            custom: Arc::clone(&self.custom),
            global: Arc::clone(&self.global),
            gm: Arc::clone(&self.gm),
            shared: Arc::clone(&self.shared),
        }
    }

    /// Walk a folder batch on this library with a budget the test chose -
    /// the same call `run_manifest_worker` makes for `ManifestWork::Folders`,
    /// minus the queue. A test that needs the batch to arrive already spent
    /// cannot get there through the real one, which budgets its own.
    #[cfg(test)]
    pub(super) fn run_folders_work_for_test(
        &self,
        specs: &[String],
        generation: u64,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        run_folders_work(&self.manifest_context(), specs, generation, budget)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    #[cfg(test)]
    pub(in crate::samples) mod manifest_congestion_tests {
        //! A busy manifest loader keeps a job waiting in line rather than turning it
        //! away; only the line's bound refuses, and a refusal is a failure. See
        //! [`ManifestQueue`].

        use super::*;
        use crate::samples::manifest_queue::MANIFEST_QUEUE_JOBS;

        const SPEC: &str = "github:me/pack";

        /// A score grant that lets `github:` maps be fetched.
        pub(in crate::samples) fn access() -> ScoreSampleAccess {
            let mut access = ScoreSampleAccess::denied();
            access
                .permit_source("https://raw.githubusercontent.com")
                .expect("origin");
            access
        }

        fn inline(name: &str) -> String {
            format!(r#"{{"{name}":"http://127.0.0.1:9/{name}.wav"}}"#)
        }

        /// Put `count` inline registrations in a line nothing drains.
        fn park(library: &SampleLibrary, count: usize) {
            for slot in 0..count {
                library
                    .register_trusted_batch_async_for_test(
                        vec![(inline(&format!("slot{slot}")), None)],
                        &[],
                    )
                    .expect("a busy loader parks the job");
            }
        }

        fn quoted(spec: &str) -> String {
            serde_json::to_string(spec).expect("a string")
        }

        /// What a job asks for, in a word, for reading the line's order.
        fn describe(job: &ManifestJob) -> String {
            match &job.work {
                ManifestWork::Defaults { .. } => "defaults".to_owned(),
                ManifestWork::Folders { specs, .. } => format!("folders {}", specs.join(" ")),
                ManifestWork::Custom {
                    effects, preloads, ..
                } => effects
                    .iter()
                    .map(|(map, _)| map.clone())
                    .chain(preloads.iter().map(|spec| format!("preload {spec}")))
                    .collect::<Vec<_>>()
                    .join(" "),
            }
        }

        /// A look-up behind a busy loader waits in line: its spec reads loading,
        /// the loader counts it as pending, and the checker has nothing to refuse.
        #[test]
        fn a_look_up_behind_a_busy_loader_waits_and_reads_loading() {
            let (library, jobs) = crate::samples::live_warm_tests::library();
            park(&library, 2);

            library
                .look_up_samples_source(SPEC, &access())
                .expect("a busy loader parks the look-up");
            assert_eq!(
                library.samples_source_state(SPEC),
                Some(SourceState::Loading)
            );
            assert_eq!(library.manifests_pending(), 3);
            let source = format!("samples('{SPEC}')\n$: s(\"bd\")");
            let diagnostics = crate::lint::lint(&source, false, Some(&library));
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
            let last = jobs.try_iter().last().expect("jobs in line");
            assert_eq!(describe(&last), quoted(SPEC));
        }

        /// Jobs leave the line in the order they were put in, whichever producer
        /// put them there, so the last registration wins and a preload rides behind
        /// the registration it waits for.
        #[test]
        fn jobs_leave_the_line_in_the_order_every_producer_put_them_in() {
            let (library, jobs) = crate::samples::live_warm_tests::library();
            let walked = tempfile::tempdir().expect("a folder to walk");
            let folder = walked.path().to_string_lossy().into_owned();

            library
                .register_trusted_batch_async_for_test(vec![(inline("first"), None)], &[])
                .expect("an evaluation's registration");
            library
                .warm_score_sounds_async(&["first".to_owned()], &access())
                .expect("a warm-up behind it");
            library
                .look_up_samples_source(SPEC, &access())
                .expect("the checker's look-up");
            library.adopt_global_sources(&[GlobalSource {
                spec: folder.clone(),
                enabled: true,
            }]);
            library
                .register_trusted_batch_async_for_test(vec![(inline("last"), None)], &[])
                .expect("a later registration");

            assert_eq!(
                jobs.try_iter()
                    .map(|job| describe(&job))
                    .collect::<Vec<_>>(),
                [
                    inline("first"),
                    "preload first".to_owned(),
                    quoted(SPEC),
                    format!("folders {folder}"),
                    inline("last"),
                ]
            );
        }

        /// A Settings source asked for while the loader is busy waits in line: its
        /// row reads fetching, not failed.
        #[test]
        fn a_settings_source_behind_a_busy_loader_waits_rather_than_failing() {
            let (library, _jobs) = crate::samples::live_warm_tests::library();
            park(&library, 2);
            let folder = tempfile::tempdir().expect("a folder to walk");

            let reports = library.adopt_global_sources(&[
                GlobalSource {
                    spec: folder.path().to_string_lossy().into_owned(),
                    enabled: true,
                },
                GlobalSource {
                    spec: "https://example.com/pack/strudel.json".to_owned(),
                    enabled: true,
                },
            ]);

            assert_eq!(reports.len(), 2);
            assert!(
                reports
                    .iter()
                    .all(|report| report.state == GlobalSourceState::Loading),
                "{reports:?}"
            );
        }

        /// Past its bound of jobs the line refuses, and the refusal is the spec's
        /// failure: the loader is overwhelmed, not merely busy.
        #[test]
        fn the_line_refuses_past_its_bound_and_the_spec_says_why() {
            let (library, _jobs) = crate::samples::live_warm_tests::library();
            park(&library, MANIFEST_QUEUE_JOBS);

            let error = library
                .look_up_samples_source(SPEC, &access())
                .expect_err("the line is full");
            assert!(error.contains("queue is full"), "{error}");
            assert_eq!(
                library.samples_source_state(SPEC),
                Some(SourceState::Failed(error))
            );
            assert_eq!(library.manifests_pending(), MANIFEST_QUEUE_JOBS);
        }

        /// The text the waiting jobs carry is bounded too: a few of the largest
        /// registrations fill the line long before its count of jobs does.
        #[test]
        fn the_text_waiting_in_line_is_bounded() {
            let (library, _jobs) = crate::samples::live_warm_tests::library();
            let large = |name: &str| {
                let file = "x".repeat(MAX_MANIFEST_EFFECT_BYTES - 64);
                vec![(
                    format!(r#"{{"{name}":"http://127.0.0.1:9/{file}.wav"}}"#),
                    None,
                )]
            };
            for name in ["one", "two"] {
                library
                    .register_trusted_batch_async_for_test(large(name), &[])
                    .expect("room for two of the largest");
            }
            let error = library
                .register_trusted_batch_async_for_test(large("three"), &[])
                .expect_err("the line's text is full");
            assert!(error.contains("queue is full"), "{error}");
            assert_eq!(library.manifests_pending(), 2);
        }

        /// A stopped loader refuses every job, and each map it names says why.
        #[test]
        fn a_stopped_loader_marks_every_map_failed() {
            let (library, jobs) = crate::samples::live_warm_tests::library();
            let arrived = "github:me/arrived";
            library.note_samples_source_for_tests(arrived);
            drop(jobs);

            let error = library
                .register_score_batch_async_for_session(
                    vec![(quoted(arrived), None), (quoted(SPEC), None)],
                    &[],
                    &access(),
                )
                .expect_err("the loader has stopped");
            assert!(error.contains("stopped"), "{error}");
            for spec in [arrived, SPEC] {
                assert_eq!(
                    library.samples_source_state(spec),
                    Some(SourceState::Failed(error.clone()))
                );
            }
        }
    }

    #[cfg(test)]
    mod source_retention_tests {
        //! Repeated score registration retains a fixed source-history working set.
        //!
        //! Owned string storage reaches a plateau and container capacities remain
        //! bounded. This does not measure allocator overhead, process RSS, or the
        //! memory used by decoded audio and other tables.

        use super::*;

        const MAP_BYTES: usize = 1024 * 1024;
        const REGISTRATIONS: usize = 96;

        fn location(index: usize) -> &'static str {
            if index.is_multiple_of(2) {
                "https://samples.example/first.wav"
            } else {
                "https://samples.example/second.wav"
            }
        }

        /// Different valid JSON text, with one bank and only two file identities.
        /// Whitespace fills the history budget without accumulating banks or URLs.
        fn source_map(index: usize) -> String {
            let mut map = String::with_capacity(MAP_BYTES);
            map.push_str(&format!(r#"{{"active":"{}"}}"#, location(index)));
            map.extend(std::iter::repeat_n('\n', index));
            map.extend(std::iter::repeat_n(' ', MAP_BYTES - map.len()));
            map
        }

        #[derive(Debug, PartialEq, Eq)]
        struct HistoryStorage {
            unique_text_bytes: usize,
            entry_counts: [usize; 3],
            string_capacity: usize,
        }

        fn history_storage(library: &SampleLibrary) -> HistoryStorage {
            let tables = library
                .shared
                .source_tables
                .lock()
                .expect("samples source tables");
            assert_eq!(
                tables.bytes,
                tables.order.iter().map(String::len).sum::<usize>()
            );
            // Hash-map capacity can vary as removal leaves tombstones and insertion
            // reclaims them. Bound that slack instead of requiring identical capacity
            // at every checkpoint; source text, entries and string buffers are exact.
            for capacity in [
                tables.sources.capacity(),
                tables.states.capacity(),
                tables.order.capacity(),
            ] {
                assert!(capacity <= 4 * (MAX_SOURCE_TABLE_BYTES / MAP_BYTES));
            }
            HistoryStorage {
                unique_text_bytes: tables.bytes,
                entry_counts: [
                    tables.sources.len(),
                    tables.states.len(),
                    tables.order.len(),
                ],
                string_capacity: tables
                    .sources
                    .iter()
                    .chain(tables.states.keys())
                    .chain(tables.order.iter())
                    .map(String::capacity)
                    .sum(),
            }
        }

        #[test]
        fn repeated_score_registrations_bound_history_and_keep_current_lookup() {
            // Keep this fixture at 96 MiB of streamed input if the product's budget
            // changes: resizing the soak should be an explicit test decision.
            assert_eq!(MAX_SOURCE_TABLE_BYTES, 32 * MAP_BYTES);
            let retained_maps = MAX_SOURCE_TABLE_BYTES / MAP_BYTES;
            let library = SampleLibrary::empty();
            library.set_direct_diagnostic_logging(false);
            let mut access = ScoreSampleAccess::denied();
            access
                .permit_source("https://samples.example")
                .expect("fixture origin");
            let first = source_map(0);
            let mut plateau = None;

            for index in 0..REGISTRATIONS {
                let map = source_map(index);
                library
                    .register_score_custom(&map, None, &access)
                    .expect("inline score map publishes without a fetch");
                assert_eq!(library.samples_source_state(&map), Some(SourceState::Ready));
                assert!(library.knows_samples_source(&map));
                assert_eq!(
                    library.file_location("active", None).as_deref(),
                    Some(location(index)),
                    "the newest registration supplies the active bank"
                );

                let completed = index + 1;
                if completed == retained_maps + 1 || completed.is_multiple_of(retained_maps) {
                    let storage = history_storage(&library);
                    assert_eq!(storage.unique_text_bytes, MAX_SOURCE_TABLE_BYTES);
                    assert_eq!(storage.entry_counts, [retained_maps; 3]);
                    assert_eq!(storage.string_capacity, 3 * MAX_SOURCE_TABLE_BYTES);
                    if completed > retained_maps {
                        assert!(!library.knows_samples_source(&first));
                        assert_eq!(library.samples_source_state(&first), None);
                        match &plateau {
                            Some(previous) => assert_eq!(&storage, previous),
                            None => plateau = Some(storage),
                        }
                    }
                }
            }

            assert!(plateau.is_some(), "the registrations crossed the bound");
            assert_eq!(library.manifests_pending(), 0);
            assert_eq!(library.pending_loads(), 0, "lookup never fetches audio");
            assert!(library.take_failures().is_empty());
        }
    }

    #[cfg(test)]
    mod source_retry_tests {
        //! Failed manifest imports retry after a cooldown. Repeated failures increase
        //! the background retry delay. Each new failure reason is reported once.
        //! Updates remain blocked until the import succeeds.

        use super::*;
        use crate::samples::manifest_congestion_tests::access;
        use std::io::Write;
        use std::net::TcpListener;

        /// A server answering each request with the next status and body of
        /// `script`, at an address no earlier run can have cached. `{origin}` in a
        /// body is its own origin. Returns the origin, the manifest's address and
        /// the server thread.
        fn scripted_server(
            script: Vec<(u16, &'static str)>,
        ) -> (String, String, std::thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
            let origin = format!("http://{}", listener.local_addr().expect("address"));
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let spec = format!("{origin}/retry-{}-{unique}.json", std::process::id());
            let own = origin.clone();
            let server = std::thread::spawn(move || {
                for (status, body) in script {
                    let (stream, _) = listener.accept().expect("accept");
                    let mut stream = crate::samples::manifest_worker_tests::drain_request(stream);
                    let body = body.replace("{origin}", &own);
                    write!(
                        stream,
                        "HTTP/1.1 {status} Scripted\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .expect("write response");
                }
            });
            (origin, spec, server)
        }

        /// A failed manifest blocks updates until a retry succeeds. Repeated failures
        /// report each distinct reason once. Recovery does not require a restart.
        #[test]
        fn a_failed_import_recovers_once_a_later_ask_succeeds() {
            let (origin, spec, server) = scripted_server(vec![
                (404, ""),
                (404, ""),
                (500, ""),
                (200, r#"{"_base":"{origin}/","retried":"retried.wav"}"#),
            ]);
            let mut access = ScoreSampleAccess::denied();
            access.permit_source(&origin).expect("origin");
            let library = SampleLibrary::empty();
            let score = format!("samples('{spec}')\n$: s(\"retried\")");
            let refusal = || crate::lint::rejection(&score, false, Some(&library));
            let ask = || {
                library
                    .look_up_samples_source(&spec, &access)
                    .expect("asked");
                library.wait_until_idle(Duration::from_secs(30));
                library.samples_source_state(&spec)
            };

            let Some(SourceState::Failed(missing)) = ask() else {
                panic!("a missing map is a failure");
            };
            assert_eq!(
                library.take_failures_with_maps(),
                [SampleFailure {
                    message: missing.clone(),
                    maps: vec![serde_json::to_string(&spec).expect("a string")],
                }],
                "said about its own map"
            );
            assert!(
                refusal().is_some_and(|reason| reason.contains(&missing)),
                "the failure refuses the update"
            );

            // Fresh, it rests: nobody asks the server.
            assert_eq!(ask(), Some(SourceState::Failed(missing.clone())));

            library.rest_samples_source_for_tests(&spec);
            assert_eq!(ask(), Some(SourceState::Failed(missing.clone())));
            assert!(
                library.take_failures().is_empty(),
                "the same failure is said once"
            );
            assert!(refusal().is_some(), "and still refuses the update");

            library.rest_samples_source_for_tests(&spec);
            let Some(SourceState::Failed(broken)) = ask() else {
                panic!("a server error is a failure");
            };
            assert_ne!(broken, missing);
            assert_eq!(library.take_failures(), [broken]);

            library.rest_samples_source_for_tests(&spec);
            assert_eq!(ask(), Some(SourceState::Ready));
            assert!(library.knows("retried"));
            assert_eq!(refusal(), None, "the update goes through");
            server.join().expect("server");
        }

        /// A retry requires an expired cooldown and an idle loader. The source keeps
        /// its failure reason while the retry is pending.
        #[test]
        fn a_failure_is_asked_again_only_once_rested_with_the_loader_idle() {
            let spec = "github:me/gone";
            let failed = SourceState::Failed("offline".to_owned());

            let (library, _jobs) = crate::samples::live_warm_tests::library();
            library.note_samples_source_state_for_tests(spec, failed.clone());
            library
                .look_up_samples_source(spec, &access())
                .expect("a fresh failure rests");
            assert_eq!(library.manifests_pending(), 0, "a fresh failure rests");
            library.rest_samples_source_for_tests(spec);
            library
                .register_trusted_batch_async_for_test(
                    vec![(r#"{"busy":"http://127.0.0.1:9/busy.wav"}"#.to_owned(), None)],
                    &[],
                )
                .expect("the loader has something in hand");
            library
                .look_up_samples_source(spec, &access())
                .expect("a busy loader is left alone");
            assert_eq!(
                library.manifests_pending(),
                1,
                "a busy loader is left alone"
            );

            let (library, _jobs) = crate::samples::live_warm_tests::library();
            library.note_samples_source_state_for_tests(spec, failed.clone());
            library.rest_samples_source_for_tests(spec);
            library
                .look_up_samples_source(spec, &access())
                .expect("asked again");
            assert_eq!(library.manifests_pending(), 1);
            assert_eq!(
                library.samples_source_state(spec),
                Some(failed),
                "the failure stands while it is asked again"
            );
        }

        /// Consecutive failures double the background retry delay. Explicit requests
        /// keep the shorter cooldown. A successful response resets the failure count.
        #[test]
        fn a_failure_in_a_row_rests_longer_before_the_background_asks() {
            let (origin, spec, server) = scripted_server(vec![(404, ""), (404, "")]);
            let mut access = ScoreSampleAccess::denied();
            access.permit_source(&origin).expect("origin");
            let library = SampleLibrary::empty();
            let ask = || {
                library
                    .look_up_samples_source(&spec, &access)
                    .expect("asked");
                library.wait_until_idle(Duration::from_secs(30));
            };

            ask();
            library.rest_samples_source_for_tests(&spec);
            assert!(
                library.samples_source_rested(&spec, true),
                "a first failure"
            );

            ask();
            library.rest_samples_source_for_tests(&spec);
            assert!(library.samples_source_rested(&spec, false), "an ask");
            assert!(
                !library.samples_source_rested(&spec, true),
                "the second failure rests twice as long"
            );
            library.rest_samples_source_for_tests(&spec);
            assert!(library.samples_source_rested(&spec, true));
            server.join().expect("server");

            library.note_samples_source_state_for_tests(&spec, SourceState::Ready);
            library
                .note_samples_source_state_for_tests(&spec, SourceState::Failed("offline".into()));
            library.rest_samples_source_for_tests(&spec);
            assert!(
                library.samples_source_rested(&spec, true),
                "an answer starts the count over"
            );
        }

        /// A refused lookup counts as one failure, whether access is denied before
        /// submission or the queue refuses the job.
        #[test]
        fn a_refused_look_up_counts_as_one_failure() {
            let (library, jobs) = crate::samples::live_warm_tests::library();
            drop(jobs);
            for (spec, access) in [
                ("github:me/refused", access()),
                ("github:me/denied", ScoreSampleAccess::denied()),
            ] {
                let ask = || {
                    library
                        .look_up_samples_source(spec, &access)
                        .expect_err("refused");
                    library.rest_samples_source_for_tests(spec);
                };
                ask();
                assert!(library.samples_source_rested(spec, true), "{spec}: one");
                ask();
                assert!(!library.samples_source_rested(spec, true), "{spec}: two");
                library.rest_samples_source_for_tests(spec);
                assert!(library.samples_source_rested(spec, true), "{spec}: rested");
            }
        }

        /// The retry delay cannot exceed [`FAILED_RETRY_LONGEST`].
        #[test]
        fn the_rest_stops_growing() {
            let standing = |failures| Standing {
                failures,
                ..Standing::now(SourceState::Failed("offline".into()))
            };
            assert_eq!(standing(1).rest(), FAILED_RETRY_AFTER);
            assert_eq!(standing(2).rest(), FAILED_RETRY_AFTER * 2);
            assert_eq!(standing(u32::MAX).rest(), FAILED_RETRY_LONGEST);
        }
    }

    #[cfg(test)]
    mod source_text_tests {
        //! What the text of a `samples()` source, base or bank entry may hold, and
        //! what a refusal of it says.

        use super::*;

        const IDLE: Duration = Duration::from_secs(5);

        /// A library that has registered what `register` asks of it and settled,
        /// with nothing refused.
        fn settled(register: impl FnOnce(&SampleLibrary) -> Result<(), String>) -> SampleLibrary {
            let library = SampleLibrary::empty();
            register(&library).expect("the map is queued");
            library.wait_until_idle(IDLE);
            assert_eq!(library.take_failures(), Vec::<String>::new());
            library
        }

        /// `map` registered on the trusted route, as the studio's own sources are.
        fn trusted(map: &str, base: Option<&str>) -> SampleLibrary {
            settled(|library| library.register_trusted_custom(map, base))
        }

        /// `map` registered by a score granted `origin`, as `--allow-sample-origin`
        /// grants it.
        fn scored(map: &str, base: Option<&str>, origin: &str) -> SampleLibrary {
            let mut access = ScoreSampleAccess::denied();
            access.permit_source(origin).expect("a well-formed origin");
            settled(|library| library.register_score_custom(map, base, &access))
        }

        /// A refused bank entry reports the fetch boundary's own reason, such as
        /// the scheme or a missing host. The other entries still register.
        #[test]
        fn a_refused_bank_entry_says_what_was_wrong() {
            let library = SampleLibrary::empty();
            library
                .register_trusted_custom(
                    r#"{
                "ftp": "ftp://example.com/kit/bd.wav",
                    "hostless": "https:///kit/bd.wav",
                    "fine": "https://example.com/kit/bd.wav"
                }"#,
                    None,
                )
                .expect("the map is queued");
            library.wait_until_idle(IDLE);
            let failures = library.take_failures();

            let refusal = |name: &str| {
                failures
                    .iter()
                    .find(|failure| {
                        failure.starts_with(&format!("samples() entry {name:?} was refused: "))
                    })
                    .unwrap_or_else(|| panic!("{name} was not refused: {failures:?}"))
            };
            assert!(refusal("ftp").contains("is a ftp: address"), "{failures:?}");
            assert!(
                refusal("hostless").contains("names no host"),
                "{failures:?}"
            );
            assert!(
                failures
                    .iter()
                    .all(|failure| !failure.contains("bank URLs must be http(s)")),
                "{failures:?}"
            );
            assert_eq!(
                library.file_location("fine", None).as_deref(),
                Some("https://example.com/kit/bd.wav")
            );
        }

        /// Drum libraries name cymbals by size in inches: `Crash 18".wav`. A bank
        /// holding one registers on both routes, as it does in a browser, which
        /// percent-encodes the quote; a sample server decodes it back. The trusted
        /// route keeps the entry as written and encodes it when it fetches; the
        /// score route registers the parsed address.
        #[test]
        fn a_file_name_holding_a_quote_still_registers() {
            let map = r#"{"crash": "https://example.com/kit/Crash 18\".wav"}"#;
            assert_eq!(
                trusted(map, None).file_location("crash", None).as_deref(),
                Some("https://example.com/kit/Crash 18\".wav")
            );
            assert_eq!(
                scored(map, None, "https://example.com")
                    .file_location("crash", None)
                    .as_deref(),
                Some("https://example.com/kit/Crash%2018%22.wav")
            );
        }

        /// A `samples()` source written in a multi-line template literal ends in
        /// a newline. The URL parser always dropped it, and it is still dropped:
        /// on the score's own address, and on the address a `github:` shorthand
        /// expands to, where it would otherwise land inside the path.
        #[test]
        fn a_samples_source_with_a_trailing_newline_still_registers() {
            let mut access = ScoreSampleAccess::denied();
            access.permit_builtin_sources();
            access.permit_source("samples.example").unwrap();

            let source = "https://samples.example/kit.json\n";
            access
                .preapprove_effect(&serde_json::to_string(source).unwrap())
                .expect("the score's samples() call is accepted");
            let (approved, _) = access.approve_remote(source).expect("approved");
            assert_eq!(approved.as_ref(), "https://samples.example/kit.json");
            // Leading whitespace too, the same on this route as on the trusted one.
            let (approved, _) = access
                .approve_remote("\n  https://samples.example/kit.json")
                .expect("approved with leading whitespace");
            assert_eq!(approved.as_ref(), "https://samples.example/kit.json");

            match read_source("github:tidalcycles/dirt-samples\n", GITHUB_SAMPLE_MANIFEST) {
                SampleSource::Url(url) => assert_eq!(
                    url,
                    "https://raw.githubusercontent.com/tidalcycles/dirt-samples/main/strudel.json"
                ),
                SampleSource::LocalFolder(folder) => panic!("read as a folder: {folder}"),
            }
            access
                .preapprove_effect(
                    &serde_json::to_string("github:tidalcycles/dirt-samples\n").unwrap(),
                )
                .expect("the shorthand is accepted too");
        }

        /// A base written in a template literal ends in a newline too, and a base is
        /// joined to every file name: `https://samples.example/kit/\n` + `bd.wav`.
        /// The newline is trimmed off the base before the join, on both routes, so
        /// each file registers at the address it names.
        #[test]
        fn a_base_with_a_trailing_newline_still_joins_its_files() {
            let map = r#"{"bd": "bd.wav", "sd": {"_base": "https://samples.example/snares/\n", "c3": "sd.wav"}}"#;
            let base = Some("https://samples.example/kit/\n");
            for library in [
                trusted(map, base),
                scored(map, base, "https://samples.example"),
            ] {
                assert_eq!(
                    library.file_location("bd", None).as_deref(),
                    Some("https://samples.example/kit/bd.wav")
                );
                assert_eq!(
                    library.file_location("sd", None).as_deref(),
                    Some("https://samples.example/snares/sd.wav")
                );
            }
        }

        #[test]
        fn loading_and_published_sources_share_the_retention_bound() {
            let library = SampleLibrary::empty();
            let pad = "x".repeat(1024 * 1024);
            let first = format!("{{\"m-first\": \"{pad}\"}}");
            let latest = format!("{{\"m-latest\": \"{}\"}}", "y".repeat(1024 * 1024));
            library.mark_source_loading(&first);
            library
                .shared
                .note_source_standing(&first, SourceState::Ready);
            // Enough distinct megabyte maps to cross the retention bound.
            for index in 0..(MAX_SOURCE_TABLE_BYTES / (1024 * 1024) + 2) {
                let map = format!("{{\"m{index}\": \"{pad}\"}}");
                library.mark_source_loading(&map);
                library
                    .shared
                    .note_source_standing(&map, SourceState::Ready);
            }
            let tables = library
                .shared
                .source_tables
                .lock()
                .expect("samples source tables");
            assert!(
                !tables.states.contains_key(&first) && !tables.sources.contains(&first),
                "the oldest registration survived eviction at the retention bound"
            );
            assert!(
                tables.bytes <= MAX_SOURCE_TABLE_BYTES,
                "the retained bytes ran past the bound"
            );
            assert_eq!(
                tables.bytes,
                tables.order.iter().map(String::len).sum::<usize>()
            );
            drop(tables);
            // The newest registration is never the one evicted.
            library
                .shared
                .note_source_standing(&latest, SourceState::Ready);
            assert!(
                library
                    .shared
                    .source_tables
                    .lock()
                    .expect("samples source tables")
                    .states
                    .contains_key(&latest)
            );
        }

        #[test]
        fn refreshed_source_status_does_not_duplicate_retention_accounting() {
            let mut tables = SourceTables::default();
            for state in [SourceState::Loading, SourceState::Ready] {
                tables.note("map", state);
                tables.forget_state("map");
                tables.note("map", SourceState::Loading);
                assert_eq!(tables.bytes, "map".len());
                assert_eq!(tables.order.len(), 1);
            }
            tables.note("map", SourceState::Ready);
            assert!(tables.sources.contains("map"));
        }

        #[test]
        fn oversized_lookup_failures_do_not_enter_source_history() {
            let mut tables = SourceTables::default();
            tables.note("recent", SourceState::Ready);
            let oversized = "x".repeat(MAX_SCORE_SAMPLE_MAP_BYTES + 1);
            tables.note(&oversized, SourceState::Failed("map exceeds limit".into()));
            assert_eq!(tables.states.len(), 1);
            assert!(tables.sources.contains("recent"));
            assert_eq!(tables.bytes, "recent".len());
        }
    }

    #[cfg(test)]
    mod score_source_boundary_tests {
        use super::*;

        #[test]
        fn score_and_trusted_routes_share_one_wire_identity() {
            let mut access = ScoreSampleAccess::denied();
            access
                .permit_source("https://example.com")
                .expect("grant origin");
            let raw = "HTTPS://EXAMPLE.com:443/kit/take#2.wav?q=//";
            let (approved, approved_access) =
                access.approve_remote(raw).expect("approve score URL");

            assert_eq!(
                approved.as_ref(),
                sample_fetch::wire_url(raw).unwrap().as_str()
            );
            assert_eq!(
                approved_access,
                ScoreFetchAccess::Remote {
                    rule: SourceRule::parse("https://example.com").expect("a test source"),
                }
            );
            assert_eq!(
                cache_path(Path::new("/cache"), approved.as_ref()),
                cache_path(Path::new("/cache"), raw),
                "authorization and cache identity diverged from the request"
            );
        }

        /// The built-in sources allow their own domains over https and nothing
        /// else. Loopback names and plain http need a source that spells them
        /// out.
        #[test]
        fn the_builtin_sources_allow_their_domains_and_nothing_nearer() {
            let denied = ScoreSampleAccess::denied();
            assert!(denied.is_denied());
            assert!(
                denied
                    .approve_remote("https://raw.githubusercontent.com/a/b/main/strudel.json")
                    .is_err()
            );

            let mut access = ScoreSampleAccess::denied();
            access.permit_builtin_sources();
            assert!(!access.is_denied(), "the built-in sources are not a denial");
            let (_, github) = access
                .approve_remote("https://raw.githubusercontent.com/a/b/main/strudel.json")
                .expect("a built-in domain");
            assert_eq!(
                github,
                ScoreFetchAccess::Remote {
                    rule: SourceRule::parse("raw.githubusercontent.com").expect("a test source"),
                }
            );
            access
                .approve_remote("https://cdn.freesound.org/previews/182/182967_529852-lq.mp3")
                .expect("freesound is built in");
            for refused in [
                "https://samples.example/kit.json",
                "http://raw.githubusercontent.com/a/b/main/strudel.json",
                "https://localhost:9000/kit.json",
                "https://kit.localhost/kit.json",
                "https://127.0.0.1:9000/kit.json",
                "https://[::1]:9000/kit.json",
            ] {
                let refusal = access.approve_remote(refused).expect_err(refused);
                assert!(
                    refusal.contains("--allow-sample-origin"),
                    "{refused}: the refusal must name the flag that fixes it: {refusal}"
                );
            }

            access
                .permit_source("http://localhost:9000")
                .expect("an address on this computer");
            let (_, granted) = access
                .approve_remote("http://localhost:9000/kit.json")
                .expect("a source that spells out loopback reaches it");
            assert_eq!(
                granted,
                ScoreFetchAccess::Remote {
                    rule: SourceRule::parse("http://localhost:9000").expect("a test source"),
                }
            );
        }

        /// An allowed source loads without asking the server for CORS
        /// headers: the request carries no `Origin`, and a response without
        /// `Access-Control-Allow-Origin` is read.
        #[test]
        fn an_allowed_source_loads_without_cors_headers() {
            use std::io::Write;
            use std::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let url = format!("{origin}/kit.wav");
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("one request");
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut sent_origin = false;
                let mut line = String::new();
                loop {
                    line.clear();
                    match std::io::BufRead::read_line(&mut reader, &mut line) {
                        Ok(0) => break,
                        Ok(_) if line == "\r\n" || line == "\n" => break,
                        Ok(_) => sent_origin |= line.to_ascii_lowercase().starts_with("origin:"),
                        Err(_) => break,
                    }
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                );
                let _ = stream.flush();
                sent_origin
            });

            let allowed = ScoreFetchAccess::Remote {
                rule: SourceRule::parse(&origin).expect("a test source"),
            };
            assert_eq!(
                fetch_score_source(&url, &allowed, 4096).expect("an allowed fetch"),
                b"ok".to_vec()
            );
            assert!(!server.join().expect("server thread"), "no Origin header");
        }

        fn settle(library: &SampleLibrary) {
            library.wait_until_idle(Duration::from_secs(2));
        }

        /// An origin grant gives network access only, never the filesystem.
        /// With no grant, the call fails instead of dropping the bad entry.
        #[test]
        fn an_origin_grant_never_unlocks_the_filesystem() {
            let library = SampleLibrary::empty();

            assert!(
                library
                    .register_score_custom(
                        r#"{"bd": "file:///etc/passwd"}"#,
                        None,
                        &ScoreSampleAccess::denied(),
                    )
                    .is_err(),
                "an ungranted score must fail closed"
            );
            assert!(!library.knows("bd"));

            // Grant an ordinary web origin, then try the same local read.
            let _ = library
                .register_custom_for_test("https://example.com", r#"{"bd": "file:///etc/passwd"}"#);
            assert!(
                !library.knows("bd"),
                "an origin grant is not a filesystem grant"
            );

            // A `_base` is as much a local read as an entry is.
            let _ = library.register_custom_for_test(
                "https://example.com",
                r#"{"sd": {"_base": "file:///etc", "c3": "passwd"}}"#,
            );
            assert!(!library.knows("sd"));
        }

        /// The documented web paths keep working: an absolute https entry
        /// registers without any network activity (loads happen per trigger).
        #[test]
        fn ordinary_web_sources_still_register() {
            let library = SampleLibrary::empty();
            library
                .register_custom_for_test(
                    "https://example.com",
                    r#"{"bd": "https://example.com/kits/808/kick.wav"}"#,
                )
                .expect("https entry");
            settle(&library);
            assert!(library.knows("bd"));
        }

        /// Cache names stay flat even when a URL's tail tries to carry a path.
        #[test]
        fn cache_names_take_only_a_plain_extension() {
            let dir = std::env::temp_dir();
            for (url, expected) in [
                ("https://example.com/kick.wav", Some(".wav")),
                ("https://example.com/kick.WAV", Some(".WAV")),
                ("https://example.com/kick.wav?rev=2", Some(".wav")),
                ("https://example.com/a.b/c", None),
                ("https://example.com/x/y..//z", None),
            ] {
                let path = cache_path(&dir, url);
                let parent_gone = path.parent().map(|p| p == dir).unwrap_or(true);
                assert!(parent_gone, "{url} escaped the cache directory: {path:?}");
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                assert_eq!(
                    name.rfind('.').map(|i| &name[i..]),
                    expected,
                    "{url}: wrong extension handling ({name})"
                );
            }
        }
    }

    #[cfg(test)]
    pub(in crate::samples) mod manifest_worker_tests {
        /// How long a liveness wait allows for a request to arrive.
        ///
        /// The value is generous on purpose. These waits check that the request
        /// reached the server, not how fast. The separate `elapsed()` bounds make
        /// the timing claims. A short timeout fails under parallel load.
        const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(30);

        use super::*;
        use std::io::{BufRead, Write};
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::AtomicUsize;

        fn async_register(
            library: &SampleLibrary,
            effects: Vec<(String, Option<String>)>,
            preloads: &[String],
        ) -> Result<PrefetchStatus, String> {
            library.register_trusted_batch_async_for_test(effects, preloads)
        }

        fn inline_bank(name: &str, url: &str) -> String {
            format!(r#"{{"{name}":"{url}"}}"#)
        }

        fn digest(bytes: &[u8]) -> String {
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }

        /// A budget whose deadline has already passed: every `check` fails at
        /// once, deterministic where waiting out a real deadline is not.
        fn rustel_sample_fetch_deadline_in_past() -> sample_fetch::FetchBudget {
            sample_fetch::FetchBudget::until(
                Instant::now() - Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
            )
        }

        fn test_dir(label: &str) -> PathBuf {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "rustel-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("test directory");
            path
        }

        fn wait_for_manifest_jobs(shared: &Shared) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while shared.manifest_pending.load(Ordering::Acquire) != 0 && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(shared.manifest_pending.load(Ordering::Acquire), 0);
        }

        pub(in crate::samples) fn drain_request(stream: TcpStream) -> TcpStream {
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone stream"));
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            stream
        }

        #[test]
        fn dropping_a_library_releases_its_background_workers() {
            let library = SampleLibrary::empty();
            let shared = Arc::downgrade(&library.shared);
            drop(library);

            let deadline = Instant::now() + Duration::from_secs(5);
            while shared.strong_count() != 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                shared.strong_count(),
                0,
                "sample/font/manifest worker retained Shared after its library dropped"
            );
        }

        fn respond(mut stream: TcpStream, body: &str) {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        }

        #[test]
        fn retry_batch_stays_before_publications_on_both_sides_of_its_lock() {
            let library = SampleLibrary::empty();
            let replaced_id = SampleId(9);
            let retry_id = SampleId(10);
            let decoded =
                |value| DecodedSample::from_parts(48_000, 1, vec![value]).expect("decoded fixture");
            let stale_retry = decoded(0.25);
            let superseded_retry = decoded(0.1);
            let latest_retry = decoded(0.2);
            let already_published = decoded(0.5);
            let published_after = decoded(0.75);

            // This publication represents the race window after a producer took
            // its old batch but before it reacquired the ready mutex to retry.
            library.requeue_ready(replaced_id, already_published.clone());
            library.requeue_ready_batch_before_newer(vec![
                (retry_id, superseded_retry),
                (replaced_id, stale_retry),
                (retry_id, latest_retry.clone()),
            ]);
            library.requeue_ready(replaced_id, published_after.clone());

            assert_eq!(
                library.decoded_identity(retry_id),
                Some(latest_retry.identity())
            );
            assert_eq!(
                library.decoded_identity(replaced_id),
                Some(published_after.identity())
            );

            assert_eq!(
                library.take_ready(),
                vec![
                    (retry_id, latest_retry),
                    (replaced_id, already_published),
                    (replaced_id, published_after)
                ]
            );
        }

        /// A one-frame WAV, enough for the scan to call a file audio.
        pub(in crate::samples) fn tiny_wav() -> Vec<u8> {
            let mut wav = Vec::new();
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&40u32.to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16u32.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&48_000u32.to_le_bytes());
            wav.extend_from_slice(&96_000u32.to_le_bytes());
            wav.extend_from_slice(&2u16.to_le_bytes());
            wav.extend_from_slice(&16u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&4u32.to_le_bytes());
            wav.extend_from_slice(&[0, 0, 0, 0]);
            wav
        }

        /// A folder imported in Settings is every set's, and it sits under the
        /// set you are in: a set that has its own `bd` keeps playing its own.
        #[test]
        fn an_imported_folder_is_under_the_open_sets_own_folder() {
            let root = test_dir("imported-folder-precedence");
            std::fs::create_dir_all(root.join("kicks")).expect("fixture");
            std::fs::write(root.join("kicks/1.wav"), tiny_wav()).expect("fixture");
            std::fs::create_dir_all(root.join("bd")).expect("fixture");
            std::fs::write(root.join("bd/0.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::empty();
            let reports = library.adopt_global_sources_settled(&[GlobalSource {
                spec: format!("local:{}", root.display()),
                enabled: true,
            }]);
            assert_eq!(
                reports,
                vec![GlobalSourceReport {
                    spec: format!("local:{}", root.display()),
                    state: GlobalSourceState::Ready { banks: 2 },
                }],
                "each child folder is one bank"
            );
            assert!(library.knows("bd"), "the imported bd is nameable");
            assert!(library.knows("kicks"));
            assert_eq!(library.variants_of("kicks"), Some(1));

            // The set's own folder outranks it, which is the whole point of the
            // layer sitting under `custom`.
            library.custom.write().expect("custom banks").insert(
                "bd".to_owned(),
                Bank::Array(vec![Arc::from("file:///set/bd.wav")]),
            );
            let chosen = library
                .look_up("bd", |named| match named {
                    Named::Bank(Bank::Array(urls)) => urls[0].to_string(),
                    _ => "not an array".to_owned(),
                })
                .expect("bd resolves");
            assert_eq!(chosen, "file:///set/bd.wav", "the set's own bd wins");

            // And it comes back when the set stops claiming the name.
            library.custom.write().expect("custom banks").remove("bd");
            let chosen = library
                .look_up("bd", |named| match named {
                    Named::Bank(Bank::Array(urls)) => urls[0].to_string(),
                    _ => "not an array".to_owned(),
                })
                .expect("bd resolves");
            let chosen = chosen.replace('\\', "/");
            assert!(chosen.ends_with("bd/0.wav") && chosen.contains("imported-folder-precedence"));
            let _ = std::fs::remove_dir_all(root);
        }

        /// What a drop and the picker hand over is a bare path. It has to be a
        /// folder, or nothing anybody imports from the studio ever imports.
        #[test]
        fn a_bare_folder_path_is_a_folder_source() {
            let root = test_dir("bare-path-source");
            std::fs::create_dir_all(&root).expect("fixture");
            std::fs::create_dir_all(root.join("snap")).expect("fixture");
            std::fs::write(root.join("snap/0.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::empty();
            let reports = library.adopt_global_sources_settled(&[GlobalSource {
                spec: root.display().to_string(),
                enabled: true,
            }]);
            assert_eq!(
                reports[0].state,
                GlobalSourceState::Ready { banks: 1 },
                "a bare absolute path is a folder, not a URL: {:?}",
                reports[0].state
            );
            assert!(library.knows("snap"));
            let _ = std::fs::remove_dir_all(root);
        }

        /// A new folder can be added before any audio has been copied into it. It
        /// stays imported and a later scan picks up the files when they arrive.
        #[test]
        fn an_empty_folder_is_a_ready_source_and_can_fill_later() {
            let root = test_dir("empty-source-can-fill-later");
            let source = GlobalSource {
                spec: root.display().to_string(),
                enabled: true,
            };
            let library = SampleLibrary::empty();

            let reports = library.adopt_global_sources_settled(std::slice::from_ref(&source));
            assert_eq!(reports[0].state, GlobalSourceState::Ready { banks: 0 });

            std::fs::write(root.join("kick.wav"), tiny_wav()).expect("sample arrives");
            let reports = library.adopt_global_sources_settled(&[source]);
            assert_eq!(reports[0].state, GlobalSourceState::Ready { banks: 1 });
            assert!(library.knows("kick"));
            let _ = std::fs::remove_dir_all(root);
        }

        /// A pack lands in the imported layer, filed under its source, and a
        /// re-adoption that no longer lists it takes it away.
        #[test]
        fn a_pack_lands_in_the_imported_layer_and_a_later_adoption_can_drop_it() {
            // A relative entry: it resolves against the pack's own origin,
            // which is what the player's grant covers. An absolute entry on
            // another plain-http host is refused, exactly as a score's would be.
            let body = r#"{"tone":"tone.wav"}"#.to_owned();
            let (url, arrival, server) = serve_once("/pack.json", body);

            let library = SampleLibrary::empty();
            // Not the settled reading: this one is about the row that says
            // `fetching…` while the pack is still on its way.
            let reports = library.adopt_global_sources(&[GlobalSource {
                spec: url.clone(),
                enabled: true,
            }]);
            assert_eq!(reports[0].state, GlobalSourceState::Loading);
            arrival
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the pack was asked for");
            library.wait_until_idle(Duration::from_secs(2));
            server.join().expect("pack server");

            assert!(library.knows("tone"), "the pack's sound is nameable");
            assert!(
                library
                    .global
                    .read()
                    .expect("global banks")
                    .contains_key("tone"),
                "it lives in the imported layer"
            );
            assert!(
                !library
                    .custom
                    .read()
                    .expect("custom banks")
                    .contains_key("tone"),
                "and not in the score's"
            );
            assert_eq!(
                library.global_source_reports()[0].state,
                GlobalSourceState::Ready { banks: 1 },
                "the row says what it brought"
            );
            let entry = library
                .catalogue()
                .into_iter()
                .find(|entry| entry.name == "tone")
                .expect("in the browser");
            assert_eq!(entry.origin, SoundOrigin::Global);
            assert_eq!(
                entry.import.as_deref(),
                Some(url.as_str()),
                "filed under its source"
            );

            // Gone from Settings, gone from the library.
            library.adopt_global_sources(&[]);
            assert!(!library.knows("tone"));
        }

        /// A pack's list that arrives after the adoption that asked for it has
        /// been replaced is thrown away, not resurrected.
        #[test]
        fn a_stale_pack_list_does_not_resurrect_a_removed_source() {
            let body = r#"{"late":"late.wav"}"#.to_owned();
            let (url, entered, release, server) = stalled_server(body);

            let library = SampleLibrary::empty();
            library.adopt_global_sources(&[GlobalSource {
                spec: url.clone(),
                enabled: true,
            }]);
            entered
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the pack was asked for");
            // Removed before its list has landed.
            library.adopt_global_sources(&[]);
            release.send(()).expect("release the list");
            server.join().expect("pack server");
            library.wait_until_idle(Duration::from_secs(2));

            assert!(
                !library.knows("late"),
                "a list from a superseded adoption must not fill a row that is gone"
            );
            assert!(library.global_source_reports().is_empty());
        }

        /// A pack that shares a name with the open set's folder leaves the
        /// set's bookkeeping alone: the set's bank stays the set's, and the
        /// next set can still put it down.
        #[test]
        fn a_pack_does_not_untrack_the_sets_own_bank() {
            let set_root = test_dir("pack-vs-set-folder");
            std::fs::create_dir_all(&set_root).expect("fixture");
            std::fs::create_dir_all(set_root.join("bd")).expect("fixture");
            std::fs::write(set_root.join("bd/0.wav"), tiny_wav()).expect("fixture");
            let body = r#"{"bd":"bd.wav"}"#.to_owned();
            let (url, arrival, server) = serve_once("/pack.json", body);

            let library = SampleLibrary::empty();
            library.adopt_set_folder(&set_root).expect("adopt the set");
            assert!(library.set_folder_holds(&set_root, "bd"));
            library.adopt_global_sources(&[GlobalSource {
                spec: url,
                enabled: true,
            }]);
            arrival
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the pack was asked for");
            library.wait_until_idle(Duration::from_secs(2));
            server.join().expect("pack server");

            assert!(
                library.set_folder_holds(&set_root, "bd"),
                "the set's own bd is still the set's"
            );
            assert!(
                library
                    .shared
                    .set_banks
                    .read()
                    .expect("set banks")
                    .contains_key("bd"),
                "the set's displacement record survived the pack"
            );
            let bd = library
                .catalogue()
                .into_iter()
                .find(|entry| entry.name == "bd")
                .expect("bd in the browser");
            assert_eq!(
                bd.origin,
                SoundOrigin::Set,
                "the browser still says whose it is"
            );
            let _ = std::fs::remove_dir_all(set_root);
        }

        /// Colliding sources keep a stable canonical name and preserve the other
        /// source under an automatic alias, independent of arrival order.
        #[test]
        fn a_later_folder_row_outranks_an_earlier_pack() {
            let root = test_dir("folder-below-pack");
            std::fs::create_dir_all(&root).expect("fixture");
            std::fs::create_dir_all(root.join("bd")).expect("fixture");
            std::fs::write(root.join("bd/0.wav"), tiny_wav()).expect("fixture");
            let body = r#"{"bd":"pack-bd.wav"}"#.to_owned();
            let (url, arrival, server) = serve_once("/pack.json", body);

            let library = SampleLibrary::empty();
            library.adopt_global_sources(&[
                GlobalSource {
                    spec: url,
                    enabled: true,
                },
                GlobalSource {
                    spec: root.display().to_string(),
                    enabled: true,
                },
            ]);
            arrival
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the pack was asked for");
            library.wait_until_idle(Duration::from_secs(2));
            server.join().expect("pack server");

            let chosen = library
                .look_up("bd", |named| match named {
                    Named::Bank(Bank::Array(urls)) => urls[0].to_string(),
                    _ => "not an array".to_owned(),
                })
                .expect("bd resolves");
            assert!(chosen.starts_with("file://"), "stable canonical: {chosen}");
            assert!(library.knows("bd_1"), "the pack remains addressable");
            let _ = std::fs::remove_dir_all(root);
        }

        /// The imported layer sits over the fonts and the pinned banks, not
        /// only under the set's own folder.
        #[test]
        fn an_import_outranks_the_fonts_and_the_pinned_banks() {
            let root = test_dir("import-over-pinned");
            std::fs::create_dir_all(&root).expect("fixture");
            std::fs::create_dir_all(root.join("bd")).expect("fixture");
            std::fs::create_dir_all(root.join("gm_piano")).expect("fixture");
            std::fs::write(root.join("bd/0.wav"), tiny_wav()).expect("fixture");
            std::fs::write(root.join("gm_piano/0.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::with_background_loaders(
                HashMap::from([(
                    "bd".to_owned(),
                    Bank::Array(vec![Arc::from("http://127.0.0.1:9/pinned-bd.wav")]),
                )]),
                Vec::new(),
                HashMap::from([(
                    "gm_piano".to_owned(),
                    vec![Arc::from("0000_Piano_sf2_file")],
                )]),
                String::new(),
            )
            .expect("library");
            library.adopt_global_sources_settled(&[GlobalSource {
                spec: root.display().to_string(),
                enabled: true,
            }]);

            let first = |name: &str| {
                library
                    .look_up(name, |named| match named {
                        Named::Bank(Bank::Array(urls)) => urls[0].to_string(),
                        Named::Bank(Bank::Notes(_)) => "keyed".to_owned(),
                        Named::Font(_) => "font".to_owned(),
                    })
                    .expect("resolves")
            };
            assert!(
                first("bd").starts_with("file://"),
                "over the pinned bank: {}",
                first("bd")
            );
            assert!(
                first("gm_piano").starts_with("file://"),
                "over the font: {}",
                first("gm_piano")
            );
            assert_eq!(
                library.variants_of("gm_piano"),
                Some(1),
                "and every asker agrees"
            );
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn colliding_imports_get_stable_source_scoped_aliases() {
            let root = test_dir("collision aliases");
            let alpha = root.join("alpha");
            let beta = root.join("beta");
            std::fs::create_dir_all(&alpha).expect("alpha");
            std::fs::create_dir_all(&beta).expect("beta");
            std::fs::create_dir_all(alpha.join("kick")).expect("alpha bank");
            std::fs::create_dir_all(beta.join("kick")).expect("beta bank");
            std::fs::write(alpha.join("kick/0.wav"), tiny_wav()).expect("alpha kick");
            std::fs::write(beta.join("kick/0.wav"), tiny_wav()).expect("beta kick");
            let library = SampleLibrary::empty();
            let source = |path: &Path| GlobalSource {
                spec: path.display().to_string(),
                enabled: true,
            };
            library.adopt_global_sources_settled(&[source(&beta), source(&alpha)]);
            assert!(library.knows("kick"));
            assert!(library.knows("kick_1"));
            let entries = library.catalogue();
            assert_eq!(
                entries
                    .iter()
                    .find(|entry| entry.name == "kick")
                    .and_then(|entry| entry.import.as_deref()),
                Some(alpha.to_string_lossy().as_ref()),
                "the canonical first source owns the unsuffixed name"
            );
            library.set_source_bank_renames(HashMap::from([(
                (beta.display().to_string(), "kick".to_owned()),
                "thump".to_owned(),
            )]));
            assert!(library.knows("thump"), "the manual source alias wins");
            assert!(library.knows("kick"), "the other source keeps its own name");
            library.set_source_bank_renames(HashMap::new());
            library.adopt_global_sources_settled(&[source(&beta)]);
            assert!(
                library.knows("kick_1"),
                "removal does not rename a live score"
            );
            assert!(!library.knows("kick"), "the vacated base remains a gap");
            assert_eq!(
                library.banks_for_import(&beta.display().to_string()),
                ["kick"]
            );
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn a_parent_silently_owns_and_replaces_its_child_source() {
            let root = test_dir("covered child");
            let child = root.join("drums");
            std::fs::create_dir_all(&child).expect("child");
            std::fs::write(child.join("kick.wav"), tiny_wav()).expect("kick");
            let reports = SampleLibrary::empty().adopt_global_sources_settled(&[
                GlobalSource {
                    spec: child.display().to_string(),
                    enabled: true,
                },
                GlobalSource {
                    spec: root.display().to_string(),
                    enabled: true,
                },
            ]);
            assert_eq!(reports.len(), 1, "the child has no duplicate row");
            assert_eq!(reports[0].spec, root.display().to_string());
            assert_eq!(reports[0].state, GlobalSourceState::Ready { banks: 1 });
            let _ = std::fs::remove_dir_all(root);
        }

        /// The file behind a browser row is that row's own: a numbered sample
        /// is its own file, not the bank's first, and a name no file plays has
        /// no location.
        #[test]
        fn a_sounds_file_location_is_the_variants_own_file() {
            let root = test_dir("file location");
            let kicks = root.join("kicks");
            std::fs::create_dir_all(&kicks).expect("fixture");
            for name in ["a.wav", "b.wav", "c.wav"] {
                std::fs::write(kicks.join(name), tiny_wav()).expect("fixture");
            }
            let library = SampleLibrary::empty();
            library.adopt_set_folder(&root).expect("adopt");

            let first = library.file_location("kicks", None).expect("the bank");
            assert!(first.ends_with("a.wav"), "{first}");
            let third = library
                .file_location("kicks", Some(2))
                .expect("the third sample");
            assert!(third.ends_with("c.wav"), "{third}");
            assert_eq!(
                library.file_location("kicks", Some(9)),
                Some(first),
                "a number past the end falls back to the first file"
            );
            assert_eq!(library.file_location("no-such-sound", Some(0)), None);
            let _ = std::fs::remove_dir_all(root);
        }

        /// The set's own folder answers to the renames too.
        #[test]
        fn a_renamed_set_bank_answers_to_its_new_name() {
            let root = test_dir("rename-set-folder");
            std::fs::create_dir_all(&root).expect("fixture");
            std::fs::create_dir_all(root.join("hats")).expect("fixture");
            std::fs::write(root.join("hats/0.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::empty();
            library.set_bank_renames(HashMap::from([("hats".to_owned(), "ch".to_owned())]));
            library.adopt_set_folder(&root).expect("adopt");
            assert!(library.set_folder_holds(&root, "ch"));
            assert!(!library.knows("hats"));
            let _ = std::fs::remove_dir_all(root);
        }

        /// A rename made after a source landed reaches it without re-adopting:
        /// the rows are read through the overlay again.
        #[test]
        fn a_rename_reaches_an_imported_folder_without_re_adopting() {
            let root = test_dir("rename-after-adopt");
            std::fs::create_dir_all(&root).expect("fixture");
            std::fs::create_dir_all(root.join("hats")).expect("fixture");
            std::fs::write(root.join("hats/0.wav"), tiny_wav()).expect("fixture");
            let library = SampleLibrary::empty();
            library.adopt_global_sources_settled(&[GlobalSource {
                spec: root.display().to_string(),
                enabled: true,
            }]);
            assert!(library.knows("hats"));
            library.set_bank_renames(HashMap::from([("hats".to_owned(), "ch".to_owned())]));
            assert!(library.knows("ch") && !library.knows("hats"));
            let _ = std::fs::remove_dir_all(root);
        }

        /// The set's own folder is found by the spelling the scan gives its
        /// urls - a raw path - so a set with a space in its path still holds
        /// what it holds.
        #[test]
        fn set_folder_holds_survives_a_space_in_the_path() {
            let root = test_dir("set folder with space");
            std::fs::create_dir_all(&root).expect("fixture");
            std::fs::create_dir_all(root.join("bd")).expect("fixture");
            std::fs::write(root.join("bd/0.wav"), tiny_wav()).expect("fixture");
            let library = SampleLibrary::empty();
            library.adopt_set_folder(&root).expect("adopt");
            assert!(
                library.set_folder_holds(&root, "bd"),
                "a space in the set's path must not hide its own sounds"
            );
            let _ = std::fs::remove_dir_all(root);
        }

        /// Eleven files come back in the order they went: the copies are
        /// zero-padded so the scan's name order is the bank's order.
        #[test]
        fn a_consolidated_bank_keeps_its_order_past_ten_files() {
            let source = test_dir("consolidate-order-source");
            let set = test_dir("consolidate-order-set");
            std::fs::create_dir_all(source.join("kit")).expect("fixture");
            std::fs::create_dir_all(&set).expect("fixture");
            for index in 0..11 {
                // Each file is told apart by its length, so order is checkable
                // after the copy.
                let mut wav = tiny_wav();
                wav.extend(std::iter::repeat_n(0u8, index * 2));
                std::fs::write(source.join(format!("kit/{index}.wav")), wav).expect("fixture");
            }
            let library = SampleLibrary::empty();
            library.adopt_global_sources_settled(&[GlobalSource {
                spec: source.display().to_string(),
                enabled: true,
            }]);
            let lengths = |library: &SampleLibrary| -> Vec<usize> {
                library
                    .look_up("kit", |named| match named {
                        Named::Bank(Bank::Array(urls)) => urls
                            .iter()
                            .map(|url| fetch_located(url).expect("read").len())
                            .collect(),
                        _ => Vec::new(),
                    })
                    .expect("kit")
            };
            let before = lengths(&library);
            assert_eq!(library.copy_bank_into("kit", &set), Ok(11));
            library.adopt_set_folder(&set).expect("adopt the set");
            assert_eq!(
                lengths(&library),
                before,
                "`n` picks the same file before and after"
            );
            let _ = std::fs::remove_dir_all(source);
            let _ = std::fs::remove_dir_all(set);
        }

        /// A keyed bank is refused rather than laid out wrong, and a bank with
        /// a file still to download is refused rather than copied with a gap.
        #[test]
        fn consolidating_refuses_what_cannot_round_trip() {
            let set = test_dir("consolidate-refusals");
            std::fs::create_dir_all(&set).expect("fixture");
            let library = SampleLibrary::empty();
            library.banks.write().expect("banks").insert(
                "piano".to_owned(),
                Bank::Notes(vec![(60.0, vec![Arc::from("http://127.0.0.1:9/c4.wav")])]),
            );
            library.banks.write().expect("banks").insert(
                "remote".to_owned(),
                Bank::Array(vec![Arc::from("http://127.0.0.1:9/never.wav")]),
            );
            let keyed = library
                .copy_bank_into("piano", &set)
                .expect_err("a keyed bank is refused");
            assert!(keyed.contains("keyed"), "{keyed}");
            let gap = library
                .copy_bank_into("remote", &set)
                .expect_err("a missing file is refused");
            assert!(gap.contains("1 of 1"), "{gap}");
            assert!(!set.join("piano").exists() && !set.join("remote").exists());
            let _ = std::fs::remove_dir_all(set);
        }

        /// Consolidating never fetches - proven by a server that would have to
        /// see a connection, and does not.
        #[test]
        fn consolidating_never_fetches() {
            let set = test_dir("consolidate-no-fetch");
            std::fs::create_dir_all(&set).expect("fixture");
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let url = format!("http://{}/never.wav", listener.local_addr().unwrap());
            let library = SampleLibrary::empty();
            library.banks.write().expect("banks").insert(
                "remote".to_owned(),
                Bank::Array(vec![Arc::from(url.as_str())]),
            );
            let _ = library.copy_bank_into("remote", &set);
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                "consolidating connected to the network"
            );
            let _ = std::fs::remove_dir_all(set);
        }

        /// A pack's file is copied out of the score cache, where a granted url
        /// is kept.
        #[test]
        fn consolidating_reads_a_granted_url_out_of_the_score_cache() {
            let set = test_dir("consolidate-score-cache");
            std::fs::create_dir_all(&set).expect("fixture");
            let library = SampleLibrary::empty();
            let url = "http://127.0.0.1:9/pack/clap.wav";
            library
                .shared
                .score_sources
                .write()
                .expect("grants")
                .insert(
                    Arc::from(url),
                    ScoreFetchAccess::Remote {
                        rule: SourceRule::parse("http://127.0.0.1:9").expect("a test source"),
                    },
                );
            let path = library.shared.score_cache.path(url, ScoreCacheKind::Audio);
            library
                .shared
                .score_cache
                .admit(&path, &tiny_wav())
                .expect("seed the score cache");
            library
                .banks
                .write()
                .expect("banks")
                .insert("clap".to_owned(), Bank::Array(vec![Arc::from(url)]));
            assert_eq!(library.copy_bank_into("clap", &set), Ok(1));
            assert!(set.join("clap/0.wav").is_file());
            let _ = std::fs::remove_dir_all(set);
        }

        #[test]
        fn consolidating_refuses_a_bank_name_outside_the_set() {
            let source = test_dir("consolidate-escape-source");
            let set = test_dir("consolidate-escape-set");
            let escaped = test_dir("consolidate-escape-target");
            std::fs::write(source.join("clap.wav"), tiny_wav()).expect("fixture");
            let url = local_file_url(&source.join("clap.wav"));
            let escaped_name = escaped.file_name().expect("name").to_string_lossy();
            let name = format!("../{escaped_name}");
            let library = SampleLibrary::empty();
            library
                .banks
                .write()
                .expect("banks")
                .insert(name.clone(), Bank::Array(vec![Arc::from(url.as_str())]));

            let result = library.copy_bank_into(&name, &set);
            let wrote_outside = escaped.join("0.wav").exists();
            let _ = std::fs::remove_dir_all(source);
            let _ = std::fs::remove_dir_all(set);
            let _ = std::fs::remove_dir_all(escaped);

            assert!(result.is_err(), "a parent bank name was accepted");
            assert!(!wrote_outside, "the bank wrote outside the set");
        }

        #[cfg(any(unix, windows))]
        #[test]
        fn consolidating_refuses_a_bank_folder_link_outside_the_set() {
            let source = test_dir("consolidate-link-source");
            let set = test_dir("consolidate-link-set");
            let target = test_dir("consolidate-link-target");
            std::fs::write(source.join("clap.wav"), tiny_wav()).expect("fixture");
            let url = local_file_url(&source.join("clap.wav"));
            let library = SampleLibrary::empty();
            library
                .banks
                .write()
                .expect("banks")
                .insert("kit".to_owned(), Bank::Array(vec![Arc::from(url.as_str())]));
            let link = set.join("kit");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &link).expect("directory link");
            #[cfg(windows)]
            {
                let link_text = link.display().to_string();
                let target_text = target.display().to_string();
                let output = std::process::Command::new("cmd")
                    .args(["/C", "mklink", "/J", &link_text, &target_text])
                    .output()
                    .expect("run mklink");
                assert!(
                    output.status.success(),
                    "create a junction: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }

            let result = library.copy_bank_into("kit", &set);
            let wrote_outside = target.join("0.wav").exists();
            let _ = std::fs::remove_dir_all(source);
            let _ = std::fs::remove_dir_all(set);
            let _ = std::fs::remove_dir_all(target);

            assert!(result.is_err(), "a linked bank folder was accepted");
            assert!(!wrote_outside, "the bank wrote through the link");
        }

        /// The extension a copy gets is the one the scan will read back, taken
        /// from the path rather than from a query or a fragment.
        #[test]
        fn a_copy_is_named_by_what_the_scan_can_read() {
            assert!(is_sample_audio(Path::new("0.mp3")));
            assert!(!is_sample_audio(Path::new("0.mp3#v")));
            assert!(matches!(codec_for("http://x/kick.mp3?v=2"), Codec::Mp3));
            assert!(matches!(codec_for("http://x/kick.wav#frag"), Codec::Wav));
        }

        /// Emptying the cache removes the cache's own entries and nothing else:
        /// not its lock, not its marker, not a file somebody kept beside it.
        #[test]
        fn clearing_the_cache_removes_only_what_the_cache_wrote() {
            let base = test_dir("clear-cache-scope");
            std::fs::create_dir_all(&base).expect("fixture");
            // The cache writer names the entries, so the guard is tested against
            // the names the cache really writes.
            let entry = cache_path(&base, "https://example.test/kick.wav");
            let bare = cache_path(&base, "https://example.test/manifest");
            let foreign = base.join("my-notes.txt");
            let dir = base.join("keep");
            std::fs::create_dir_all(&dir).expect("fixture");
            for path in [&entry, &bare, &foreign] {
                std::fs::write(path, b"x").expect("fixture");
            }
            assert_ne!(entry, bare, "two urls, two entries");
            assert!(sample_cache_usage_at(&base) >= 3);
            clear_sample_cache_at(&base).expect("clear");
            assert!(
                !entry.exists() && !bare.exists(),
                "the cache's own entries go"
            );
            assert!(foreign.exists(), "a foreign file stays");
            assert!(dir.exists(), "a folder stays");
            assert!(
                base.join(SCORE_CACHE_NO_LEGACY_MARKER).exists(),
                "the score cache's marker is left standing"
            );
            let _ = std::fs::remove_dir_all(base);
        }

        /// A source that will not read is a row that says so, never a studio
        /// that will not open: an unplugged drive must cost you its sounds and
        /// nothing else.
        #[test]
        fn a_missing_source_is_reported_and_the_others_still_import() {
            let root = test_dir("missing-source-is-not-fatal");
            std::fs::create_dir_all(&root).expect("fixture");
            std::fs::write(root.join("clap.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::empty();
            let reports = library.adopt_global_sources_settled(&[
                GlobalSource {
                    spec: "local:/no/such/folder/anywhere".to_owned(),
                    enabled: true,
                },
                GlobalSource {
                    spec: format!("local:{}", root.display()),
                    enabled: true,
                },
                GlobalSource {
                    spec: "local:/turned/off".to_owned(),
                    enabled: false,
                },
            ]);
            assert!(matches!(reports[0].state, GlobalSourceState::Missing(_)));
            assert_eq!(reports[1].state, GlobalSourceState::Ready { banks: 1 });
            assert_eq!(reports[2].state, GlobalSourceState::Off);
            assert!(library.knows("clap"), "the readable source still imported");
            let _ = std::fs::remove_dir_all(root);
        }

        /// A folder batch whose budget is spent must not leave an unwalked row
        /// on `Loading`. Each such row reports the walk's name instead. The test
        /// calls the folder walk directly with a spent budget, which is
        /// deterministic.
        #[test]
        fn a_spent_budget_says_so_on_every_folder_it_never_walked() {
            let first = test_dir("spent-budget-folder-a");
            let second = test_dir("spent-budget-folder-b");
            std::fs::create_dir_all(&first).expect("fixture");
            std::fs::create_dir_all(&second).expect("fixture");
            std::fs::write(first.join("kick.wav"), tiny_wav()).expect("fixture");
            std::fs::write(second.join("snap.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::empty();
            let specs = vec![
                format!("local:{}", first.display()),
                format!("local:{}", second.display()),
            ];
            // The rows are staged straight on, not adopted: an adoption would
            // enqueue a real walk on a fine budget, and the always-running worker
            // could land it either side of the spent call below - a race, not a
            // result.
            *library
                .shared
                .global_slots
                .lock()
                .expect("global source slots") = specs
                .iter()
                .enumerate()
                .map(|(row, spec)| GlobalSlot {
                    row,
                    spec: spec.clone(),
                    kind: GlobalKind::Folder,
                    banks: HashMap::new(),
                    state: GlobalSourceState::Loading,
                    rewalk: false,
                })
                .collect::<Vec<_>>();
            let generation = library
                .shared
                .global_generation
                .fetch_add(1, Ordering::AcqRel)
                + 1;

            // A spent budget: the check between folders is the walk's first act.
            let budget = rustel_sample_fetch_deadline_in_past();
            let _ = library.run_folders_work_for_test(&specs, generation, &budget);

            let reports = library.global_source_reports();
            for report in reports.iter().filter(|report| specs.contains(&report.spec)) {
                assert!(
                    matches!(report.state, GlobalSourceState::Failed(_)),
                    "{} was left on {:?} with nothing coming to fill it",
                    report.spec,
                    report.state
                );
            }
            assert!(
                !library.knows("kick") && !library.knows("snap"),
                "a walk that never happened brings no sounds"
            );
            let _ = std::fs::remove_dir_all(first);
            let _ = std::fs::remove_dir_all(second);
        }

        /// The same spent budget through the whole worker, not just the folder
        /// arm: the deadline error surfaces as the job's result, and no later
        /// folder steals a walk the batch had already lost.
        #[test]
        fn a_folder_batch_that_runs_out_of_budget_aborts_between_folders_not_mid_row() {
            let first = test_dir("abort-between-folders-a");
            let second = test_dir("abort-between-folders-b");
            std::fs::create_dir_all(&first).expect("fixture");
            std::fs::create_dir_all(&second).expect("fixture");
            std::fs::write(first.join("kick.wav"), tiny_wav()).expect("fixture");
            std::fs::write(second.join("snap.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::empty();
            let specs = vec![
                format!("local:{}", first.display()),
                format!("local:{}", second.display()),
            ];
            // The rows are staged straight on, not adopted: an adoption would
            // enqueue a real walk on a fine budget, and the always-running worker
            // could land it either side of the spent call below - a race, not a
            // result.
            *library
                .shared
                .global_slots
                .lock()
                .expect("global source slots") = specs
                .iter()
                .enumerate()
                .map(|(row, spec)| GlobalSlot {
                    row,
                    spec: spec.clone(),
                    kind: GlobalKind::Folder,
                    banks: HashMap::new(),
                    state: GlobalSourceState::Loading,
                    rewalk: false,
                })
                .collect::<Vec<_>>();
            let generation = library
                .shared
                .global_generation
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            // Through the whole worker, not just the folder arm: a real job on a
            // real queue, its verdict read from the completion the caller owns.
            // A job with no time at all is spent the moment the worker takes it.
            let context = library.manifest_context();
            let (queue, jobs) = manifest_queue::manifest_queue();
            let worker = std::thread::spawn(move || run_manifest_worker(context, jobs));
            // enqueue marks pending before it sends and the worker unpends when
            // the job is finished; the count is kept honest the same way here.
            library
                .shared
                .manifest_pending
                .fetch_add(1, Ordering::AcqRel);
            let (completion, verdict) = mpsc::sync_channel(1);
            queue
                .push(Box::new(ManifestJob {
                    work: ManifestWork::Folders {
                        specs: specs.clone(),
                        generation,
                    },
                    timeout: Duration::ZERO,
                    completion: Some(completion),
                }))
                .expect("the empty queue takes the batch");
            queue.close();
            let result = verdict
                .recv_timeout(Duration::from_secs(5))
                .expect("the worker answers a queued batch");
            worker
                .join()
                .expect("the worker ends when its queue closes");
            assert_eq!(
                result.expect_err("a spent budget refuses the batch"),
                "sample manifest deadline exceeded"
            );
            let _ = std::fs::remove_dir_all(first);
            let _ = std::fs::remove_dir_all(second);
        }

        /// Re-adopting replaces rather than accumulates: a source removed in
        /// Settings takes its sounds with it.
        #[test]
        fn re_adopting_drops_the_sources_that_are_gone() {
            let root = test_dir("re-adopt-drops-old");
            std::fs::create_dir_all(&root).expect("fixture");
            std::fs::write(root.join("rim.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::empty();
            let source = GlobalSource {
                spec: format!("local:{}", root.display()),
                enabled: true,
            };
            library.adopt_global_sources_settled(std::slice::from_ref(&source));
            assert!(library.knows("rim"));
            library.adopt_global_sources_settled(&[]);
            assert!(!library.knows("rim"), "the removed source took its sounds");
            let _ = std::fs::remove_dir_all(root);
        }

        /// Consolidating copies what the scores name into the set's folder,
        /// where the set's own scan then answers for it - no score is
        /// rewritten, because the folder already wins by name.
        #[test]
        fn copying_a_bank_into_the_set_lays_it_out_the_way_the_scan_reads_it() {
            let library_root = test_dir("consolidate-source");
            let set_root = test_dir("consolidate-set");
            std::fs::create_dir_all(&library_root).expect("fixture");
            std::fs::create_dir_all(&set_root).expect("fixture");
            std::fs::write(library_root.join("clap.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::empty();
            library.adopt_global_sources_settled(&[GlobalSource {
                spec: format!("local:{}", library_root.display()),
                enabled: true,
            }]);
            assert!(library.knows("clap"));
            assert!(
                !library.set_folder_holds(&set_root, "clap"),
                "the set does not carry it yet"
            );

            assert_eq!(library.copy_bank_into("clap", &set_root), Ok(1));
            // The scan's rule read backwards: a folder named for the bank, with
            // the bank's files inside in the order it had them.
            assert!(set_root.join("clap/0.wav").is_file());

            // And the set now answers for it out of its own folder, which is
            // what makes the copied set play on another machine.
            library.adopt_set_folder(&set_root).expect("adopt");
            assert!(library.set_folder_holds(&set_root, "clap"));

            let _ = std::fs::remove_dir_all(library_root);
            let _ = std::fs::remove_dir_all(set_root);
        }

        /// A rename is an overlay on what a source brings: the files keep their
        /// own names, and taking the rename away brings the old name back.
        #[test]
        fn a_renamed_import_answers_to_the_new_name_and_gives_it_back() {
            let root = test_dir("rename-overlay");
            std::fs::create_dir_all(&root).expect("fixture");
            std::fs::create_dir_all(root.join("hats")).expect("fixture");
            std::fs::write(root.join("hats/0.wav"), tiny_wav()).expect("fixture");

            let library = SampleLibrary::empty();
            let source = GlobalSource {
                spec: format!("local:{}", root.display()),
                enabled: true,
            };
            library.set_bank_renames(HashMap::from([("hats".to_owned(), "ch".to_owned())]));
            library.adopt_global_sources_settled(std::slice::from_ref(&source));
            assert!(library.knows("ch"), "it plays under the new name");
            assert!(!library.knows("hats"), "and not under the old one");

            library.set_bank_renames(HashMap::new());
            library.adopt_global_sources_settled(std::slice::from_ref(&source));
            assert!(library.knows("hats"), "removing the rename gives it back");
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn peeking_ready_sample_keeps_the_current_payload_alive_without_draining_it() {
            let library = SampleLibrary::empty();
            let id = SampleId(9);
            let older = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("older decode");
            let newer = DecodedSample::from_parts(48_000, 1, vec![0.5]).expect("newer decode");
            assert!(library.peek_ready_sample(id).is_none());

            library.requeue_ready(id, older);
            library.requeue_ready(id, newer.clone());
            let retained = library.peek_ready_sample(id).expect("queued sample");
            assert_eq!(retained.identity(), newer.identity());
            let installing = library.take_ready();
            assert_eq!(installing.len(), 2, "peeking must leave the queue intact");
            assert!(library.peek_ready_sample(id).is_none());
            drop(installing);
            assert_eq!(retained, newer, "the in-flight clone keeps its PCM alive");

            library.requeue_ready(id, retained);
            assert_eq!(
                library
                    .peek_ready_sample(id)
                    .expect("deferred install")
                    .identity(),
                newer.identity()
            );
        }

        #[test]
        fn older_retry_cannot_replace_identity_after_newer_pcm_has_left_the_queue() {
            let library = SampleLibrary::empty();
            let id = SampleId(9);
            let older = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("older decode");
            let newer = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("newer decode");
            assert_eq!(older, newer, "content equality is independent of identity");
            assert!(newer.identity() > older.identity());

            library.requeue_ready(id, older.clone());
            let retry = library.take_ready();
            library.requeue_ready(id, newer.clone());
            assert_eq!(library.decoded_identity(id), Some(newer.identity()));
            let installing = library.take_ready();
            assert_eq!(installing[0].1.identity(), newer.identity());

            library.requeue_ready_batch_before_newer(retry);
            library.requeue_ready(id, older);
            assert_eq!(library.decoded_identity(id), Some(newer.identity()));
            assert!(library.take_ready().is_empty(), "stale PCM was requeued");
        }

        #[test]
        fn deferred_install_keeps_identity_for_every_unattempted_sample() {
            let library = SampleLibrary::empty();
            let first = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("first decode");
            let later = DecodedSample::from_parts(48_000, 1, vec![0.5]).expect("later decode");
            library.requeue_ready(SampleId(8), first.clone());
            library.requeue_ready(SampleId(9), later.clone());

            // The whole batch leaves the library before any install is attempted.
            // Refusing its first entry must not hide the later intended payload.
            let deferred = library.take_ready();
            assert_eq!(
                library.decoded_identity(SampleId(8)),
                Some(first.identity())
            );
            assert_eq!(
                library.decoded_identity(SampleId(9)),
                Some(later.identity())
            );
            library.requeue_ready_batch_before_newer(deferred);
            assert_eq!(
                library.decoded_identity(SampleId(9)),
                Some(later.identity())
            );
            assert_eq!(library.take_ready().len(), 2);
        }

        #[test]
        fn recovery_requeue_preserves_cloned_payload_identity() {
            let library = SampleLibrary::empty();
            let id = SampleId(8);
            let retained =
                DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("decoded sample");
            library.requeue_ready(id, retained.clone());
            let initial = library.take_ready();
            assert_eq!(initial[0].1.identity(), retained.identity());
            drop(initial);

            library.requeue_ready_batch_before_newer(vec![(id, retained.clone())]);
            let reopened = library.take_ready();
            assert_eq!(library.decoded_identity(id), Some(retained.identity()));
            assert_eq!(reopened[0].1.identity(), retained.identity());
            assert_eq!(
                library.decoded_identity(BUNDLED_BD_SAMPLE_ID),
                Some(BUNDLED_BD_SAMPLE_IDENTITY)
            );
            assert_eq!(library.decoded_identity(SampleId(7)), None);
            assert_eq!(
                library.decoded_identity(SampleId(SAMPLE_BANK_CAPACITY as u32)),
                None
            );
        }

        /// Released ids are the next ones reserved, oldest first, before the
        /// counter moves - and a request the bank cannot seat whole takes
        /// nothing, so a font is never half-reserved.
        #[test]
        fn reserve_takes_released_ids_oldest_first_before_advancing_the_counter() {
            let library = SampleLibrary::empty();
            let counter_before = library.shared.next_id.load(Ordering::Relaxed);
            for id in [SampleId(5), SampleId(9)] {
                library.shared.ready.lock().expect("ready").identities[id.0 as usize] =
                    DecodedIdentity::Forgotten;
            }
            library.release_ids([SampleId(5), SampleId(9)]);
            assert_eq!(library.free_id_count(), 2);

            assert_eq!(
                reserve_sample_ids(&library.shared, 1),
                Ok(vec![SampleId(5)])
            );
            assert_eq!(
                library.shared.next_id.load(Ordering::Relaxed),
                counter_before,
                "a reissued id does not move the counter"
            );
            let three = reserve_sample_ids(&library.shared, 3).expect("three");
            assert_eq!(three[0], SampleId(9), "the older released id goes first");
            assert_eq!(three[1], SampleId(counter_before));
            assert_eq!(three[2], SampleId(counter_before + 1));

            // Nothing is taken when the whole request cannot be seated.
            library.shared.ready.lock().expect("ready").identities[11] = DecodedIdentity::Forgotten;
            library.release_ids([SampleId(11)]);
            library
                .shared
                .next_id
                .store(SAMPLE_BANK_CAPACITY as u32 - 1, Ordering::Relaxed);
            assert!(reserve_sample_ids(&library.shared, 3).is_err());
            assert_eq!(library.free_id_count(), 1, "the released id is still there");
            assert_eq!(
                library.shared.next_id.load(Ordering::Relaxed),
                SAMPLE_BANK_CAPACITY as u32 - 1,
                "and the counter did not move"
            );
        }

        /// Only an id the library has forgotten can be handed back: one a table
        /// still promises, the bundled bd, or one never known are refused, and a
        /// second release is a no-op.
        #[test]
        fn release_refuses_ids_still_promised_and_the_bundled_bd() {
            let library = SampleLibrary::empty();
            let promised = SampleId(4);
            library.requeue_ready(
                promised,
                DecodedSample::from_parts(48_000, 1, vec![0.0; 4]).expect("pcm"),
            );
            library.release_ids([promised, BUNDLED_BD_SAMPLE_ID, SampleId(77)]);
            assert_eq!(
                library.free_id_count(),
                0,
                "nothing forgotten, nothing freed"
            );

            library.shared.ready.lock().expect("ready").identities[6] = DecodedIdentity::Forgotten;
            library.release_ids([SampleId(6)]);
            library.release_ids([SampleId(6)]);
            assert_eq!(library.free_id_count(), 1, "a second release is a no-op");
        }

        /// A released id accepts the next decode like a fresh slot: the
        /// tombstone that refused a stale retry is gone with the release.
        #[test]
        fn a_released_id_accepts_the_next_decode() {
            let library = SampleLibrary::empty();
            let id = SampleId(8);
            let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 4]).expect("pcm");
            library.shared.ready.lock().expect("ready").identities[id.0 as usize] =
                DecodedIdentity::Forgotten;
            library.requeue_ready(id, pcm());
            assert!(
                library.take_ready().is_empty(),
                "a forgotten id refuses PCM"
            );
            library.release_ids([id]);
            let fresh = pcm();
            let identity = fresh.identity();
            library.requeue_ready(id, fresh);
            let taken = library.take_ready();
            assert_eq!(taken.len(), 1, "a released id takes the next decode");
            assert_eq!(taken[0].0, id);
            assert_eq!(library.decoded_identity(id), Some(identity));
        }

        /// A fetch that fails hands its id straight back: nothing ever saw it.
        #[test]
        fn a_failed_fetch_hands_its_id_back() {
            let url: Arc<str> = Arc::from("http://127.0.0.1:9/never.wav");
            let library = SampleLibrary::with_background_loaders(
                HashMap::from([("never".to_owned(), Bank::Array(vec![url.clone()]))]),
                Vec::new(),
                HashMap::new(),
                String::new(),
            )
            .expect("library");
            assert_eq!(library.readiness("never", 0.0), SoundReadiness::Loading);
            library.wait_until_idle(Duration::from_secs(5));
            assert!(matches!(
                library.shared.by_url.read().expect("url table").get(&url),
                Some(UrlState::Failed { .. })
            ));
            assert_eq!(library.free_id_count(), 1, "the id came back");
            let reissued = reserve_sample_ids(&library.shared, 1).expect("one");
            assert_eq!(reissued, vec![SampleId(1)], "and is the next one reserved");
        }

        /// A font that failed rests, then a real ask tries again; a bet does
        /// not, and a fresh failure is left to rest.
        #[test]
        fn a_rested_failed_font_is_asked_again_by_a_real_ask() {
            let font: Arc<str> = Arc::from("0000_Test_sf2_file");
            let library = SampleLibrary::with_background_loaders(
                HashMap::new(),
                Vec::new(),
                HashMap::from([("gm_test".to_owned(), vec![font.clone()])]),
                String::new(),
            )
            .expect("library");
            let rested = Instant::now() - FAILED_RETRY_AFTER - Duration::from_secs(1);
            library
                .shared
                .fonts
                .write()
                .expect("fonts")
                .insert(font.clone(), FontState::Failed { at: rested });

            assert!(matches!(
                library.resolve_with_priority("gm_test", 0.0, 60.0, LoadPriority::Bet),
                SampleResolution::Failed
            ));
            assert!(
                matches!(
                    library.shared.fonts.read().expect("fonts").get(&font),
                    Some(FontState::Failed { .. })
                ),
                "a bet does not spend the retry"
            );
            assert!(matches!(
                library.resolve_with_priority("gm_test", 0.0, 60.0, LoadPriority::Now),
                SampleResolution::Loading
            ));
            assert!(
                matches!(
                    library.shared.fonts.read().expect("fonts").get(&font),
                    Some(FontState::Loading)
                ),
                "a real ask after the rest asks again"
            );

            library
                .shared
                .fonts
                .write()
                .expect("fonts")
                .insert(font.clone(), FontState::Failed { at: Instant::now() });
            assert!(
                matches!(
                    library.resolve_with_priority("gm_test", 0.0, 60.0, LoadPriority::Now),
                    SampleResolution::Failed
                ),
                "a fresh failure is left to rest"
            );
        }

        /// The batch lookup the memory policy uses finds what asking name by
        /// name finds: exact spellings first in table order, then any case.
        #[test]
        fn many_names_at_once_resolve_as_one_at_a_time_does() {
            let library = SampleLibrary::empty();
            let url =
                |name: &str| -> Arc<str> { Arc::from(format!("http://127.0.0.1:9/{name}.wav")) };
            let bank = |name: &str| Bank::Array(vec![url(name)]);
            library
                .custom
                .write()
                .expect("custom")
                .insert("Kick".to_owned(), bank("custom-kick"));
            {
                let mut global = library.global.write().expect("global");
                global.insert("kick".to_owned(), bank("global-kick"));
                global.insert("Snare".to_owned(), bank("global-snare"));
            }
            {
                let mut banks = library.banks.write().expect("banks");
                banks.insert("snare".to_owned(), bank("default-snare"));
                banks.insert("HH".to_owned(), bank("default-hh"));
            }
            {
                let mut by_url = library.shared.by_url.write().expect("url table");
                for (id, name) in [
                    "custom-kick",
                    "global-kick",
                    "global-snare",
                    "default-snare",
                    "default-hh",
                ]
                .into_iter()
                .enumerate()
                {
                    by_url.insert(
                        url(name),
                        UrlState::Ready {
                            id: SampleId(id as u32 + 1),
                            duration_secs: 1.0,
                        },
                    );
                }
            }
            let names = ["Kick", "kick", "KICK", "snare", "SNARE", "hh", "Hh", "sine"];
            for name in names {
                assert_eq!(
                    library.peek_ready_ids_of([name]),
                    library.peek_ready_ids(name).into_iter().collect(),
                    "{name}"
                );
            }
            assert_eq!(
                library.peek_ready_ids_of(names),
                names
                    .iter()
                    .flat_map(|name| library.peek_ready_ids(name))
                    .collect()
            );
            assert_eq!(
                library.peek_ready_ids_of(["KICK"]),
                HashSet::from([SampleId(1)]),
                "the first table with any spelling of a name answers for it"
            );
        }

        /// A host that drops decoded PCM has to be able to hand the name back:
        /// nothing here keeps the samples, so a `Ready` url nobody holds is a
        /// sound that can never be played again.
        #[test]
        fn a_forgotten_url_is_asked_for_again_rather_than_reported_ready() {
            let library = SampleLibrary::empty();
            let url: Arc<str> = Arc::from("http://127.0.0.1:9/tone.wav");
            let other: Arc<str> = Arc::from("http://127.0.0.1:9/kept.wav");
            library.banks.write().expect("banks").insert(
                "tone".to_owned(),
                Bank::Array(vec![url.clone(), other.clone()]),
            );
            {
                let mut by_url = library.shared.by_url.write().expect("url table");
                by_url.insert(
                    url.clone(),
                    UrlState::Ready {
                        id: SampleId(7),
                        duration_secs: 1.0,
                    },
                );
                by_url.insert(
                    other.clone(),
                    UrlState::Ready {
                        id: SampleId(8),
                        duration_secs: 1.0,
                    },
                );
            }
            assert_eq!(
                library.peek_ready_ids("tone"),
                vec![SampleId(7), SampleId(8)]
            );
            let retired = DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("retired decode");
            let kept = DecodedSample::from_parts(48_000, 1, vec![0.5]).expect("kept decode");
            library.requeue_ready(SampleId(7), retired.clone());
            library.requeue_ready(SampleId(8), kept.clone());

            let forgotten = library.forget_decoded(&HashSet::from([SampleId(7)]));
            assert_eq!(forgotten, vec![SampleId(7)]);
            assert_eq!(library.decoded_identity(SampleId(7)), None);
            assert_eq!(library.decoded_identity(SampleId(8)), Some(kept.identity()));
            library.requeue_ready_batch_before_newer(vec![(SampleId(7), retired.clone())]);
            library.requeue_ready(SampleId(7), retired);
            assert_eq!(library.decoded_identity(SampleId(7)), None);
            assert_eq!(library.take_ready(), vec![(SampleId(8), kept)]);
            assert_eq!(
                library.peek_ready_ids("tone"),
                vec![SampleId(8)],
                "only the dropped url is forgotten"
            );
            assert!(
                !library
                    .shared
                    .by_url
                    .read()
                    .expect("url table")
                    .contains_key(&url),
                "a forgotten url must be unknown again, so the next ask loads it"
            );
        }

        /// A released id is the next one reserved. A full bank reports the
        /// failure once, rests, and the next real ask after the rest tries again.
        #[test]
        fn evicting_previews_drains_sample_ids_until_new_sounds_fail_silently() {
            // This test reads the job queue directly, so it must not share it
            // with a loader that pops jobs in order to work on them.
            let library = SampleLibrary::empty_without_loading();
            let old_url: Arc<str> = Arc::from("http://127.0.0.1:9/old.wav");
            library
                .banks
                .write()
                .expect("banks")
                .insert("old".to_owned(), Bank::Array(vec![old_url.clone()]));
            let old_id = SampleId(5);
            library.shared.by_url.write().expect("url table").insert(
                old_url.clone(),
                UrlState::Ready {
                    id: old_id,
                    duration_secs: 1.0,
                },
            );
            library.requeue_ready(
                old_id,
                DecodedSample::from_parts(48_000, 1, vec![0.0; 8]).expect("pcm"),
            );
            let _ = library.take_ready();
            library
                .shared
                .next_id
                .store(SAMPLE_BANK_CAPACITY as u32 - 2, Ordering::Relaxed);

            // Evicted and handed back: the next ask reuses the id rather than
            // taking a fresh one.
            assert_eq!(
                library.forget_decoded(&HashSet::from([old_id])),
                vec![old_id]
            );
            library.release_ids([old_id]);
            assert_eq!(library.readiness("old", 0.0), SoundReadiness::Loading);
            let job = library.shared.jobs.try_pop().expect("a load was queued");
            match job.kind {
                LoadKind::Install { id, .. } => {
                    assert_eq!(id, old_id, "the released id is reissued first")
                }
                LoadKind::Cache => panic!("eviction retry must seat PCM, not only cache bytes"),
            }
            assert_eq!(
                library.shared.next_id.load(Ordering::Relaxed),
                SAMPLE_BANK_CAPACITY as u32 - 2,
                "the counter did not move for a reissued id"
            );

            // A full bank fails out loud, once, and rests.
            library
                .shared
                .next_id
                .store(SAMPLE_BANK_CAPACITY as u32, Ordering::Relaxed);
            let fresh_url: Arc<str> = Arc::from("http://127.0.0.1:9/fresh.wav");
            library
                .banks
                .write()
                .expect("banks")
                .insert("fresh".to_owned(), Bank::Array(vec![fresh_url.clone()]));
            assert_eq!(library.readiness("fresh", 0.0), SoundReadiness::Failed);
            let failures = library.take_failures();
            assert_eq!(
                failures
                    .iter()
                    .filter(|line| line.contains("fresh.wav"))
                    .count(),
                1,
                "said once: {failures:?}"
            );
            assert_eq!(library.readiness("fresh", 0.0), SoundReadiness::Failed);
            assert!(library.take_failures().is_empty(), "and not again per ask");
            assert!(
                matches!(
                    library
                        .shared
                        .by_url
                        .read()
                        .expect("url table")
                        .get(&fresh_url),
                    Some(UrlState::Failed { .. })
                ),
                "the table remembers the failure, so the next onset does not re-fail it"
            );

            // A released id after the rest is what the retry takes.
            library.shared.by_url.write().expect("url table").insert(
                fresh_url.clone(),
                UrlState::Failed {
                    at: Instant::now() - FAILED_RETRY_AFTER,
                },
            );
            let spare = SampleId(7);
            library.shared.ready.lock().expect("ready").identities[spare.0 as usize] =
                DecodedIdentity::Forgotten;
            library.release_ids([spare]);
            assert!(
                matches!(
                    library.resolve_with_priority("fresh", 0.0, 60.0, LoadPriority::Now),
                    SampleResolution::Loading
                ),
                "a real ask after the rest tries again"
            );
            let job = library.shared.jobs.try_pop().expect("a load was queued");
            match job.kind {
                LoadKind::Install { id, .. } => assert_eq!(id, spare),
                LoadKind::Cache => panic!("a real ask after rest must seat PCM"),
            }
        }

        /// A soundfont is one decode over one run of ids. Dropping a zone of it
        /// under the preview ceiling retires the whole font: what is left plays
        /// only the keys those zones cover, and the caller is told which ids it
        /// is now holding for nothing.
        #[test]
        fn forgetting_one_zone_forgets_its_whole_font() {
            let library = SampleLibrary::empty();
            let zone = |id| FontZone {
                key_lo: 0.0,
                key_hi: 127.0,
                base_detune: 0.0,
                id,
                duration_secs: 1.0,
                loop_secs: None,
            };
            let kept: Arc<str> = Arc::from("other.sf.js");
            library.shared.fonts.write().expect("fonts").insert(
                Arc::from("piano.sf.js"),
                FontState::Ready(Arc::new(vec![zone(SampleId(3)), zone(SampleId(4))])),
            );
            library.shared.fonts.write().expect("fonts").insert(
                kept.clone(),
                FontState::Ready(Arc::new(vec![zone(SampleId(5))])),
            );
            for id in [SampleId(3), SampleId(4), SampleId(5)] {
                library.requeue_ready(
                    id,
                    DecodedSample::from_parts(48_000, 1, vec![0.25]).expect("decoded zone"),
                );
            }

            let mut forgotten = library.forget_decoded(&HashSet::from([SampleId(4)]));
            forgotten.sort_by_key(|id| id.0);
            assert_eq!(
                forgotten,
                vec![SampleId(3), SampleId(4)],
                "the untouched zone comes back so the caller can drop it too"
            );
            assert_eq!(library.decoded_identity(SampleId(3)), None);
            assert_eq!(library.decoded_identity(SampleId(4)), None);
            assert!(library.decoded_identity(SampleId(5)).is_some());
            let ready = library.take_ready();
            assert_eq!(ready.len(), 1);
            assert_eq!(ready[0].0, SampleId(5));
            let fonts = library.shared.fonts.read().expect("fonts");
            assert!(!fonts.contains_key(&Arc::from("piano.sf.js")));
            assert!(
                fonts.contains_key(&kept),
                "a font with no dropped zone is left alone"
            );
        }

        /// Serve one body once, at a url no earlier process can have cached.
        ///
        /// The word over the channel is what a test waits on, with a deadline:
        /// a server thread blocked in `accept()` that nobody ever connects to
        /// would otherwise hang the whole binary on `join()`.
        fn serve_once(
            path: &str,
            body: String,
        ) -> (String, mpsc::Receiver<()>, std::thread::JoinHandle<()>) {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
            let unique_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let url = format!(
                "http://{}/{}-{}-{unique_time}{path}",
                listener.local_addr().unwrap(),
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            );
            let (arrived, arrival) = mpsc::sync_channel(1);
            let thread = std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("accept");
                let stream = drain_request(stream);
                let _ = arrived.send(());
                respond(stream, &body);
            });
            (url, arrival, thread)
        }

        fn stalled_server(
            body: String,
        ) -> (
            String,
            mpsc::Receiver<()>,
            mpsc::SyncSender<()>,
            std::thread::JoinHandle<()>,
        ) {
            static NEXT_URL: AtomicUsize = AtomicUsize::new(0);
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
            // The trusted manifest cache outlives this test process. A fixed
            // path can hit bytes left by an earlier process when the OS reuses its
            // ephemeral port. Include wall time as well as the process-local
            // counter: a self-hosted runner may reuse both PIDs and ports across
            // jobs, while the fixture must always exercise its server.
            let unique_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let url = format!(
                "http://{}/strudel-{}-{unique_time}-{}.json",
                listener.local_addr().unwrap(),
                std::process::id(),
                NEXT_URL.fetch_add(1, Ordering::Relaxed)
            );
            let (entered, entered_rx) = mpsc::sync_channel(1);
            let (release, release_rx) = mpsc::sync_channel(1);
            let thread = std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("accept");
                let stream = drain_request(stream);
                entered.send(()).expect("announce request");
                release_rx.recv().expect("release response");
                respond(stream, &body);
            });
            (url, entered_rx, release, thread)
        }

        #[test]
        fn session_manifest_io_is_background_work_and_counts_as_pending() {
            let body = r#"{"tone":"http://127.0.0.1:9/tone.wav"}"#.to_owned();
            let (url, entered, release, server) = stalled_server(body);
            let library = SampleLibrary::empty();
            let started = Instant::now();
            async_register(
                &library,
                vec![(serde_json::to_string(&url).unwrap(), None)],
                &[],
            )
            .expect("enqueue manifest");
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "registration waited for the server: {:?}",
                started.elapsed()
            );
            entered.recv_timeout(ARRIVAL_TIMEOUT).expect("request");

            let waited = Instant::now();
            library.wait_until_idle(Duration::from_millis(40));
            assert!(
                waited.elapsed() >= Duration::from_millis(30),
                "manifest work was omitted from idle accounting"
            );
            assert_ne!(library.shared.manifest_pending.load(Ordering::Acquire), 0);

            release.send(()).unwrap();
            server.join().unwrap();
            library.wait_until_idle(Duration::from_secs(2));
            assert!(library.knows("tone"));
            assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
        }

        #[test]
        fn public_register_custom_blocks_and_returns_fetch_errors() {
            let body = r#"{"tone":"http://127.0.0.1:9/tone.wav"}"#.to_owned();
            let (url, entered, release, server) = stalled_server(body);
            let library = Arc::new(SampleLibrary::empty());
            let worker_library = Arc::clone(&library);
            let (returned, returned_rx) = mpsc::sync_channel(1);
            let registration = std::thread::spawn(move || {
                let result = worker_library.register_trusted_custom(
                    &serde_json::to_string(&url).expect("source JSON"),
                    None,
                );
                returned.send(result).expect("return result");
            });
            entered.recv_timeout(ARRIVAL_TIMEOUT).expect("request");
            assert!(
                returned_rx.recv_timeout(Duration::from_millis(40)).is_err(),
                "public registration returned before its map was ready"
            );
            release.send(()).expect("release response");
            server.join().expect("server");
            returned_rx
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("registration returned")
                .expect("registration succeeded");
            registration.join().expect("registration thread");
            assert!(library.knows("tone"), "success returned before publication");

            let error = library
                .register_trusted_custom("{", None)
                .expect_err("public parse failure must be returned");
            assert!(error.contains("does not parse"), "{error}");
        }

        #[test]
        fn public_prefetch_reports_only_the_current_published_map() {
            let library = SampleLibrary::empty();
            library
                .register_trusted_custom(
                    r#"{"tone":["http://127.0.0.1:9/a.wav","http://127.0.0.1:9/b.wav"]}"#,
                    None,
                )
                .expect("published bank");
            assert_eq!(library.prefetch("tone"), 1);
            assert_eq!(library.prefetch("tone:1"), 1);
            assert_eq!(library.prefetch("not-yet-defined"), 0);
        }

        #[test]
        fn the_catalogue_lists_every_bank_with_its_variants_and_origin() {
            let library = SampleLibrary::empty();
            // With no banks the catalogue holds only the synths - and the
            // audio input, which stands with them.
            assert!(
                library
                    .catalogue()
                    .iter()
                    .all(|entry| matches!(entry.origin, SoundOrigin::Synth | SoundOrigin::Input))
            );
            assert!(!library.has_banks());
            library
                .register_trusted_custom(
                    r#"{"kick":["http://127.0.0.1:9/a.wav","http://127.0.0.1:9/b.wav"],"snare":["http://127.0.0.1:9/c.wav"]}"#,
                    None,
                )
                .expect("banks");
            let catalogue = library.catalogue();
            let names = catalogue
                .iter()
                .filter(|entry| !matches!(entry.origin, SoundOrigin::Synth | SoundOrigin::Input))
                .map(|entry| (entry.name.as_str(), entry.variants, entry.origin))
                .collect::<Vec<_>>();
            assert_eq!(
                names,
                [
                    ("kick", 2, SoundOrigin::Score),
                    ("snare", 1, SoundOrigin::Score)
                ]
            );
            assert!(library.has_banks());
            assert!(library.knows_sound("kick") && !library.knows_sound("kickk"));
        }

        /// The browser's catalogue lists the synths a score can play beside the
        /// banks, first, and never twice when a bank has taken a synth's name.
        #[test]
        fn the_catalogue_lists_the_native_synths_first_unless_a_bank_claims_the_name() {
            let library = SampleLibrary::empty();
            let entries = library.catalogue();
            assert!(!entries.is_empty());
            let supersaw = entries
                .iter()
                .find(|entry| entry.name == "supersaw")
                .expect("supersaw is listed");
            assert_eq!(supersaw.origin, SoundOrigin::Synth);
            assert_eq!(supersaw.variants, 1);
            assert!(
                entries.iter().any(|entry| entry.name == "sbd")
                    && entries.iter().any(|entry| entry.name == "sawtooth")
            );
            let native = |entry: &&SoundEntry| {
                matches!(entry.origin, SoundOrigin::Synth | SoundOrigin::Input)
            };
            assert!(
                entries.iter().take_while(native).count() == entries.iter().filter(native).count(),
                "the synths come first"
            );
            // The audio input stands with them, under its own origin: `s("i…")`
            // finds it the way it finds a synth.
            let input = entries
                .iter()
                .find(|entry| entry.name == "in")
                .expect("the audio input is listed");
            assert_eq!(input.origin, SoundOrigin::Input);
            assert_eq!(input.category, SoundCategory::Synth);
            assert_eq!(input.variants, 1, "one channel until a device says more");
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry.name == "supersaw")
                    .count(),
                1
            );
        }

        /// A pinned bank takes the category that its pin declares, whatever its
        /// address. The pin is the only place that names a pack's category.
        #[test]
        fn a_bank_is_filed_by_what_its_pin_declares() {
            let of = |url: &str| SoundCategory::of_location(Some(url));
            assert_eq!(
                of("https://strudel.b-cdn.net/tidal-drum-machines/machines/RolandTR909/bd/x.wav"),
                SoundCategory::Drums
            );
            assert_eq!(
                of("https://strudel.b-cdn.net/uzu-drumkit/kick.wav"),
                SoundCategory::Drums
            );
            assert_eq!(
                of("https://strudel.b-cdn.net/mrid/dhin.wav"),
                SoundCategory::Percussion
            );
            assert_eq!(
                of("https://strudel.b-cdn.net/piano/A0v8.mp3"),
                SoundCategory::Piano
            );
            assert_eq!(
                of("https://strudel.b-cdn.net/VCSL/Strings/x.wav"),
                SoundCategory::Orchestra
            );
            assert_eq!(
                of("https://strudel.b-cdn.net/uzu-wavetables/wt_x.wav"),
                SoundCategory::Wavetable
            );
            assert_eq!(
                of("https://strudel.b-cdn.net/Dirt-Samples/casio/high.wav"),
                SoundCategory::Dirt
            );
            // A pack pinned off the CDN, on its author's GitHub: filed by what
            // its pin says it is, not left in Other for the accident of its
            // address.
            assert_eq!(
                of(
                    "https://raw.githubusercontent.com/switchangel/pad/4f1b7bbddc72556a4246cc24e5ef282812f44d80/10_switch_angel_pad.wav"
                ),
                SoundCategory::Synth
            );
            assert_eq!(
                of(
                    "https://raw.githubusercontent.com/tzfm/akwf-waveforms/4630e06f96d342fee64faf662764ede07be4475e/wt_dbass/AKWF_dbass_0001.wav"
                ),
                SoundCategory::Wavetable
            );
            // Case-insensitive against the declared prefix, the way VCSL is
            // spelled in caps on the CDN.
            assert_eq!(
                of("https://STRUDEL.B-CDN.NET/vcsl/Strings/x.wav"),
                SoundCategory::Orchestra
            );
            assert_eq!(of("http://127.0.0.1:9/a.wav"), SoundCategory::Other);
            assert_eq!(SoundCategory::of_location(None), SoundCategory::Other);

            // Every source with files declares a category, and every declared
            // category is a real shelf label.
            let pinned: PinnedFile = serde_json::from_str(PINNED_BANKS).expect("pin parses");
            for source in &pinned.sources {
                if source.base.is_some() {
                    assert!(
                        source.category.is_some(),
                        "pin source {} declares no category",
                        source.name
                    );
                }
            }
            assert_eq!(pinned.inline.category, Some(SoundCategory::Dirt));

            let library = SampleLibrary::empty();
            assert!(
                library
                    .catalogue()
                    .iter()
                    .all(|entry| entry.category == SoundCategory::Synth)
            );
        }

        #[test]
        fn native_synth_preloads_need_no_files_and_emit_no_failure() {
            let library = SampleLibrary::empty();
            let preloads = [
                "triangle", "supersaw", "pulse", "sbd", "white", "pink", "sawtooth",
            ]
            .map(str::to_owned);

            assert_eq!(
                async_register(&library, Vec::new(), &preloads).expect("native synth preloads"),
                PrefetchStatus::Requested(0)
            );
            assert!(library.take_failures().is_empty());
        }

        /// A fetched map's own `_base` is the base for its entries, unless the
        /// source that named the map gave one. The folder the map came from is
        /// only the fallback. Shabda's maps depend on this.
        #[test]
        fn a_fetched_maps_own_base_outranks_the_folder_it_came_from() {
            // Served from `/deep/`, but declaring samples at the root - the
            // shape every shabda answer has.
            let declared = "http://127.0.0.1:9/declared/";
            let body = format!(r#"{{"_base":"{declared}","tone":"tone.wav"}}"#);
            let (url, arrival, server) = serve_once("/deep/map.json", body);

            let library = SampleLibrary::empty();
            async_register(
                &library,
                vec![(serde_json::to_string(&url).unwrap(), None)],
                &[],
            )
            .expect("manifest job");
            arrival
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the manifest was asked for");
            library.wait_until_idle(Duration::from_secs(2));
            server.join().expect("manifest server");

            let banks = library.custom.read().expect("custom banks");
            let Some(Bank::Array(urls)) = banks.get("tone") else {
                panic!("the manifest's bank was not registered: {:?}", banks.keys());
            };
            assert_eq!(
                urls[0].as_ref(),
                format!("{declared}tone.wav"),
                "the map's own _base is what its relative entries hang off"
            );
            assert!(
                !urls[0].contains("/deep/"),
                "the folder the manifest was fetched from is only the fallback"
            );
        }

        /// A source that answers with an empty map says so.
        ///
        /// Shabda returns `{"_base": "…"}` and HTTP 200 when a word finds
        /// nothing, so "it worked" and "there is nothing here" arrive looking
        /// identical. Registering the second as the first leaves you hunting a
        /// sound the browser never had.
        #[test]
        fn a_source_that_brings_no_sounds_is_reported() {
            let body = r#"{"_base":"https://example.com/"}"#.to_owned();
            let (url, arrival, server) = serve_once("/nothing.json", body);
            let origin = Url::parse(&url).unwrap().origin().ascii_serialization();

            let library = SampleLibrary::empty();
            library
                .register_custom_for_test(&origin, &serde_json::to_string(&url).unwrap())
                .expect("score registration");
            arrival
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the manifest was asked for");
            library.wait_until_idle(Duration::from_secs(2));
            server.join().expect("manifest server");

            let failures = library.take_failures();
            assert!(
                failures.iter().any(|failure| failure.contains("no sounds")),
                "an empty map registered silently: {failures:?}"
            );
        }

        /// A base the score wrote outranks the map's own `_base`. The order is:
        /// the score's base, the map's `_base`, the folder the map came from.
        /// Upstream reads `baseUrl || json._base || base`, the same order.
        #[test]
        fn a_base_the_score_wrote_outranks_the_maps_own() {
            let body = r#"{"_base":"http://127.0.0.1:9/declared/","tone":"tone.wav"}"#.to_owned();
            let (url, arrival, server) = serve_once("/deep/map.json", body);

            let library = SampleLibrary::empty();
            let asked = "http://127.0.0.1:9/asked/";
            async_register(
                &library,
                vec![(serde_json::to_string(&url).unwrap(), Some(asked.to_owned()))],
                &[],
            )
            .expect("manifest job");
            arrival
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the manifest was asked for");
            library.wait_until_idle(Duration::from_secs(2));
            server.join().expect("manifest server");

            let banks = library.custom.read().expect("custom banks");
            let Some(Bank::Array(urls)) = banks.get("tone") else {
                panic!("the manifest's bank was not registered: {:?}", banks.keys());
            };
            assert_eq!(
                urls[0].as_ref(),
                format!("{asked}tone.wav"),
                "the base the score named is the one its samples hang off"
            );
        }

        /// The same three-way order on the score path - the one `samples('…')`
        /// in a score, and every Settings pack, actually take.
        #[test]
        fn the_score_path_honours_a_fetched_maps_base_too() {
            let declared = "http://127.0.0.1:9/declared/";
            let body = format!(r#"{{"_base":"{declared}","tone":"tone.wav"}}"#);
            let (url, arrival, server) = serve_once("/deep/map.json", body);
            let origin = Url::parse(&url).unwrap().origin().ascii_serialization();
            let library = SampleLibrary::empty();
            let mut access = ScoreSampleAccess::denied();
            access.permit_source(&origin).expect("origin");
            access
                .permit_source("http://127.0.0.1:9")
                .expect("declared host");
            library
                .register_score_custom(&serde_json::to_string(&url).unwrap(), None, &access)
                .expect("score registration");
            arrival
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the manifest was asked for");
            library.wait_until_idle(Duration::from_secs(2));
            server.join().expect("manifest server");
            let banks = library.custom.read().expect("custom banks");
            let Some(Bank::Array(urls)) = banks.get("tone") else {
                panic!("not registered: {:?}", banks.keys());
            };
            assert_eq!(urls[0].as_ref(), format!("{declared}tone.wav"));
        }

        /// A map with no `_base` uses the folder it was fetched from as its
        /// base.
        #[test]
        fn a_fetched_map_without_a_base_still_uses_its_own_folder() {
            let body = r#"{"tone":"tone.wav"}"#.to_owned();
            let (url, arrival, server) = serve_once("/deep/map.json", body);

            let library = SampleLibrary::empty();
            async_register(
                &library,
                vec![(serde_json::to_string(&url).unwrap(), None)],
                &[],
            )
            .expect("manifest job");
            arrival
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the manifest was asked for");
            library.wait_until_idle(Duration::from_secs(2));
            server.join().expect("manifest server");

            let banks = library.custom.read().expect("custom banks");
            let Some(Bank::Array(urls)) = banks.get("tone") else {
                panic!("the manifest's bank was not registered: {:?}", banks.keys());
            };
            let folder = url.rsplit_once('/').map(|(folder, _)| folder).unwrap();
            assert_eq!(urls[0].as_ref(), format!("{folder}/tone.wav"));
        }

        #[test]
        fn preload_waits_for_default_then_uses_its_own_override() {
            let library = SampleLibrary::empty();
            let root = test_dir("default-preload-order");
            let manifest_path = root.join("default.json");
            let old = "http://127.0.0.1:9/default.wav";
            let body = inline_bank("tone", old);
            std::fs::write(&manifest_path, &body).expect("default fixture");
            let source = PinnedSource {
                name: "fixture".to_owned(),
                url: format!("file://{}", manifest_path.display()),
                base: Some(String::new()),
                sha256: digest(body.as_bytes()),
                category: None,
            };
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Defaults);
            library
                .enqueue_manifest_work_async(ManifestWork::Defaults {
                    sources: vec![source],
                    cache_dir: root.join("cache"),
                })
                .expect("default job");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("default ready to publish");

            let new = "http://127.0.0.1:9/new.wav";
            assert_eq!(
                async_register(
                    &library,
                    vec![(inline_bank("tone", new), None)],
                    &["tone".to_owned()],
                )
                .expect("override and preload"),
                PrefetchStatus::Deferred
            );
            assert!(
                library.shared.by_url.read().unwrap().is_empty(),
                "preload ran before its owning override"
            );

            release.send(()).unwrap();
            library.wait_until_idle(Duration::from_secs(2));
            assert!(library.shared.by_url.read().unwrap().contains_key(new));
            assert!(
                !library.shared.by_url.read().unwrap().contains_key(old),
                "preload warmed the default URL that its owning job replaced"
            );
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn preload_owned_by_the_second_override_never_uses_the_first() {
            let library = SampleLibrary::empty();
            let first = "http://127.0.0.1:9/first.wav";
            let second = "http://127.0.0.1:9/second.wav";
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Custom);
            async_register(&library, vec![(inline_bank("tone", first), None)], &[])
                .expect("first override");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("first override ready to publish");
            async_register(
                &library,
                vec![(inline_bank("tone", second), None)],
                &["tone".to_owned()],
            )
            .expect("second override and preload");
            release.send(()).expect("release first override");
            library.wait_until_idle(Duration::from_secs(2));
            let urls = library.shared.by_url.read().unwrap();
            assert!(urls.contains_key(second));
            assert!(
                !urls.contains_key(first),
                "the later evaluation's preload warmed an earlier override"
            );
        }

        #[test]
        fn idle_wait_includes_a_sample_load_started_by_manifest_completion() {
            let sample_listener = TcpListener::bind("127.0.0.1:0").expect("bind sample server");
            // The trusted audio cache persists across self-hosted CI jobs. A bare
            // ephemeral-port URL can therefore consume stale invalid bytes and
            // skip this fixture server entirely when the OS reuses the port.
            let unique_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let sample_url = format!(
                "http://{}/tone-{}-{unique_time}.wav",
                sample_listener.local_addr().unwrap(),
                std::process::id()
            );
            let (sample_entered, sample_entered_rx) = mpsc::sync_channel(1);
            let (sample_release, sample_release_rx) = mpsc::sync_channel(1);
            let sample_server = std::thread::spawn(move || {
                let (stream, _) = sample_listener.accept().expect("sample request");
                let stream = drain_request(stream);
                sample_entered.send(()).expect("announce sample request");
                sample_release_rx.recv().expect("release sample response");
                // Empty PCM fails decoding after the network work settles; either
                // Ready or Failed is idle, while the stalled request is not.
                respond(stream, "");
            });

            let (manifest_url, manifest_entered, manifest_release, manifest_server) =
                stalled_server(format!(r#"{{"tone":"{sample_url}"}}"#));
            let library = SampleLibrary::empty();
            async_register(
                &library,
                vec![(serde_json::to_string(&manifest_url).unwrap(), None)],
                &["tone".to_owned()],
            )
            .expect("manifest job");
            manifest_entered
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("manifest request");
            manifest_release.send(()).unwrap();
            manifest_server.join().unwrap();
            sample_entered_rx
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("deferred preload starts after manifest publication");

            let started = Instant::now();
            library.wait_until_idle(Duration::from_millis(70));
            assert!(
                started.elapsed() >= Duration::from_millis(50),
                "idle returned while the deferred sample response was stalled"
            );

            sample_release.send(()).unwrap();
            sample_server.join().unwrap();
            library.wait_until_idle(Duration::from_secs(1));
            assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
            assert!(matches!(
                library
                    .shared
                    .by_url
                    .read()
                    .unwrap()
                    .get(sample_url.as_str()),
                Some(UrlState::Failed { .. })
            ));
        }

        #[test]
        fn the_loader_takes_what_plays_now_first_and_the_newest_bet_before_older_ones() {
            let queue = LoadQueue::new();
            let job = |name: &str| LoadJob {
                url: Arc::from(name),
                kind: LoadKind::Install {
                    id: SampleId(0),
                    decode_rate: DecodeRate::Native,
                },
            };
            queue.push(job("bet-a"), LoadPriority::Bet);
            queue.push(job("bet-b"), LoadPriority::Bet);
            queue.push(job("now-c"), LoadPriority::Now);
            queue.push(job("bet-d"), LoadPriority::Bet);
            // The first bet turns out to be needed: it joins the front line.
            queue.promote("bet-a");
            let order = std::iter::from_fn(|| queue.try_pop())
                .map(|job| job.url.to_string())
                .collect::<Vec<_>>();
            assert_eq!(order, ["now-c", "bet-a", "bet-d", "bet-b"]);
            queue.close();
            assert!(
                queue.pop().is_none(),
                "a closed, empty line ends the thread"
            );
            queue.push(job("late"), LoadPriority::Now);
            assert!(queue.try_pop().is_none(), "nothing joins a closed line");
        }

        #[test]
        fn a_failed_download_rests_then_a_real_ask_tries_again_but_a_bet_does_not() {
            let library = SampleLibrary::empty();
            library
                .register_trusted_custom(r#"{"bd":["http://127.0.0.1:9/a.wav"]}"#, None)
                .expect("bank");
            assert_eq!(library.readiness("bd", 0.0), SoundReadiness::Loading);
            library.wait_until_idle(Duration::from_secs(5));
            assert_eq!(library.readiness("bd", 0.0), SoundReadiness::Failed);
            // Needed now, but the failure is fresh: it is not knocked on again.
            assert!(matches!(
                SampleLookup::resolve(&library, "bd", 0.0, 36.0),
                SampleResolution::Failed
            ));
            // Once it has rested, a real ask tries again; a bet still does not.
            let url: Arc<str> = Arc::from("http://127.0.0.1:9/a.wav");
            library.shared.by_url.write().expect("table").insert(
                url,
                UrlState::Failed {
                    at: Instant::now() - FAILED_RETRY_AFTER,
                },
            );
            assert_eq!(
                library.readiness("bd", 0.0),
                SoundReadiness::Failed,
                "a bet does not spend a retry"
            );
            assert!(
                matches!(
                    SampleLookup::resolve(&library, "bd", 0.0, 36.0),
                    SampleResolution::Loading
                ),
                "a real ask does"
            );
            library.wait_until_idle(Duration::from_secs(5));
            assert_eq!(library.readiness("bd", 0.0), SoundReadiness::Failed);
        }

        #[test]
        fn a_registered_map_is_a_source_the_score_can_be_held_to() {
            let library = SampleLibrary::empty();
            let map = r#"{"bd":["http://127.0.0.1:9/a.wav"]}"#;
            assert!(!library.knows_samples_source(map));
            library.register_trusted_custom(map, None).expect("map");
            assert!(
                library.knows_samples_source(map),
                "the map text, as registered"
            );
            // A score's string argument reaches the library stringified.
            library
                .shared
                .note_source_standing("\"github:me/mine\"", SourceState::Ready);
            assert!(library.knows_samples_source("github:me/mine"));
            assert!(!library.knows_samples_source("github:me/other"));
        }

        #[test]
        fn queued_preloads_are_count_and_byte_bounded_before_copying() {
            let exact = vec!["x".repeat(MAX_QUEUED_PRELOAD_BYTES)];
            let copied = copy_preload_specs(&exact).expect("exact byte boundary");
            assert_eq!(copied.len(), 1);
            assert_eq!(copied[0].len(), MAX_QUEUED_PRELOAD_BYTES);

            let over_bytes = vec![format!("{} y", exact[0])];
            let error = copy_preload_specs(&over_bytes).expect_err("byte cap");
            assert!(error.contains("session limit"), "{error}");

            let over_count = vec![vec!["x"; MAX_QUEUED_PRELOADS + 1].join(" ")];
            let error = copy_preload_specs(&over_count).expect_err("entry cap");
            assert!(error.contains("session limit"), "{error}");
        }

        #[test]
        fn manifest_payload_limits_include_effect_count_and_bases() {
            validate_manifest_effects(
                (0..MAX_MANIFEST_EFFECTS).map(|_| ("{}", Option::<&str>::None)),
            )
            .expect("exact effect boundary");
            let error = validate_manifest_effects(
                (0..=MAX_MANIFEST_EFFECTS).map(|_| ("{}", Option::<&str>::None)),
            )
            .expect_err("one effect above the boundary");
            assert!(error.contains("per-evaluation limit"), "{error}");

            let map = "x".repeat(MAX_MANIFEST_EFFECT_BYTES - 1);
            validate_manifest_effects(std::iter::once((map.as_str(), Some("x"))))
                .expect("map plus base at exact byte boundary");
            let error = validate_manifest_effects(std::iter::once((map.as_str(), Some("xx"))))
                .expect_err("base bytes count toward the boundary");
            assert!(error.contains("byte limit"), "{error}");
        }

        #[test]
        fn manifest_fifo_rejects_oversized_owned_batches_before_pending_charge() {
            let library = SampleLibrary::empty();
            let too_many = (0..=MAX_MANIFEST_EFFECTS)
                .map(|_| ("{}".to_owned(), None))
                .collect();
            let error = library
                .register_trusted_batch_async_for_test(too_many, &[])
                .expect_err("effect count bound");
            assert!(error.contains("per-evaluation limit"), "{error}");
            assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);

            let too_large = "x".repeat(MAX_MANIFEST_EFFECT_BYTES + 1);
            let error = library
                .register_trusted_batch_async_for_test(vec![(too_large, None)], &[])
                .expect_err("effect byte bound");
            assert!(error.contains("byte limit"), "{error}");
            assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
        }

        #[test]
        fn async_manifest_json_is_parsed_only_after_enqueue() {
            let library = SampleLibrary::empty();
            async_register(&library, vec![("{".to_owned(), None)], &[])
                .expect("producer performs only bounded shape checks");
            library.wait_until_idle(Duration::from_secs(1));
            assert!(
                library
                    .take_failures()
                    .iter()
                    .any(|failure| failure.contains("does not parse"))
            );
        }

        /// Registrations behind a fetch in flight wait in line without holding
        /// up the producer, and land in the order they were made: the last
        /// registration of a name wins.
        #[test]
        fn a_busy_manifest_loader_parks_without_blocking_the_producer() {
            let (url, entered, release, server) = stalled_server("{}".to_owned());
            let library = SampleLibrary::empty();
            async_register(
                &library,
                vec![(serde_json::to_string(&url).unwrap(), None)],
                &[],
            )
            .expect("active job");
            entered.recv_timeout(ARRIVAL_TIMEOUT).expect("request");
            let started = Instant::now();
            for (name, file) in [
                ("queued_one", "one"),
                ("queued_two", "first"),
                ("queued_three", "three"),
                ("queued_two", "last"),
            ] {
                async_register(
                    &library,
                    vec![(
                        inline_bank(name, &format!("http://127.0.0.1:9/{file}.wav")),
                        None,
                    )],
                    &[],
                )
                .expect("a busy loader parks the job");
            }
            assert!(started.elapsed() < Duration::from_millis(500));

            release.send(()).unwrap();
            server.join().unwrap();
            library.wait_until_idle(Duration::from_secs(2));
            for name in ["queued_one", "queued_two", "queued_three"] {
                assert!(library.knows(name), "{name}");
            }
            let custom = library.custom.read().unwrap();
            assert_eq!(
                bank_file_urls(&custom["queued_two"])
                    .iter()
                    .map(|url| &***url)
                    .collect::<Vec<_>>(),
                ["http://127.0.0.1:9/last.wav"]
            );
        }

        #[test]
        fn one_deadline_covers_recursion_and_every_effect_in_the_batch() {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind server");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let (stop, stopping) = mpsc::sync_channel(1);
            let nested = serde_json::to_string(&format!("{origin}/second.json")).unwrap();
            let server = std::thread::spawn(move || -> Result<usize, String> {
                let mut requests = 0usize;
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            requests += 1;
                            let stream = drain_request(stream);
                            match requests {
                                1 => respond(stream, &nested),
                                2 => {
                                    // Consume the complete batch deadline without
                                    // answering. Keeping the listener alive means
                                    // a later effect can still connect and be
                                    // counted if it receives a fresh budget.
                                    std::thread::sleep(Duration::from_millis(1_200));
                                    drop(stream);
                                }
                                _ => respond(stream, "{}"),
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if stopping.try_recv().is_ok() {
                                return Ok(requests);
                            }
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                }
            });

            let library = SampleLibrary::empty();
            library
                .enqueue_manifest_work_for(
                    ManifestWork::Custom {
                        effects: vec![
                            (
                                serde_json::to_string(&format!("{origin}/first.json")).unwrap(),
                                None,
                            ),
                            (
                                serde_json::to_string(&format!("{origin}/third.json")).unwrap(),
                                None,
                            ),
                        ],
                        preloads: Vec::new(),
                        intent: PrefetchIntent::Explicit,
                        access: ManifestAccess::Trusted,
                        continue_on_error: true,
                        layer: BankLayer::Score,
                    },
                    // Long enough that two loopback connections are certain even
                    // on a machine running the rest of the suite beside this one,
                    // and short enough that the stall above outlasts it: what the
                    // test reads is which effects got to ask, so the budget has to
                    // be generous for the ones that should and spent for the one
                    // that should not. At 70 ms a loaded machine spent it before
                    // the first connection and the server saw nothing at all.
                    // 500 ms was still spent before the RECURSION connected once
                    // this module grew two more tests beside it, which read as one
                    // request instead of two. The ceiling is the 1_200 ms stall:
                    // the budget has to expire inside it, or the later effect gets
                    // to ask and the point of the test is lost.
                    Duration::from_millis(900),
                )
                .expect("enqueue batch");
            library.wait_until_idle(Duration::from_secs(5));
            let failures = library.take_failures();
            let _ = stop.send(());
            let requests = server
                .join()
                .expect("manifest server thread")
                .expect("manifest server");
            assert_eq!(requests, 2, "the later effect received a fresh timeout");
            assert!(
                failures
                    .iter()
                    .any(|failure| failure.contains("deadline exceeded")),
                "the aggregate deadline was not surfaced"
            );
        }

        #[test]
        fn dropping_a_library_is_nonblocking_and_prevents_active_or_queued_commits() {
            let active = r#"{"active":"http://127.0.0.1:9/active.wav"}"#.to_owned();
            let (url, entered, release, server) = stalled_server(active);
            let library = SampleLibrary::empty();
            async_register(
                &library,
                vec![(serde_json::to_string(&url).unwrap(), None)],
                &[],
            )
            .expect("active job");
            entered.recv_timeout(ARRIVAL_TIMEOUT).expect("request");
            async_register(
                &library,
                vec![(r#"{"late":"http://127.0.0.1:9/late.wav"}"#.to_owned(), None)],
                &[],
            )
            .expect("queued job");
            let custom = Arc::clone(&library.custom);
            let shared = Arc::clone(&library.shared);
            let dropped = Instant::now();
            drop(library);
            assert!(
                dropped.elapsed() < Duration::from_millis(500),
                "Drop waited for an active HTTP response"
            );

            // Ureq cannot interrupt the active read. Releasing it lets the worker
            // observe cancellation after I/O; neither those bytes nor the queued
            // job may publish anything.
            release.send(()).unwrap();
            server.join().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while shared.manifest_pending.load(Ordering::Acquire) != 0 && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(shared.manifest_pending.load(Ordering::Acquire), 0);
            let custom = custom.read().unwrap();
            assert!(!custom.contains_key("active"));
            assert!(!custom.contains_key("late"));
            drop(custom);
            assert!(
                shared
                    .failures
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|failure| failure.message.contains("cancelled"))
            );
        }

        /// A job's time starts when the worker takes it: a wait in line longer
        /// than the job's whole budget costs it nothing.
        #[test]
        fn a_queued_job_budget_starts_when_the_worker_takes_it() {
            let library = SampleLibrary::empty();
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Custom);
            library
                .enqueue_manifest_work_for(
                    ManifestWork::Custom {
                        effects: vec![(inline_bank("first", "http://127.0.0.1:9/first.wav"), None)],
                        preloads: Vec::new(),
                        intent: PrefetchIntent::Explicit,
                        access: ManifestAccess::Trusted,
                        continue_on_error: true,
                        layer: BankLayer::Score,
                    },
                    Duration::from_secs(30),
                )
                .expect("first job");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("first job reached publication");
            library
                .enqueue_manifest_work_for(
                    ManifestWork::Custom {
                        effects: vec![(
                            inline_bank("waited", "http://127.0.0.1:9/waited.wav"),
                            None,
                        )],
                        preloads: Vec::new(),
                        intent: PrefetchIntent::Explicit,
                        access: ManifestAccess::Trusted,
                        continue_on_error: true,
                        layer: BankLayer::Score,
                    },
                    Duration::from_secs(1),
                )
                .expect("queued job");
            std::thread::sleep(Duration::from_millis(1_200));
            release.send(()).expect("release first job");
            library.wait_until_idle(Duration::from_secs(5));
            assert!(library.knows("first"));
            assert!(library.knows("waited"), "the wait in line spent its budget");
            assert!(
                library
                    .take_failures()
                    .iter()
                    .all(|failure| !failure.contains("deadline exceeded")),
            );
        }

        /// A map whose load spends the batch's budget leaves no later map of the
        /// batch reading loading with no job behind it: each reads failed, as a
        /// retry and anything waiting on it can see.
        #[test]
        fn a_spent_batch_fails_every_map_it_never_reached() {
            let library = SampleLibrary::empty();
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Custom);
            let first = inline_bank("first", "http://127.0.0.1:9/first.wav");
            let later = inline_bank("later", "http://127.0.0.1:9/later.wav");
            for map in [&first, &later] {
                library.mark_source_loading(map);
            }
            library
                .enqueue_manifest_work_for(
                    ManifestWork::Custom {
                        effects: vec![(first.clone(), None), (later.clone(), None)],
                        preloads: Vec::new(),
                        intent: PrefetchIntent::Explicit,
                        access: ManifestAccess::Trusted,
                        continue_on_error: true,
                        layer: BankLayer::Score,
                    },
                    Duration::from_secs(1),
                )
                .expect("the batch");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("the first map reached publication");
            std::thread::sleep(Duration::from_millis(1_200));
            release.send(()).expect("release the first map");
            library.wait_until_idle(Duration::from_secs(5));
            for map in [&first, &later] {
                let state = library.samples_source_state(map);
                assert!(
                    matches!(&state, Some(SourceState::Failed(reason)) if reason.contains("deadline exceeded")),
                    "{map}: {state:?}"
                );
            }
            assert!(!library.knows("later"));
        }

        #[test]
        fn drop_wins_at_custom_map_publication() {
            let library = SampleLibrary::empty();
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Custom);
            async_register(
                &library,
                vec![(inline_bank("late", "http://127.0.0.1:9/late.wav"), None)],
                &[],
            )
            .expect("job");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("map ready to publish");
            let custom = Arc::clone(&library.custom);
            let shared = Arc::clone(&library.shared);
            drop(library);
            release.send(()).expect("release publication");
            wait_for_manifest_jobs(&shared);
            assert!(!custom.read().unwrap().contains_key("late"));
        }

        #[test]
        fn drop_wins_at_default_map_publication() {
            let root = test_dir("default-publication");
            let manifest = root.join("default.json");
            let body = inline_bank("late_default", "http://127.0.0.1:9/default.wav");
            std::fs::write(&manifest, &body).expect("manifest fixture");
            let library = SampleLibrary::empty();
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Defaults);
            library
                .enqueue_manifest_work_async(ManifestWork::Defaults {
                    sources: vec![PinnedSource {
                        name: "fixture".to_owned(),
                        url: format!("file://{}", manifest.display()),
                        base: Some(String::new()),
                        sha256: digest(body.as_bytes()),
                        category: None,
                    }],
                    cache_dir: root.join("cache"),
                })
                .expect("default job");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("defaults ready to publish");
            let banks = Arc::clone(&library.banks);
            let shared = Arc::clone(&library.shared);
            drop(library);
            release.send(()).expect("release publication");
            wait_for_manifest_jobs(&shared);
            assert!(!banks.read().unwrap().contains_key("late_default"));
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn drop_wins_at_local_map_publication() {
            let root = test_dir("local-publication");
            std::fs::create_dir_all(root.join("kit")).expect("sample folder");
            std::fs::write(root.join("kit/hit.wav"), b"RIFF").expect("sample fixture");
            let library = SampleLibrary::empty();
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Local);
            async_register(
                &library,
                vec![(
                    serde_json::to_string(&format!("local:{}", root.display())).unwrap(),
                    None,
                )],
                &[],
            )
            .expect("local job");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("local map ready to publish");
            let custom = Arc::clone(&library.custom);
            let shared = Arc::clone(&library.shared);
            drop(library);
            release.send(()).expect("release publication");
            wait_for_manifest_jobs(&shared);
            assert!(!custom.read().unwrap().contains_key("kit"));
            let _ = std::fs::remove_dir_all(root);
        }

        /// A host-trusted local folder must publish even while the pinned
        /// default maps still own the manifest worker. Queuing it behind that
        /// fetch is what made studio e2e die with "sample manifest deadline
        /// exceeded" on a slow first GitHub round-trip.
        #[test]
        fn trusted_local_folder_does_not_wait_behind_default_manifests() {
            let root = test_dir("local-ahead-of-defaults");
            let samples = root.join("samples");
            std::fs::create_dir_all(samples.join("bd")).expect("bank");
            std::fs::write(samples.join("bd/hit.wav"), b"RIFF").expect("sample");
            let manifest = root.join("default.json");
            let body = inline_bank("late_default", "http://127.0.0.1:9/default.wav");
            std::fs::write(&manifest, &body).expect("manifest fixture");

            let library = SampleLibrary::empty();
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Defaults);
            library
                .enqueue_manifest_work_async(ManifestWork::Defaults {
                    sources: vec![PinnedSource {
                        name: "fixture".to_owned(),
                        url: format!("file://{}", manifest.display()),
                        base: Some(String::new()),
                        sha256: digest(body.as_bytes()),
                        category: None,
                    }],
                    cache_dir: root.join("cache"),
                })
                .expect("default job");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("defaults at publication");

            let map = serde_json::to_string(&format!("local:{}", samples.display())).unwrap();
            let (done, done_rx) = mpsc::sync_channel(1);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let _ = done.send(library.register_trusted_custom(&map, None));
                });
                let registered = done_rx.recv_timeout(Duration::from_secs(2));
                release.send(()).expect("release defaults");
                registered
                    .expect("local folder waited behind the default manifest job")
                    .expect("local folder");
            });
            assert!(
                library.knows("bd"),
                "the local bank must be published while defaults are still in flight"
            );
            library.wait_until_idle(Duration::from_secs(2));
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn drop_wins_after_cache_staging_before_final_rename() {
            let (url, _entered, release_response, server) =
                stalled_server(r#"{"tone":"http://127.0.0.1:9/tone.wav"}"#.to_owned());
            let root = test_dir("cache-publication");
            let library = SampleLibrary::empty();
            let (reached, release_publication) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Cache);
            let publication = Arc::clone(&library.shared.publication);
            let budget = library.manifest_budget(Duration::from_secs(2));
            let worker_root = root.clone();
            let worker_url = url.clone();
            let fetch = std::thread::spawn(move || {
                fetch_manifest_cached_with_budget(&worker_root, &worker_url, &budget, &publication)
            });
            release_response.send(()).expect("release response");
            server.join().expect("server");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("cache staged and synced");
            let destination = cache_path(&root, &url);
            drop(library);
            release_publication.send(()).expect("release publication");
            let error = fetch
                .join()
                .expect("cache worker")
                .expect_err("cancelled cache cannot publish");
            assert!(error.contains("cancelled"), "{error}");
            assert!(!destination.exists(), "final cache appeared after Drop");
            assert_eq!(
                std::fs::read_dir(&root).expect("cache directory").count(),
                0,
                "staged cache file was not cleaned up"
            );
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn drop_wins_after_sample_cache_staging_before_final_rename() {
            let (url, _entered, release_response, server) = stalled_server("sample".to_owned());
            let root = test_dir("sample-cache-publication");
            let library = SampleLibrary::empty();
            let (reached, release_publication) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Cache);
            let publication = Arc::clone(&library.shared.publication);
            let budget = sample_fetch::FetchBudget::for_one_fetch_with_cancellation(
                publication.cancellation(),
            );
            let worker_root = root.clone();
            let worker_url = url.clone();
            let fetch = std::thread::spawn(move || {
                fetch_cached_with_budget(&worker_root, &worker_url, &budget, &publication)
            });
            release_response.send(()).expect("release response");
            server.join().expect("server");
            reached
                .recv_timeout(Duration::from_secs(5))
                .expect("sample cache staged and synced");
            let destination = cache_path(&root, &url);
            drop(library);
            release_publication.send(()).expect("release publication");
            let error = fetch
                .join()
                .expect("sample cache worker")
                .expect_err("cancelled sample cache cannot publish");
            assert!(error.contains("cancelled"), "{error}");
            assert!(
                !destination.exists(),
                "final sample cache appeared after Drop"
            );
            assert_eq!(
                std::fs::read_dir(&root).expect("cache directory").count(),
                0,
                "staged sample cache file was not cleaned up"
            );
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn drop_wins_after_score_cache_staging_before_final_rename() {
            let root = test_dir("score-cache-publication");
            let library = SampleLibrary::empty();
            let (reached, release_publication) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Cache);
            let publication = Arc::clone(&library.shared.publication);
            let budget = sample_fetch::FetchBudget::for_one_fetch_with_cancellation(
                publication.cancellation(),
            );
            let cache = Arc::new(ScoreCache::new(root.clone()));
            let destination = cache.path("https://samples.example/late.wav", ScoreCacheKind::Audio);
            let worker_cache = Arc::clone(&cache);
            let worker_destination = destination.clone();
            let writer = std::thread::spawn(move || {
                worker_cache.admit_with_publication(
                    &worker_destination,
                    b"complete sample bytes",
                    &publication,
                    &budget,
                )
            });

            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("score cache staged and synced");
            drop(library);
            release_publication.send(()).expect("release publication");
            let error = writer
                .join()
                .expect("score cache writer")
                .expect_err("cancelled score cache cannot publish");
            assert!(error.contains("cancelled"), "{error}");
            assert!(
                !destination.exists(),
                "final score cache appeared after Drop"
            );
            assert!(
                std::fs::read_dir(cache.dir())
                    .expect("score cache directory")
                    .all(|entry| !is_score_cache_staging(&entry.expect("entry").path())),
                "staged score cache file was not cleaned up"
            );
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn drop_before_deferred_preload_prevents_a_loader_job() {
            let library = SampleLibrary::empty();
            let sample = "http://127.0.0.1:9/never.wav";
            let (reached, release) = library
                .shared
                .publication
                .install_test_barrier(PublicationKind::Preload);
            async_register(
                &library,
                vec![(inline_bank("tone", sample), None)],
                &["tone".to_owned()],
            )
            .expect("manifest and preload");
            reached
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("preload ready to start");
            let shared = Arc::clone(&library.shared);
            assert!(shared.by_url.read().unwrap().is_empty());
            drop(library);
            release.send(()).expect("release preload");
            wait_for_manifest_jobs(&shared);
            assert!(
                shared.by_url.read().unwrap().is_empty(),
                "deferred preload started after cancellation"
            );
        }

        #[test]
        fn public_and_session_default_constructors_keep_distinct_readiness_contracts() {
            let body = inline_bank("tone", "http://127.0.0.1:9/tone.wav");
            let (url, entered, release, server) = stalled_server(body.clone());
            let pinned = serde_json::json!({
                "sources": [{
                    "name": "fixture",
                    "url": url,
                    "base": "",
                    "sha256": digest(body.as_bytes()),
                }],
                "inline": { "base": "", "banks": {} },
            })
            .to_string();
            let fonts = r#"{"base":"","fonts":{}}"#.to_owned();
            let root = test_dir("blocking-default");
            let (returned, returned_rx) = mpsc::sync_channel(1);
            let blocking_root = root.clone();
            let load = std::thread::spawn(move || {
                returned
                    .send(SampleLibrary::load_default_from(
                        &pinned,
                        &fonts,
                        false,
                        blocking_root,
                    ))
                    .expect("return load result");
            });
            entered
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("default request");
            assert!(
                returned_rx.recv_timeout(Duration::from_millis(40)).is_err(),
                "public load_default returned before defaults were ready"
            );
            release.send(()).expect("release defaults");
            server.join().expect("server");
            let library = returned_rx
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("load returned")
                .expect("load succeeded");
            load.join().expect("load thread");
            assert!(library.knows("tone"));
            assert_eq!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
            drop(library);
            let _ = std::fs::remove_dir_all(root);
            let async_body = inline_bank("later", "http://127.0.0.1:9/later.wav");
            let (url, entered, release, server) = stalled_server(async_body.clone());
            let pinned = serde_json::json!({
                "sources": [{
                    "name": "fixture",
                    "url": url,
                    "base": "",
                    "sha256": digest(async_body.as_bytes()),
                }],
                "inline": { "base": "", "banks": {} },
            })
            .to_string();
            let root = test_dir("async-default");
            let started = Instant::now();
            let library = SampleLibrary::load_default_from(
                &pinned,
                r#"{"base":"","fonts":{}}"#,
                true,
                root.clone(),
            )
            .expect("asynchronous default library");
            assert!(started.elapsed() < Duration::from_millis(500));
            entered
                .recv_timeout(ARRIVAL_TIMEOUT)
                .expect("async default request");
            assert!(!library.knows("later"));
            assert_ne!(library.shared.manifest_pending.load(Ordering::Acquire), 0);
            release.send(()).expect("release async defaults");
            server.join().expect("server");
            library.wait_until_idle(Duration::from_secs(1));
            assert!(library.knows("later"));
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
