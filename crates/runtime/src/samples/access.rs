use super::*;

impl ScoreSampleAccess {
    pub fn denied() -> Self {
        Self::default()
    }

    /// Allow one source: a domain such as `example.com`, or an address such
    /// as `https://example.com/kits/`. See [`SourceRule`].
    pub fn permit_source(&mut self, source: &str) -> Result<(), String> {
        let rule = SourceRule::parse(source)?;
        if !self.sources.contains(&rule) {
            self.sources.push(rule);
        }
        Ok(())
    }

    /// Allow the domains of [`BUILTIN_SOURCES`], which the `github:`,
    /// `bubo:` and `shabda:` shorthands and the default banks read.
    pub fn permit_builtin_sources(&mut self) {
        for source in BUILTIN_SOURCES {
            self.permit_source(source)
                .expect("a built-in source is a domain");
        }
    }

    /// Take one source back. The other sources and the local root stay.
    pub fn revoke_source(&mut self, rule: &SourceRule) {
        self.sources.retain(|kept| kept != rule);
    }

    /// The sources this policy allows, in the order they were given.
    pub fn sources(&self) -> &[SourceRule] {
        &self.sources
    }

    /// The source that allows `raw`, or `None` when no source does. It does
    /// not note a refusal, so a host can ask before it fetches.
    pub fn source_for(&self, raw: &str) -> Result<Option<&SourceRule>, String> {
        let parsed = sample_fetch::wire_url(raw)?;
        Ok(self.sources.iter().find(|rule| rule.allows(&parsed)))
    }

    /// Permit `samples('local:...')` beneath one host-selected directory.
    pub fn permit_local_root(&mut self, root: impl AsRef<Path>) -> Result<(), String> {
        let root = root.as_ref();
        self.local_root = Some(Arc::new(crate::sample_server::ServedRoot::open(root)?));
        Ok(())
    }

    /// Whether `samples('local:')` has a root to read.
    pub fn has_local_root(&self) -> bool {
        self.local_root.is_some()
    }

    /// The sources this policy refused since the last call, newest last,
    /// each written as the [`SourceRule`] that would allow it.
    pub fn take_refused_sources(&self) -> Vec<String> {
        self.refused.take()
    }

    fn note_refused(&self, url: &Url) {
        // An opaque address, such as a `data:` URL, has no source to allow.
        if let Some(rule) = SourceRule::for_url(url) {
            self.refused.note(rule.to_string());
        }
    }

    /// Load `raw` once under this policy, as a score would, and say what
    /// came back: a sample map and how many sounds it names, audio, an
    /// image, a video, or other data. Nothing is cached. The Permissions
    /// page asks this to check a source before a score needs it.
    pub fn try_address(&self, raw: &str) -> Result<String, String> {
        const TRY_LIMIT: usize = 64 * 1024 * 1024;
        let (url, access) = self.approve_remote(raw)?;
        let bytes = fetch_score_source_with_budget(
            &url,
            &access,
            TRY_LIMIT,
            &sample_fetch::FetchBudget::for_one_fetch(),
            || {},
        )?;
        Ok(describe_loaded(&bytes))
    }

    pub fn is_denied(&self) -> bool {
        self.local_root.is_none() && self.sources.is_empty()
    }

    pub(super) fn approve_remote(&self, raw: &str) -> Result<(Arc<str>, ScoreFetchAccess), String> {
        let parsed = sample_fetch::wire_url(raw)
            .map_err(|error| format!("samples() URL is invalid: {error}"))?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("samples() URL contains credentials".to_owned());
        }
        let Some(rule) = self.sources.iter().find(|rule| rule.allows(&parsed)) else {
            self.note_refused(&parsed);
            let grant = SourceRule::for_url(&parsed)
                .map(|rule| format!(" Allow it with --allow-sample-origin {rule} or on the Studio Permissions page."))
                .unwrap_or_default();
            return Err(format!(
                "{} is not an allowed sample source.{grant}",
                parsed.origin().ascii_serialization()
            ));
        };
        let normalized: Arc<str> = Arc::from(parsed.as_str());
        Ok((normalized, ScoreFetchAccess::Remote { rule: rule.clone() }))
    }

    /// Apply the same sources to a score's Hydra image or video, and give the
    /// source that allows it. That source must also allow every redirect.
    #[cfg(feature = "hydra")]
    pub(crate) fn approve_hydra_media_url(
        &self,
        raw: &str,
        media: sample_fetch::HydraMedia,
    ) -> Result<SourceRule, String> {
        match self.approve_remote(raw) {
            Ok((_, ScoreFetchAccess::Remote { rule })) => Ok(rule),
            Ok(_) => Err(format!("{} URL is not a network address", media.label())),
            Err(error) => Err(error.replacen(
                "an allowed sample source",
                &format!("an allowed source for a {}", media.label()),
                1,
            )),
        }
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
    let ScoreFetchAccess::Remote { rule } = access else {
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
    if !rule.allows(&parsed) {
        return Err(format!("sample URL {url:?} is outside its allowed source"));
    }

    let path = cache.path(url, kind);
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
    // transition: a valid legacy hit is copied under both quotas. The old
    // namespace is never removed during migration. If admission is full it
    // still serves this run from its original location.
    let legacy = cache_path(&cache.base, url);
    if let Some(bytes) = cache.read_legacy(&legacy, limit)?
        && let Ok(value) = validate(&bytes)
    {
        budget.check()?;
        cache.copy_legacy_with_publication(&path, &bytes, publication, budget);
        budget.check()?;
        return Ok(value);
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
        ScoreFetchAccess::Remote { rule } => {
            let mut current = sample_fetch::wire_url(url)?;
            for redirects in 0..=5 {
                budget.check()?;
                if !rule.allows(&current) {
                    return Err(format!(
                        "sample URL {current:?} is outside its allowed source"
                    ));
                }
                // The guarded agent, not a bare one: the source check above is
                // text, and says nothing about the address the name resolves
                // to. An allowed name can still point at a private target,
                // and can point somewhere different on the next lookup.
                let response = sample_fetch::guarded_single_hop_get(&current, budget)?;
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
                    if !rule.allows(&next) {
                        return Err(format!(
                            "GET {current}: redirect target is outside the allowed source {rule}"
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

/// What a loaded body is, in a few words, with its size: a JSON object is a
/// sample map, and a file is known by its first bytes.
fn describe_loaded(bytes: &[u8]) -> String {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(bytes) {
        // `_base` and the other `_` keys are settings, not sounds.
        let sounds = map.keys().filter(|key| !key.starts_with('_')).count();
        let noun = if sounds == 1 { "sound" } else { "sounds" };
        return format!("sample map, {sounds} {noun}");
    }
    let head = |at: usize, magic: &[u8]| bytes.get(at..at + magic.len()) == Some(magic);
    let kind = if head(0, b"RIFF") && head(8, b"WAVE") {
        "WAV audio"
    } else if head(0, b"ID3") || matches!(bytes, [0xff, second, ..] if second & 0xe0 == 0xe0) {
        "MP3 audio"
    } else if head(0, b"OggS") {
        "Ogg audio"
    } else if head(0, b"fLaC") {
        "FLAC audio"
    } else if head(0, b"\x89PNG") || head(0, b"GIF8") || (head(0, b"RIFF") && head(8, b"WEBP")) {
        "image"
    } else if head(0, &[0xff, 0xd8, 0xff]) {
        "JPEG image"
    } else if head(0, &[0x1a, 0x45, 0xdf, 0xa3]) {
        "WebM video"
    } else if head(4, b"ftyp") {
        "MP4 video"
    } else {
        "data"
    };
    let size = match bytes.len() {
        small if small < 1024 => format!("{small} B"),
        kilo if kilo < 1024 * 1024 => format!("{} KB", kilo / 1024),
        mega => format!("{:.1} MB", mega as f64 / (1024.0 * 1024.0)),
    };
    format!("{kind}, {size}")
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

    /// A tried address says what it loaded in words a player reads.
    #[test]
    fn a_loaded_body_is_named_by_its_kind_and_size() {
        assert_eq!(
            describe_loaded(br#"{"_base": "https://a.example/", "bd": ["bd.wav"], "sd": []}"#),
            "sample map, 2 sounds"
        );
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.resize(3000, 0);
        assert_eq!(describe_loaded(&wav), "WAV audio, 2 KB");
        assert_eq!(describe_loaded(b"ID3\x04"), "MP3 audio, 4 B");
        assert_eq!(describe_loaded(b"\x89PNG\r\n"), "image, 6 B");
        assert_eq!(describe_loaded(b"\0\0\0\x20ftypisom"), "MP4 video, 12 B");
        assert_eq!(describe_loaded(b"<html>"), "data, 6 B");
        assert_eq!(
            describe_loaded(&vec![0; 3 * 1024 * 1024 / 2]),
            "data, 1.5 MB"
        );
    }

    /// A refusal names the source that would allow it once, for every clone
    /// of the policy, and allowing that source lets the same URL through.
    #[test]
    fn refused_sources_are_noted_once_and_allowing_one_clears_the_refusal() {
        let mut access = ScoreSampleAccess::denied();
        access.permit_builtin_sources();
        let clone = access.clone();
        for url in [
            "http://samples.example/a.json",
            "http://samples.example/b.json",
            "https://127.0.0.1/kit.json",
            "https://cdn.public.example/kit.json",
        ] {
            assert!(clone.approve_remote(url).is_err(), "{url}");
        }
        assert!(
            access
                .approve_remote("https://raw.githubusercontent.com/a/b/main/strudel.json")
                .is_ok()
        );
        assert_eq!(
            access.take_refused_sources(),
            [
                "http://samples.example",
                "https://127.0.0.1",
                "cdn.public.example"
            ]
        );
        assert!(access.take_refused_sources().is_empty(), "taken once");

        access
            .permit_source("http://samples.example")
            .expect("an address");
        assert!(
            access
                .approve_remote("http://samples.example/a.json")
                .is_ok()
        );
        let github = SourceRule::parse("raw.githubusercontent.com").unwrap();
        access.revoke_source(&github);
        assert!(
            access
                .approve_remote("https://raw.githubusercontent.com/a/b/main/strudel.json")
                .is_err()
        );
        assert_eq!(access.take_refused_sources(), ["raw.githubusercontent.com"]);
        assert_eq!(
            access.source_for("https://cdn.strudel.b-cdn.net/x.wav"),
            Ok(Some(&SourceRule::parse("strudel.b-cdn.net").unwrap()))
        );
        assert!(
            access.take_refused_sources().is_empty(),
            "asking notes nothing"
        );
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
