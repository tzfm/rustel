use super::*;

/// What a fetch refused for a missing CORS consent says. The caller that
/// holds the policy finds it by this and notes the origin.
pub(super) const NO_CORS_CONSENT: &str = "the server does not consent to cross-origin reads";

impl ScoreSampleAccess {
    pub fn denied() -> Self {
        Self::default()
    }

    /// Permit score-selected URLs on one exact HTTP(S) origin.
    ///
    /// The origin must not contain credentials, a path, query, or fragment.
    /// Redirects are confined to the same origin.
    pub fn permit_origin(&mut self, origin: &str) -> Result<(), String> {
        let parsed =
            Url::parse(origin).map_err(|error| format!("invalid sample origin: {error}"))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err("sample origin must be an http or https origin".to_owned());
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("sample origin must not contain credentials".to_owned());
        }
        if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
            return Err("sample origin must not contain a path, query, or fragment".to_owned());
        }
        let canonical = parsed.origin().ascii_serialization();
        if canonical == "null" {
            return Err("sample origin is not a network origin".to_owned());
        }
        if !self.allowed_origins.contains(&canonical) {
            self.allowed_origins.push(canonical);
        }
        Ok(())
    }

    /// Permit `samples('local:...')` beneath one host-selected directory.
    pub fn permit_local_root(&mut self, root: impl AsRef<Path>) -> Result<(), String> {
        let root = root.as_ref();
        self.local_root = Some(Arc::new(crate::sample_server::ServedRoot::open(root)?));
        Ok(())
    }

    /// Permit any public **https** origin whose server consents to
    /// cross-origin reads - the strudel.cc parity policy.
    ///
    /// Consent is checked at the wire, per response: every hop of every
    /// manifest and audio fetch must answer with
    /// `Access-Control-Allow-Origin: *` (or `https://strudel.cc`), exactly the
    /// header without which the same URL fails on strudel.cc. The reachable
    /// set is therefore the browser's, not "everything": a host that never
    /// opted into scripted fetching stays unreachable. The address boundary
    /// is unchanged - only public addresses, and URLs that *name* loopback or
    /// use plain http still require an exact [`Self::permit_origin`] grant.
    pub fn permit_public_cors_origins(&mut self) {
        self.open_public = true;
    }

    /// Turn the public CORS default of [`Self::permit_public_cors_origins`]
    /// on or off. Exact grants and the local root stay.
    pub fn set_public_cors_origins(&mut self, open: bool) {
        self.open_public = open;
    }

    /// Whether the public CORS default is on.
    pub fn public_cors_origins(&self) -> bool {
        self.open_public
    }

    /// Whether `samples('local:')` has a root to read.
    pub fn has_local_root(&self) -> bool {
        self.local_root.is_some()
    }

    /// The origins this policy refused since the last call, newest last:
    /// an origin outside it, or one whose server did not consent to a
    /// cross-origin read. Each is one exact grant away from loading.
    pub fn take_refused_origins(&self) -> Vec<String> {
        self.refused.take()
    }

    pub(super) fn note_refused_origin(&self, origin: &str) {
        // An opaque origin, such as a `data:` URL's, is nothing to grant.
        if origin.starts_with("https://") || origin.starts_with("http://") {
            self.refused.note(origin.to_owned());
        }
    }

    pub fn is_denied(&self) -> bool {
        self.local_root.is_none() && self.allowed_origins.is_empty() && !self.open_public
    }

    pub(super) fn approve_remote(&self, raw: &str) -> Result<(Arc<str>, ScoreFetchAccess), String> {
        let parsed = sample_fetch::wire_url(raw)
            .map_err(|error| format!("samples() URL is invalid: {error}"))?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("samples() URL contains credentials".to_owned());
        }
        let origin = parsed.origin().ascii_serialization();
        let access = if self.allowed_origins.contains(&origin) {
            // An exact operator grant carries its own trust: no consent
            // header is demanded, which is what lets a personal server
            // without CORS configuration be granted at all.
            ScoreFetchAccess::Remote {
                origin,
                cors_required: false,
            }
        } else if self.open_public
            && parsed.scheme() == "https"
            && !sample_fetch::url_names_loopback(&parsed)
        {
            ScoreFetchAccess::Remote {
                origin,
                cors_required: true,
            }
        } else if self.open_public {
            self.note_refused_origin(&origin);
            return Err(
                "samples() URL is outside the permitted sample origins (the default policy \
                 fetches public https origins that consent via CORS; grant this one \
                 explicitly with --allow-sample-origin)"
                    .to_owned(),
            );
        } else {
            self.note_refused_origin(&origin);
            return Err("samples() URL is outside the permitted sample origins".to_owned());
        };
        let normalized: Arc<str> = Arc::from(parsed.as_str());
        Ok((normalized, access))
    }

    /// Apply the same host-granted origin scope to a score's Hydra image or
    /// video. The media loader independently keeps its stricter HTTPS and
    /// CORS rules.
    #[cfg(feature = "hydra")]
    pub(crate) fn approve_hydra_media_url(
        &self,
        raw: &str,
        media: sample_fetch::HydraMedia,
    ) -> Result<(), String> {
        self.approve_remote(raw).map(|_| ()).map_err(|_| {
            format!(
                "{} URL is outside the permitted sample origins",
                media.label()
            )
        })
    }

    /// Read a score's Hydra image or video from the local sample root, the
    /// folder `samples('local:')` reads. `path` is relative to that root,
    /// under the same rules: no absolute path, no `..`, no hidden or linked
    /// entry, and no read outside the root.
    #[cfg(feature = "hydra")]
    pub(crate) fn read_hydra_local_media(
        &self,
        path: &str,
        media: sample_fetch::HydraMedia,
    ) -> Result<Vec<u8>, String> {
        let root = self.local_root.as_ref().ok_or_else(|| {
            format!(
                "{} {path:?} is a local file; keep it in the set folder, or grant its folder with --allow-local-samples or on the Studio Permissions page",
                media.label()
            )
        })?;
        read_local_media(root, path, media)
    }

    /// The textual half of approval, run at enqueue time so a refused source
    /// string answers the evaluation that named it instead of racing process
    /// exit on the manifest worker. The worker re-runs the full approval and
    /// stays the authority; shapes that need fetched context (bank maps,
    /// nested manifests) are left to it - this must never touch the network.
    pub(super) fn preapprove_effect(&self, map_json: &str) -> Result<(), String> {
        let Ok(serde_json::Value::String(source)) = serde_json::from_str(map_json) else {
            return Ok(());
        };
        match read_source(&source, GITHUB_SAMPLE_MANIFEST) {
            SampleSource::LocalFolder(_) => {
                if self.local_root.is_none() {
                    return Err(
                        "samples('local:') requires a host-selected local sample root (--allow-local-samples, or the Studio Permissions page)".to_owned(),
                    );
                }
                Ok(())
            }
            SampleSource::Url(url) => self.approve_remote(&url).map(|_| ()),
        }
    }
}

/// Read a Hydra image or video at `path` beneath `root`. The path is
/// relative and made of plain names: no absolute path, no `..`, and no
/// hidden entry. The open is confined to the root the same way a local
/// sample's is, so a link or a rename cannot lead a read outside it.
#[cfg(feature = "hydra")]
pub(crate) fn read_local_media(
    root: &crate::sample_server::ServedRoot,
    path: &str,
    media: sample_fetch::HydraMedia,
) -> Result<Vec<u8>, String> {
    let label = media.label();
    let relative = Path::new(path);
    if !crate::sample_server::relative_sample_path_allowed(relative) {
        return Err(format!(
            "{label} {path:?} must be a relative path inside the folder, without `..`"
        ));
    }
    let (file, length) = root
        .open_score_file(&root.path().join(relative))
        .map_err(|_| format!("{label} {path:?} is not a file in the permitted folder"))?;
    let limit = media.limit();
    let too_large = || format!("{label} {path:?} exceeds the {} MiB limit", limit >> 20);
    if length > limit as u64 {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length as usize)
        .map_err(|_| format!("{label} {path:?} exceeds host memory"))?;
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("{label} {path:?} could not be read"))?;
    if bytes.len() > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

pub(super) fn fetch_manifest_located_with_budget(
    url: &str,
    budget: &sample_fetch::FetchBudget,
) -> Result<Vec<u8>, String> {
    budget.check()?;
    if url.starts_with("file://") {
        let path = url.trim_start_matches("file://");
        let file = std::fs::File::open(path).map_err(|error| format!("read {path}: {error}"))?;
        let mut bytes = Vec::new();
        file.take(sample_fetch::MAX_REMOTE_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read {path}: {error}"))?;
        if bytes.len() > sample_fetch::MAX_REMOTE_MANIFEST_BYTES {
            return Err(format!("{path} exceeds the sample manifest size limit"));
        }
        budget.check()?;
        return Ok(bytes);
    }
    sample_fetch::fetch_manifest_with_budget(url, budget)
}

/// Read a sample cache entry through [`open_regular_cache_entry`], so a link
/// at its name is refused; callers treat any `Err` as a miss.
pub(super) fn read_sample_cache(path: &Path) -> Result<Vec<u8>, String> {
    let file = open_regular_cache_entry(path)?
        .ok_or_else(|| format!("read sample cache {}: not found", path.display()))?;
    let mut reader = file.take(rustel_audio::sample_pcm_ceiling() as u64 + 1);
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| format!("read sample cache {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        let next = bytes
            .len()
            .checked_add(read)
            .ok_or_else(|| format!("{} exceeds the sample size limit", path.display()))?;
        if next > rustel_audio::sample_pcm_ceiling() {
            return Err(format!("{} exceeds the sample size limit", path.display()));
        }
        bytes
            .try_reserve_exact(read)
            .map_err(|_| "sample cache exceeds host memory".to_owned())?;
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

pub(super) fn fetch_score_source_cached_with_budget<T>(
    cache: &ScoreCache,
    url: &str,
    access: &ScoreFetchAccess,
    kind: ScoreCacheKind,
    limit: usize,
    scope: CacheFetchScope<'_>,
    validate: impl Fn(&[u8]) -> Result<T, String>,
) -> Result<T, String> {
    use std::io::Read;

    let CacheFetchScope {
        budget,
        publication,
    } = scope;
    budget.check()?;
    // A local grant reads the file where it lies; a copy would only go stale.
    let ScoreFetchAccess::Remote {
        origin,
        cors_required,
    } = access
    else {
        let value = validate(&fetch_score_source_with_budget(
            url,
            access,
            limit,
            budget,
            || {},
        )?)?;
        budget.check()?;
        return Ok(value);
    };

    let parsed = sample_fetch::wire_url(url)?;
    if parsed.origin().ascii_serialization() != *origin
        || !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(format!(
            "sample URL {url:?} is outside its permitted origin"
        ));
    }

    let trust = ScoreCacheTrust::from_cors_required(*cors_required);
    let path = cache.path(url, kind, trust);
    if let Some(file) = cache.open(&path)? {
        budget.check()?;
        let mut bytes = Vec::new();
        if file
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .is_ok_and(|_| bytes.len() <= limit)
        {
            budget.check()?;
            match validate(&bytes) {
                Ok(value) => {
                    budget.check()?;
                    return Ok(value);
                }
                // A stored entry that no longer parses or decodes is worse
                // than no entry: it would answer every future request with the
                // same bad bytes. Drop it and go to the network ONCE.
                Err(_) => {
                    cache.remove(&path)?;
                }
            }
        } else {
            cache.remove(&path)?;
        }
    }

    // Before score-selected bytes had a dedicated namespace they shared the
    // host-trusted cache root. Keep offline renders working across the
    // transition for exact grants only: the old namespace holds pinned and
    // host-trusted data with no CORS provenance, so a cors_required fetch
    // must not read it. A valid legacy hit is copied under both quotas. The
    // old namespace is never removed during migration. If admission is full
    // it still serves this run from its original location.
    if !*cors_required {
        let legacy = cache_path(&cache.base, url);
        if let Some(bytes) = cache.read_legacy(&legacy, limit)?
            && let Ok(value) = validate(&bytes)
        {
            budget.check()?;
            cache.copy_legacy_with_publication(&path, &bytes, publication, budget);
            budget.check()?;
            return Ok(value);
        }
    }

    let bytes = fetch_score_source_with_budget(url, access, limit, budget, || {})?;
    // Validate BEFORE committing, so an error page served with status 200, or
    // a truncated body, cannot become the permanent answer for this URL.
    let value = validate(&bytes)?;
    // Written through a temporary in the same directory and renamed, so a
    // reader never sees a half-written body and a crash leaves no torn entry.
    budget.check()?;
    let _ = cache.admit_with_publication(&path, &bytes, publication, budget);
    budget.check()?;
    Ok(value)
}

#[cfg(test)]
pub(super) fn fetch_score_source(
    url: &str,
    access: &ScoreFetchAccess,
    limit: usize,
) -> Result<Vec<u8>, String> {
    fetch_score_source_with_budget(
        url,
        access,
        limit,
        &sample_fetch::FetchBudget::for_one_fetch(),
        || {},
    )
}

pub(super) fn fetch_score_source_with_budget(
    url: &str,
    access: &ScoreFetchAccess,
    limit: usize,
    budget: &sample_fetch::FetchBudget,
    before_local_open: impl FnOnce(),
) -> Result<Vec<u8>, String> {
    use std::io::Read;

    budget.check()?;
    match access {
        ScoreFetchAccess::Local { root } => {
            let parsed = Url::parse(url)
                .map_err(|error| format!("invalid local sample URL {url:?}: {error}"))?;
            if parsed.scheme() != "file" {
                return Err(format!("local sample URL {url:?} is not a file URL"));
            }
            let path = parsed
                .to_file_path()
                .map_err(|_| format!("local sample URL {url:?} is not a valid file path"))?;
            let (file, len) = root.open_score_sample_with(&path, before_local_open)?;
            if len > limit as u64 {
                return Err(format!("{} exceeds the sample size limit", path.display()));
            }
            let mut bytes = Vec::new();
            file.take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
            if bytes.len() > limit {
                return Err(format!("{} exceeds the sample size limit", path.display()));
            }
            budget.check()?;
            Ok(bytes)
        }
        ScoreFetchAccess::Remote {
            origin,
            cors_required,
        } => {
            let mut current = sample_fetch::wire_url(url)?;
            for redirects in 0..=5 {
                budget.check()?;
                if current.origin().ascii_serialization() != *origin
                    || !matches!(current.scheme(), "http" | "https")
                    || !current.username().is_empty()
                    || current.password().is_some()
                {
                    return Err(format!(
                        "sample URL {current:?} is outside its permitted origin"
                    ));
                }
                // The guarded agent, not a bare one: the origin check above is
                // text, and says nothing about the address the name resolves
                // to. A granted origin can still point at a private target,
                // and can point somewhere different on the next lookup.
                let response = sample_fetch::guarded_single_hop_get(
                    &current,
                    budget,
                    cors_required.then_some(CORS_REQUEST_ORIGIN),
                )?;
                // Consent is per RESPONSE, redirects included, exactly as a
                // browser applies its access check to every hop: a consenting
                // server must not become a pivot through a non-consenting one.
                if *cors_required && !cors_consents(response.header("Access-Control-Allow-Origin"))
                {
                    return Err(format!(
                        "GET {current}: {NO_CORS_CONSENT} \
                         (no `Access-Control-Allow-Origin: *`), so this URL would fail on \
                         strudel.cc as well; pass --allow-sample-origin {origin} to trust \
                         it anyway"
                    ));
                }
                if (300..400).contains(&response.status()) {
                    if redirects == 5 {
                        return Err(format!("GET {url}: too many redirects"));
                    }
                    let location = response
                        .header("Location")
                        .ok_or_else(|| format!("GET {current}: redirect has no Location header"))?;
                    let next = current.join(location).map_err(|error| {
                        format!("GET {current}: invalid redirect target: {error}")
                    })?;
                    if next.origin().ascii_serialization() != *origin
                        || !next.username().is_empty()
                        || next.password().is_some()
                    {
                        return Err(format!(
                            "GET {current}: redirect target is outside the permitted sample origin"
                        ));
                    }
                    current = next;
                    continue;
                }
                let mut bytes = Vec::new();
                response
                    .into_reader()
                    .take(limit as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| format!("read {current}: {error}"))?;
                budget.check()?;
                if bytes.len() > limit {
                    return Err(format!("{current} exceeds the sample size limit"));
                }
                return Ok(bytes);
            }
            unreachable!("redirect loop returns on success or refusal")
        }
    }
}

impl Shorthand {
    /// What follows this shorthand in `source`, or `None` when the row is
    /// not what was written.
    ///
    /// Prefixes without a trailing separator need a boundary check so
    /// `shabda/speechify` is not mistaken for `shabda/speech`.
    fn read<'a>(&self, source: &'a str) -> Option<&'a str> {
        let rest = source.strip_prefix(self.prefix)?;
        if self.prefix.ends_with(':') || rest.is_empty() {
            return Some(rest);
        }
        rest.starts_with([':', '/']).then_some(rest)
    }
}

/// Read a source string the way every caller needs it: spellings resolved
/// first, then either a folder or a URL. `subpath` is what a `github:`
/// shorthand asks for - the manifest name when a map is being fetched, and
/// nothing when the answer is a base for other paths to hang off.
pub(super) fn read_source(source: &str, subpath: &str) -> SampleSource {
    let mut source = source.to_owned();
    // A spelling can expand into another spelling - `bubo:` expands into
    // `github:` - so this walks. Bounded by the table's own length: a row
    // added in a cycle must not hang the score that named it.
    for _ in 0..SHORTHANDS.len() {
        // Longest match, so a row cannot be shadowed by a shorter one that
        // happens to be written above it.
        let Some((found, rest)) = SHORTHANDS
            .iter()
            .filter_map(|shorthand| shorthand.read(&source).map(|rest| (shorthand, rest)))
            .max_by_key(|(shorthand, _)| shorthand.prefix.len())
        else {
            break;
        };
        // A folder's text is its callers' to trim. An address is trimmed
        // here, what a shorthand expands and the plain URL below alike: the
        // newline that ends a multi-line template literal would otherwise
        // land inside every address built from it - a shorthand's path, or
        // each file a base is joined to.
        let trimmed_rest = rest.trim_ascii();
        match found.kind {
            ShorthandKind::LocalFolder => return SampleSource::LocalFolder(rest.to_owned()),
            ShorthandKind::GitHub => return SampleSource::Url(github_path(trimmed_rest, subpath)),
            ShorthandKind::Speech {
                pattern,
                language,
                gender,
            } => return SampleSource::Url(speech_path(trimmed_rest, pattern, language, gender)),
            ShorthandKind::Spelling(pattern) => source = pattern.replace("{}", trimmed_rest),
        }
    }
    SampleSource::Url(source.trim_ascii().to_owned())
}

/// `[/<language>/<gender>]:<words>` - what shabda's speech form says after
/// its prefix.
///
/// Language and gender are optional and positional, language first. A colon
/// separates settings from words; without one, the entire input is words.
fn speech_path(rest: &str, pattern: &str, language: &str, gender: &str) -> String {
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    // Only the first colon separates settings from words, preserving word
    // counts such as `hello:2` on the right.
    let (params, words) = match rest.split_once(':') {
        Some((params, words)) => (params, words),
        None => ("", rest),
    };
    let mut parts = params.split('/').filter(|part| !part.is_empty());
    let language = parts.next().unwrap_or(language);
    // A language without a gender still uses the shorthand's default voice.
    let gender = parts.next().unwrap_or(gender);
    pattern
        .replace("{words}", words)
        .replace("{language}", language)
        .replace("{gender}", gender)
}

fn github_path(path: &str, subpath: &str) -> String {
    let path = path.strip_suffix('/').unwrap_or(path);
    let mut components = path.split('/');
    let user = components.next().unwrap_or("");
    let repo = components.next().unwrap_or("samples");
    let branch = components.next().unwrap_or("main");
    let mut other: Vec<&str> = components.collect();
    other.push(subpath);
    format!(
        "https://raw.githubusercontent.com/{user}/{repo}/{branch}/{}",
        other.join("/")
    )
}

impl SampleScanLimits {
    pub(crate) const DEFAULT: Self = Self {
        examined_entries: MAX_SAMPLE_SCAN_ENTRIES,
        manifest_entries: MAX_SAMPLE_MANIFEST_ENTRIES,
        manifest_bytes: MAX_SAMPLE_MANIFEST_BYTES,
        working_bytes: MAX_SAMPLE_SCAN_WORK_BYTES,
    };
}

impl SampleFolderScanError {
    pub(crate) fn is_limit(&self) -> bool {
        matches!(self, Self::Limit { .. })
    }
}

impl std::fmt::Display for SampleFolderScanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(message) | Self::Empty(message) => formatter.write_str(message),
            // What a player can do about it, not only what the scanner
            // counted. A library of fifty thousand files is an ordinary
            // thing to own, and the row used to say only that some number
            // had been passed.
            Self::Limit { resource, limit } => {
                write!(
                    formatter,
                    "local samples: {resource} passed its limit of {limit} - import the \
                     subfolders separately, or a folder further in"
                )
            }
        }
    }
}

/// The directory a relative sample path is resolved against.
///
/// The last slash is the cut only when the URL has a path. A bare origin
/// such as `http://localhost:5432`, which the sample server gives, has its
/// last slash inside `http://`, and a cut there gives the base `http:/`.
pub(super) fn base_url(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    match after_scheme.find('/') {
        // No path at all: the origin itself is the directory.
        None => format!("{url}/"),
        Some(_) => match url.rfind('/') {
            Some(cut) => url[..=cut].to_owned(),
            None => format!("{url}/"),
        },
    }
}

/// Whether a url points at a sample server on this machine.
///
/// `@strudel/sampler` defaults to port 5432, but the port is configurable and
/// the host is what actually identifies it: nothing on localhost is a CDN, and
/// a failed fetch there means the server is not running rather than the
/// network being down.
pub(super) fn is_local_sampler(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    rest.starts_with("localhost")
        || rest.starts_with("127.0.0.1")
        || rest.starts_with("[::1]")
        || rest.starts_with("0.0.0.0")
}

/// A base may itself be a shorthand (processSampleMap).
pub(super) fn expand_base(base: &str) -> String {
    match read_source(base, "") {
        SampleSource::Url(url) => url,
        // A base is a prefix the entries hang off, not a folder to scan, so
        // a `local:` base is handed back as it was written.
        SampleSource::LocalFolder(rest) => format!("local:{rest}"),
    }
}

pub(super) fn join_url(base: &str, entry: &str) -> String {
    if entry.starts_with("http://") || entry.starts_with("https://") {
        return entry.to_owned();
    }
    let entry = repair_vcsl_relative(base, entry);
    // `@strudel/sampler` writes its paths with a leading slash and its callers
    // hand over a base with a trailing one, so a naive concatenation doubles
    // the separator. Harmless over HTTP, wrong for a `file://` path.
    if base.ends_with('/') && entry.starts_with('/') {
        return format!("{base}{}", entry.trim_start_matches('/'));
    }
    format!("{base}{entry}")
}

/// The pinned VCSL map omits the `Membranophones/` parent on a handful of
/// Tom 1 Mallet paths, so those URLs 404 forever and a pack's row can never
/// reach a full cache. Put the parent back when the base is VCSL's.
fn repair_vcsl_relative<'a>(base: &str, entry: &'a str) -> std::borrow::Cow<'a, str> {
    let base_l = base.to_ascii_lowercase();
    if !base_l.contains("/vcsl/") && !base_l.ends_with("/vcsl") {
        return std::borrow::Cow::Borrowed(entry);
    }
    let broken = entry.starts_with("Struck%20Membranophones/")
        || entry.starts_with("Struck Membranophones/");
    if broken {
        std::borrow::Cow::Owned(format!("Membranophones/{entry}"))
    } else {
        std::borrow::Cow::Borrowed(entry)
    }
}

fn approve_score_url(
    base: &str,
    file: &str,
    access: &ScoreSampleAccess,
    sources: &mut Vec<(Arc<str>, ScoreFetchAccess)>,
) -> Result<Arc<str>, String> {
    if sources.len() >= MAX_SCORE_SAMPLE_FILES {
        return Err(format!(
            "samples() map exceeds the {MAX_SCORE_SAMPLE_FILES}-file limit"
        ));
    }
    sources
        .try_reserve(1)
        .map_err(|_| "not enough host memory for sample sources".to_owned())?;
    let (url, source_access) = access.approve_remote(&join_url(base, file))?;
    sources.push((url.clone(), source_access));
    Ok(url)
}

pub(super) fn parse_score_bank(
    entry: &serde_json::Value,
    base: &str,
    access: &ScoreSampleAccess,
    sources: &mut Vec<(Arc<str>, ScoreFetchAccess)>,
) -> Result<Option<Bank>, String> {
    match entry {
        serde_json::Value::String(file) => Ok(Some(Bank::Array(vec![approve_score_url(
            base, file, access, sources,
        )?]))),
        serde_json::Value::Array(files) => {
            let count = files.iter().filter(|file| file.is_string()).count();
            if sources.len().saturating_add(count) > MAX_SCORE_SAMPLE_FILES {
                return Err(format!(
                    "samples() map exceeds the {MAX_SCORE_SAMPLE_FILES}-file limit"
                ));
            }
            let mut urls = Vec::new();
            urls.try_reserve(count)
                .map_err(|_| "not enough host memory for sample URLs".to_owned())?;
            for file in files.iter().filter_map(serde_json::Value::as_str) {
                urls.push(approve_score_url(base, file, access, sources)?);
            }
            Ok((!urls.is_empty()).then_some(Bank::Array(urls)))
        }
        serde_json::Value::Object(keys) => {
            if keys.len() > SAMPLE_BANK_CAPACITY - 1 {
                return Err(format!(
                    "samples() note map exceeds the {}-entry limit",
                    SAMPLE_BANK_CAPACITY - 1
                ));
            }
            let mut notes = Vec::new();
            notes
                .try_reserve(keys.len())
                .map_err(|_| "not enough host memory for sample notes".to_owned())?;
            for (key, files) in keys {
                if key.starts_with('_') {
                    continue;
                }
                let Some(midi) = rustel_core::util::note_to_midi(key, 3).ok() else {
                    return Ok(None);
                };
                let mut urls = Vec::new();
                match files {
                    serde_json::Value::String(file) => {
                        urls.push(approve_score_url(base, file, access, sources)?);
                    }
                    serde_json::Value::Array(files) => {
                        let count = files.iter().filter(|file| file.is_string()).count();
                        if sources.len().saturating_add(count) > MAX_SCORE_SAMPLE_FILES {
                            return Err(format!(
                                "samples() map exceeds the {MAX_SCORE_SAMPLE_FILES}-file limit"
                            ));
                        }
                        urls.try_reserve(count)
                            .map_err(|_| "not enough host memory for sample URLs".to_owned())?;
                        for file in files.iter().filter_map(serde_json::Value::as_str) {
                            urls.push(approve_score_url(base, file, access, sources)?);
                        }
                    }
                    _ => return Ok(None),
                }
                if midi.is_nan() || urls.is_empty() {
                    return Ok(None);
                }
                notes.push((midi, urls));
            }
            Ok((!notes.is_empty()).then_some(Bank::Notes(notes)))
        }
        _ => Ok(None),
    }
}

/// `getSoundIndex(n, numSounds)` - round, NaN falls back to 0, euclid wrap.
/// An empty list wraps to 0 rather than dividing by zero; a caller that
/// indexes with the result has to check for emptiness itself.
pub(crate) fn sound_index(n: f64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let n = if n.is_nan() { 0.0 } else { n };
    let rounded = crate::samples::js_round(n) as i64;
    rounded.rem_euclid(len as i64) as usize
}

/// `Math.round`: floor(x + 0.5), including negative halves.
pub(crate) fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Every registered name also answers to itself lowercased.
///
/// A bank name in the catalogue has mixed case, such as `RolandMC303`, and a
/// score can write it in another case: `.bank('Rolandmc303')` names the same
/// bank. Aliases are registered lowercased and match in any case. This
/// function gives the canonical names the same behaviour.
///
/// The lowercase key is registered, so the exact lookup stays one hash of
/// the given name. Existing keys are never overwritten, so of two banks
/// that differ only by case, the first registered keeps the key.
pub(super) fn expand_case_insensitive(banks: &mut HashMap<String, Bank>) {
    let lowered: Vec<(String, Bank)> = banks
        .iter()
        .filter(|(key, _)| key.chars().any(char::is_uppercase))
        .map(|(key, bank)| (key.to_lowercase(), Bank::clone(bank)))
        .collect();
    for (key, bank) in lowered {
        banks.entry(key).or_insert(bank);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refusal names its origin once, for every clone of the policy, and
    /// an exact grant then lets the same URL through.
    #[test]
    fn refused_origins_are_noted_once_and_a_grant_clears_the_refusal() {
        let mut access = ScoreSampleAccess::denied();
        access.permit_public_cors_origins();
        let clone = access.clone();
        for url in [
            "http://samples.example/a.json",
            "http://samples.example/b.json",
            "https://127.0.0.1/kit.json",
        ] {
            assert!(clone.approve_remote(url).is_err(), "{url}");
        }
        assert!(
            access
                .approve_remote("https://public.example/kit.json")
                .is_ok()
        );
        assert_eq!(
            access.take_refused_origins(),
            ["http://samples.example", "https://127.0.0.1"]
        );
        assert!(access.take_refused_origins().is_empty(), "taken once");

        access
            .permit_origin("http://samples.example")
            .expect("grant");
        assert!(
            access
                .approve_remote("http://samples.example/a.json")
                .is_ok()
        );
        access.set_public_cors_origins(false);
        assert!(!access.public_cors_origins());
        assert!(
            access
                .approve_remote("https://public.example/kit.json")
                .is_err()
        );
        assert_eq!(access.take_refused_origins(), ["https://public.example"]);
    }

    #[cfg(test)]
    mod base_url_tests {
        use super::{
            GITHUB_SAMPLE_MANIFEST, SHORTHANDS, SampleSource, base_url, expand_base, github_path,
            read_source,
        };

        /// Every shorthand goes through the one table, and a spelling that
        /// expands into another shorthand is followed.
        #[test]
        fn the_table_is_the_only_place_a_shorthand_is_known() {
            let url = |source: &str| match read_source(source, GITHUB_SAMPLE_MANIFEST) {
                SampleSource::Url(url) => url,
                SampleSource::LocalFolder(rest) => panic!("{source} read as the folder {rest:?}"),
            };
            assert_eq!(
                url("github:user/repo"),
                "https://raw.githubusercontent.com/user/repo/main/strudel.json"
            );
            // bubo: is a spelling of github:, and the walk follows it.
            assert_eq!(url("bubo:drum"), url("github:Bubobubobubobubo/dough-drum"));
            // A URL is not a shorthand, and neither is an unknown prefix: both
            // are handed back to be fetched, not rewritten.
            assert_eq!(
                url("https://example.com/map.json"),
                "https://example.com/map.json"
            );
            assert_eq!(url("nosuch:thing"), "nosuch:thing");
            assert!(matches!(
                read_source("local: kit ", GITHUB_SAMPLE_MANIFEST),
                SampleSource::LocalFolder(rest) if rest == " kit "
            ));
            // A base wants the folder, not the manifest, and a local base is
            // a prefix rather than a folder to scan.
            assert_eq!(
                expand_base("github:user/repo"),
                "https://raw.githubusercontent.com/user/repo/main/"
            );
            assert_eq!(expand_base("local:kit"), "local:kit");

            // shabda finds sounds by word. The plain form substitutes; nothing
            // is escaped, because the comma and the colon in `bass:2,kick` are
            // the service's own grammar.
            assert_eq!(
                url("shabda:bass,kick"),
                "https://shabda.ndre.gr/bass,kick.json?strudel=1"
            );

            // The speech form has a grammar instead: `[/language/gender]:words`,
            // both optional, language first.
            assert_eq!(
                url("shabda/speech/en-US/m:music,vocode"),
                "https://shabda.ndre.gr/speech/music,vocode.json\
             ?gender=m&language=en-US&strudel=1"
            );
            assert_eq!(
                url("shabda/speech:hello"),
                "https://shabda.ndre.gr/speech/hello.json?gender=f&language=en-GB&strudel=1",
                "no parameters is the row's own default voice"
            );
            assert_eq!(
                url("shabda/speech/de-DE:wort"),
                "https://shabda.ndre.gr/speech/wort.json?gender=f&language=de-DE&strudel=1",
                "a language with no gender beside it keeps the default gender; \
             upstream sends the word `undefined` here and gets another file"
            );
            // Only the first colon divides settings from words, so a count
            // stays with the word it belongs to.
            assert_eq!(
                url("shabda/speech:hello:2"),
                "https://shabda.ndre.gr/speech/hello:2.json?gender=f&language=en-GB&strudel=1"
            );
            // The boundary is checked, so a longer word beginning with the
            // prefix is not read as the prefix and a stray language.
            assert_eq!(url("shabda/speechify:x"), "shabda/speechify:x");

            assert_eq!(SHORTHANDS.len(), 5, "a new shorthand is a row, and a test");
        }

        #[test]
        fn github_shorthand_keeps_the_standard_manifest_name() {
            assert_eq!(GITHUB_SAMPLE_MANIFEST, "strudel.json");
            assert_eq!(
                github_path("user/repo", GITHUB_SAMPLE_MANIFEST),
                "https://raw.githubusercontent.com/user/repo/main/strudel.json"
            );
        }

        #[test]
        fn a_bare_origin_is_its_own_directory() {
            // What `samples('http://localhost:5432')` and the sample server give.
            assert_eq!(base_url("http://localhost:5432"), "http://localhost:5432/");
            assert_eq!(base_url("http://127.0.0.1:5432"), "http://127.0.0.1:5432/");
        }

        #[test]
        fn a_map_url_resolves_against_its_own_folder() {
            assert_eq!(
                base_url("https://example.com/kit/strudel.json"),
                "https://example.com/kit/"
            );
            assert_eq!(
                base_url("https://example.com/strudel.json"),
                "https://example.com/"
            );
            assert_eq!(
                base_url("https://example.com/kit/"),
                "https://example.com/kit/"
            );
        }
    }
}
