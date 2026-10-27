use super::*;

/// What the downloaded samples take on disk, in bytes.
///
/// The function walks the cache on each call. Loader threads and other
/// processes write to the same folder, so a running total would not be
/// accurate. The walk is bounded like the other scans here: a cache past
/// the limit reports what the walk counted.
pub fn sample_cache_usage() -> u64 {
    sample_cache_usage_at(&cache_dir())
}

pub(super) fn sample_cache_usage_at(base: &Path) -> u64 {
    const MAX_ENTRIES: usize = 200_000;
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut folders = vec![base.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return total;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file()
                && let Ok(metadata) = entry.metadata()
            {
                total = total.saturating_add(metadata.len());
            }
            // A symlink is neither: following one would let a link planted
            // in the cache fold somebody else's disk into this number.
        }
    }
    total
}

/// Empty the sample cache - everything downloaded, both the pinned banks'
/// files and the score-selected namespace beside them.
///
/// Nothing decoded is touched: what is already in RAM goes on playing, and
/// the next thing that needs a file fetches it again.
pub fn clear_sample_cache() -> Result<(), String> {
    clear_sample_cache_at(&cache_dir())
}

/// Whether a name is one the cache writes: the hex digest an entry is
/// filed under, with the extension its url had. Nothing else in the folder
/// is deleted: not the cache's own lock and marker, and not a file that
/// somebody keeps beside the cache.
///
/// The writer uses the first sixteen bytes of the digest
/// ([`cache_path_in_domain`]), which is thirty-two hex characters. A
/// sixty-four character name is a full digest, the other form this cache
/// has used, and also matches.
fn is_cache_entry_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let (stem, extension) = match name.split_once('.') {
        Some((stem, extension)) => (stem, Some(extension)),
        None => (name, None),
    };
    (stem.len() == 32 || stem.len() == 64)
        && stem
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && extension.is_none_or(|ext| {
            !ext.is_empty() && ext.len() <= 5 && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

pub(super) fn clear_sample_cache_at(base: &Path) -> Result<(), String> {
    // The score namespace has a lock and a marker protocol of its own, so
    // it is cleared through its own door rather than deleted from under it.
    clear_score_sample_cache_at(base)?;
    // The decoded-PCM sidecars answer to no manifest of their own: the
    // folder as a whole is this cache's, so the folder as a whole goes.
    let _ = std::fs::remove_dir_all(base.join(super::decode_cache::DECODED_DIR));
    let Ok(entries) = std::fs::read_dir(base) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        if !is_cache_entry_name(&entry.file_name()) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let path = entry.path();
        std::fs::remove_file(&path)
            .map_err(|error| format!("clear {}: {error}", path.display()))?;
    }
    Ok(())
}

/// Where the pinned and host-trusted sample files land: the same folder
/// [`cache_dir`] picks, reported so the CLI can say where a cache run wrote
/// to without each caller re-deriving the location.
pub fn sample_cache_dir() -> PathBuf {
    cache_dir()
}

/// Directory containing only persistently cached score-selected responses.
///
/// [`clear_score_sample_cache`] removes this directory without touching the
/// pinned and host-trusted sample cache beside it. The product's sample-cache
/// environment override, when set, selects the parent directory.
pub fn score_sample_cache_dir() -> PathBuf {
    cache_dir().join(SCORE_CACHE_NAMESPACE)
}

/// Remove the dedicated score cache and disable reads from its legacy shared
/// location.
///
/// Active Sessions may continue using bytes they already decoded. Their
/// per-Session admission counters deliberately do not reset: clearing storage
/// is not a way for one Session to exceed its write allowance.
pub fn clear_score_sample_cache() -> Result<(), String> {
    clear_score_sample_cache_at(&cache_dir())
}

impl Default for ScoreCacheLimits {
    fn default() -> Self {
        Self {
            global_bytes: SCORE_SAMPLE_CACHE_MAX_BYTES,
            global_entries: SCORE_SAMPLE_CACHE_MAX_ENTRIES,
            session_bytes: SCORE_SAMPLE_CACHE_SESSION_MAX_BYTES,
            session_entries: SCORE_SAMPLE_CACHE_SESSION_MAX_ENTRIES,
        }
    }
}

impl ScoreCacheKind {
    fn key_domain(self) -> &'static [u8] {
        match self {
            Self::Manifest => b"manifest",
            Self::Audio => b"audio",
        }
    }
}

impl ScoreCacheTrust {
    fn key_domain(self) -> &'static [u8] {
        match self {
            Self::Cors => b"cors",
            Self::Grant => b"grant",
        }
    }

    pub(super) fn from_cors_required(cors_required: bool) -> Self {
        if cors_required {
            Self::Cors
        } else {
            Self::Grant
        }
    }
}

impl ScoreCache {
    pub(super) fn new(base: PathBuf) -> Self {
        Self {
            base,
            limits: ScoreCacheLimits::default(),
            session: Mutex::new(ScoreCacheUsage::default()),
        }
    }

    #[cfg(test)]
    fn with_limits(base: PathBuf, limits: ScoreCacheLimits) -> Self {
        Self {
            base,
            limits,
            session: Mutex::new(ScoreCacheUsage::default()),
        }
    }

    pub(super) fn dir(&self) -> PathBuf {
        self.base.join(SCORE_CACHE_NAMESPACE)
    }

    pub(super) fn path(&self, url: &str, kind: ScoreCacheKind, trust: ScoreCacheTrust) -> PathBuf {
        score_cache_path(&self.dir(), url, kind, trust)
    }

    pub(super) fn open(&self, path: &Path) -> Result<Option<std::fs::File>, String> {
        let dir = self.dir();
        match std::fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.file_type().is_dir() => open_regular_cache_entry(path),
            Ok(_) => Err(format!(
                "score sample cache {} is not a directory",
                dir.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "inspect score sample cache {}: {error}",
                dir.display()
            )),
        }
    }

    /// Admit one already-validated body without evicting another score's
    /// cache entry.
    ///
    /// Refusal is intentionally an optimisation failure, not a sample
    /// failure: the caller still has validated bytes for the current run. This
    /// keeps the persistent bound hard without allowing an untrusted score to
    /// evict an offline reference corpus. Both budgets are charged while the
    /// process-wide cache lock is held and BEFORE a staging file is opened.
    #[cfg(test)]
    pub(super) fn admit(&self, path: &Path, bytes: &[u8]) -> Result<CacheAdmission, String> {
        let publication = PublicationGate::new();
        let budget = sample_fetch::FetchBudget::for_one_fetch();
        self.admit_with_publication(path, bytes, &publication, &budget)
    }

    pub(super) fn admit_with_publication(
        &self,
        path: &Path,
        bytes: &[u8],
        publication: &PublicationGate,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<CacheAdmission, String> {
        budget.check()?;
        let mut session = self.session.lock().expect("score cache session budget");
        with_score_cache_lock(&self.base, || {
            self.admit_locked(&mut session, path, bytes, publication, budget)
        })
    }

    fn admit_locked(
        &self,
        session: &mut ScoreCacheUsage,
        path: &Path,
        bytes: &[u8],
        publication: &PublicationGate,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<CacheAdmission, String> {
        budget.check()?;
        let incoming = u64::try_from(bytes.len())
            .map_err(|_| "score sample cache entry is too large to account".to_owned())?;
        let dir = self.dir();
        ensure_score_cache_dir(&dir)?;
        cleanup_score_cache_staging(&dir)?;

        // Another Session may have populated the same key while this one
        // fetched. Keeping the complete entry costs this Session nothing, even
        // when its own admission allowance is already spent.
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                return Ok(CacheAdmission::AlreadyPresent);
            }
            Ok(_) => {
                return Err(format!(
                    "score sample cache entry {} is not a regular file",
                    path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect score sample cache entry {}: {error}",
                    path.display()
                ));
            }
        }

        if session.entries >= self.limits.session_entries
            || session
                .bytes
                .checked_add(incoming)
                .is_none_or(|total| total > self.limits.session_bytes)
        {
            return Ok(CacheAdmission::Refused);
        }

        let (entries, occupied) = score_cache_occupancy(&dir)?;
        if entries >= self.limits.global_entries
            || occupied
                .checked_add(incoming)
                .is_none_or(|total| total > self.limits.global_bytes)
        {
            return Ok(CacheAdmission::Refused);
        }

        // Resource limits fire before allocation at the allocation site.
        // Charge before `commit_cache_entry` opens its staging file; roll the
        // charge back if the filesystem refuses the commit.
        session.entries += 1;
        session.bytes += incoming;
        if let Err(error) = commit_cache_entry(path, bytes, Some((publication, budget))) {
            // A failed parent-directory sync happens after rename published
            // the complete file. Keep that visible entry charged even though
            // its crash durability could not be confirmed.
            if !error.published {
                session.entries -= 1;
                session.bytes -= incoming;
            }
            return Err(format!(
                "write score sample cache entry {}: {error}",
                path.display()
            ));
        }
        Ok(CacheAdmission::Stored)
    }

    pub(super) fn remove(&self, path: &Path) -> Result<(), String> {
        with_score_cache_lock(&self.base, || match std::fs::remove_file(path) {
            Ok(()) => sync_parent_directory(path)
                .map_err(|error| format!("sync score sample cache removal: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove score sample cache entry {}: {error}",
                path.display()
            )),
        })
    }

    pub(super) fn read_legacy(
        &self,
        legacy: &Path,
        limit: usize,
    ) -> Result<Option<Vec<u8>>, String> {
        use std::io::Read;

        with_score_cache_lock(&self.base, || {
            if !legacy_reads_enabled_locked(&self.base)? {
                return Ok(None);
            }
            let Some(file) = open_regular_cache_entry(legacy)? else {
                return Ok(None);
            };
            let mut bytes = Vec::new();
            if !file
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .is_ok_and(|_| bytes.len() <= limit)
            {
                return Ok(None);
            }
            Ok(Some(bytes))
        })
    }

    #[cfg(test)]
    fn copy_legacy(&self, promoted: &Path, bytes: &[u8]) {
        let publication = PublicationGate::new();
        let budget = sample_fetch::FetchBudget::for_one_fetch();
        self.copy_legacy_with_publication(promoted, bytes, &publication, &budget);
    }

    pub(super) fn copy_legacy_with_publication(
        &self,
        promoted: &Path,
        bytes: &[u8],
        publication: &PublicationGate,
        budget: &sample_fetch::FetchBudget,
    ) {
        let mut session = self.session.lock().expect("score cache session budget");
        let _ = with_score_cache_lock(&self.base, || {
            budget.check()?;
            // An explicit clear may land after the legacy read and validation.
            // Re-check under the SAME lock as admission so an old root entry
            // cannot repopulate the namespace after clear returned.
            if !legacy_reads_enabled_locked(&self.base)? {
                return Ok(());
            }
            // The old namespace is shared with pinned and host-trusted data,
            // so its provenance cannot be recovered from the hash alone. Copy
            // an authorized, validated hit into the bounded namespace, but
            // never retire the only copy a trusted loader may know about.
            let _ = self.admit_locked(&mut session, promoted, bytes, publication, budget)?;
            Ok(())
        });
    }
}

fn legacy_reads_enabled_locked(base: &Path) -> Result<bool, String> {
    let marker = base.join(SCORE_CACHE_NO_LEGACY_MARKER);
    match std::fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(false),
        Ok(_) => Err(format!(
            "score sample cache marker {} is not a regular file",
            marker.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!(
            "inspect score sample cache marker {}: {error}",
            marker.display()
        )),
    }
}

fn score_cache_lock_path(base: &Path) -> PathBuf {
    base.join(".score-cache.lock")
}

pub(super) fn with_score_cache_lock<T>(
    base: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    ensure_private_dir(base).map_err(|error| format!("create sample cache {error}"))?;
    let lock_path = score_cache_lock_path(base);
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err("score sample cache lock is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect score sample cache lock: {error}")),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    // A cache directory may be shared between local accounts. Refuse a
    // pre-placed symlink rather than opening and locking its target.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options
        .open(&lock_path)
        .map_err(|error| format!("open score sample cache lock: {error}"))?;
    if !std::fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_file()) {
        return Err("score sample cache lock is not a regular file".to_owned());
    }
    lock.lock()
        .map_err(|error| format!("lock score sample cache: {error}"))?;
    operation()
}

fn ensure_score_cache_dir(dir: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => Err(format!(
            "score sample cache {} is not a directory",
            dir.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_private_dir(dir)
                .map_err(|error| format!("create score sample cache {}: {error}", dir.display()))?;
            sync_parent_directory(dir).map_err(|error| {
                format!(
                    "sync score sample cache directory {}: {error}",
                    dir.display()
                )
            })
        }
        Err(error) => Err(format!(
            "inspect score sample cache {}: {error}",
            dir.display()
        )),
    }
}

pub(super) fn is_score_cache_staging(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.get(..32).is_some_and(|hash| {
        hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            && name
                .get(32..)
                .is_some_and(|suffix| suffix.starts_with(".partial"))
    })
}

fn cleanup_score_cache_staging(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir)
        .map_err(|error| format!("read score sample cache {}: {error}", dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read score sample cache entry: {error}"))?;
        if is_score_cache_staging(&entry.path()) {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "remove incomplete score sample cache entry: {error}"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn score_cache_occupancy(dir: &Path) -> Result<(usize, u64), String> {
    let mut entries = 0usize;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(dir)
        .map_err(|error| format!("read score sample cache {}: {error}", dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read score sample cache entry: {error}"))?;
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|error| format!("inspect score sample cache entry: {error}"))?;
        if is_score_cache_staging(&entry.path()) {
            continue;
        }
        if !metadata.file_type().is_file() {
            return Err(format!(
                "score sample cache entry {} is not a regular file",
                entry.path().display()
            ));
        }
        entries = entries
            .checked_add(1)
            .ok_or_else(|| "score sample cache entry count overflow".to_owned())?;
        bytes = bytes
            .checked_add(metadata.len())
            .ok_or_else(|| "score sample cache byte count overflow".to_owned())?;
    }
    Ok((entries, bytes))
}

fn clear_score_sample_cache_at(base: &Path) -> Result<(), String> {
    with_score_cache_lock(base, || {
        // Disable compatibility reads FIRST. If removal below fails, clear
        // reports that failure, but old mixed-namespace entries cannot quietly
        // make score-selected data reappear.
        let marker = base.join(SCORE_CACHE_NO_LEGACY_MARKER);
        cleanup_no_legacy_marker_staging(base)?;
        match std::fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(format!(
                    "score sample cache marker {} is not a regular file",
                    marker.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                commit_cache_entry(&marker, b"score-cache-v1\n", None)
                    .map_err(|error| format!("create score sample cache clear marker: {error}"))?;
            }
            Err(error) => {
                return Err(format!(
                    "inspect score sample cache marker {}: {error}",
                    marker.display()
                ));
            }
        }
        // An earlier attempt may have published the marker but failed while
        // syncing this directory. Do not report a later clear as durable until
        // the marker's directory entry has reached storage too.
        sync_directory(base)
            .map_err(|error| format!("sync score sample cache clear marker: {error}"))?;

        let dir = base.join(SCORE_CACHE_NAMESPACE);
        match std::fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                std::fs::remove_file(&dir).map_err(|error| {
                    format!("remove score sample cache {}: {error}", dir.display())
                })?
            }
            Ok(_) => std::fs::remove_dir_all(&dir)
                .map_err(|error| format!("remove score sample cache {}: {error}", dir.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect score sample cache {}: {error}",
                    dir.display()
                ));
            }
        }
        // Persist the namespace removal before returning success. Syncing the
        // parent makes the root unlink durable even though the removed tree no
        // longer has a directory handle to flush.
        sync_directory(base)
            .map_err(|error| format!("sync cleared score sample cache: {error}"))?;
        Ok(())
    })
}

fn cleanup_no_legacy_marker_staging(base: &Path) -> Result<(), String> {
    let prefix = format!("{SCORE_CACHE_NO_LEGACY_MARKER}.partial");
    for entry in std::fs::read_dir(base)
        .map_err(|error| format!("read sample cache {}: {error}", base.display()))?
    {
        let entry = entry.map_err(|error| format!("read sample cache entry: {error}"))?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "remove incomplete score sample cache marker: {error}"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The identity of a request, for cache purposes: the URL as it goes on the
/// wire, plus the extension a human would expect the file to have.
///
/// The URL is parsed, not trimmed as text. `Url` serialises an empty path as
/// `/`, so `https://host` and `https://host/` share one identity. `/a` and
/// `/a/` stay distinct, and the query stays part of the identity: a slash
/// inside a query does not merge two resources.
///
/// `#` is percent-encoded before parsing and is not a fragment. Bank
/// filenames contain it, and `sample_fetch::fetch` puts `%23` on the wire,
/// so the key matches the request.
fn cache_identity(url: &str) -> (String, Option<String>) {
    let wire = url.replace('#', "%23");
    let Ok(parsed) = sample_fetch::wire_url(url) else {
        // Not a parseable URL; key on the text and take no extension
        // rather than guess one from an unparsed tail.
        return (wire, None);
    };
    // Take the extension from the path only. A dotted host must not supply
    // one: `https://example.com` and `https://example.com/` share an entry.
    let extension = parsed
        .path()
        .rsplit('/')
        .next()
        .and_then(|segment| segment.rsplit_once('.'))
        .map(|(_, ext)| ext)
        .filter(|ext| {
            ext.len() <= 5 && !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .map(str::to_owned);
    (parsed.as_str().to_owned(), extension)
}

impl std::fmt::Display for CacheCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    path.parent().map_or(Ok(()), sync_directory)
}

/// Persist a directory-entry change where the platform exposes directory
/// syncing through ordinary file handles.
#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// Atomic visibility remains portable; Rust does not expose a portable
/// directory-sync operation on other targets.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Put bytes at `path` so a reader sees all of them or none.
///
/// The staging name is unique per writer, not per target. Two Sessions in
/// one process share a cache directory, and each has a loader thread. With
/// a name keyed on the process id alone they would write to the same
/// staging inode, and a rename could publish mixed bytes.
///
/// The staging file is created with `create_new`, so a symlink that an
/// attacker placed in a shared cache directory causes a refusal, not a
/// write through the link.
fn commit_cache_entry(
    path: &Path,
    bytes: &[u8],
    publication: Option<(&PublicationGate, &sample_fetch::FetchBudget)>,
) -> Result<(), CacheCommitError> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static WRITER: AtomicU64 = AtomicU64::new(0);
    let staging = path.with_extension(format!(
        "partial{}.{}",
        std::process::id(),
        WRITER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| CacheCommitError {
            error,
            published: false,
        })?;
    let written = file.write_all(bytes).and_then(|()| {
        // Durable before the rename publishes it, so a crash cannot leave a
        // named entry whose contents never reached the disk.
        file.sync_all()
    });
    if let Err(error) = written {
        drop(file);
        let _ = std::fs::remove_file(&staging);
        return Err(CacheCommitError {
            error,
            published: false,
        });
    }
    drop(file);
    let _publication = match publication {
        Some((publication, budget)) => match publication.enter(PublicationKind::Cache, budget) {
            Ok(guard) => Some(guard),
            Err(error) => {
                let _ = std::fs::remove_file(&staging);
                return Err(CacheCommitError {
                    error: std::io::Error::new(std::io::ErrorKind::Interrupted, error),
                    published: false,
                });
            }
        },
        None => None,
    };
    match std::fs::rename(&staging, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&staging);
            Err(CacheCommitError {
                error,
                published: false,
            })
        }
    }?;
    // File sync makes the bytes durable; syncing the containing directory
    // makes the rename durable. Without both, a successful clear marker could
    // disappear after a power loss and silently re-enable legacy imports.
    sync_parent_directory(path).map_err(|error| CacheCommitError {
        error,
        published: true,
    })
}

pub(super) fn cache_path(dir: &Path, url: &str) -> PathBuf {
    cache_path_in_domain(dir, url, None)
}

fn score_cache_path(
    dir: &Path,
    url: &str,
    kind: ScoreCacheKind,
    trust: ScoreCacheTrust,
) -> PathBuf {
    cache_path_in_domain(dir, url, Some(&[kind.key_domain(), trust.key_domain()]))
}

fn cache_path_in_domain(dir: &Path, url: &str, domains: Option<&[&[u8]]>) -> PathBuf {
    let (identity, extension) = cache_identity(url);
    let mut hasher = Sha256::new();
    if let Some(domains) = domains {
        // Domain separation keeps a manifest and an audio response at the same
        // URL from sharing validation or eviction state, and keeps CORS-
        // consented bytes apart from grant-only ones. The legacy/trusted
        // layout deliberately retains its original URL-only identity above.
        hasher.update(SCORE_CACHE_KEY_DOMAIN_V1);
        for domain in domains {
            hasher.update(domain);
            hasher.update(b"\0");
        }
    }
    hasher.update(identity.as_bytes());
    let digest = hasher.finalize();
    let mut name = String::with_capacity(40);
    for byte in digest.iter().take(16) {
        name.push_str(&format!("{byte:02x}"));
    }
    // Kept only so a human poking at the cache can tell what a file is.
    if let Some(extension) = extension {
        name.push('.');
        name.push_str(&extension);
    }
    dir.join(name)
}

/// Open a cache entry for reading without following a link: `Ok(None)` when
/// the name is absent, and an error when it holds anything but a regular file.
pub(super) fn open_regular_cache_entry(path: &Path) -> Result<Option<std::fs::File>, String> {
    // One open, no follow: `symlink_metadata` then `File::open` races a
    // swapped symlink on a shared temp cache directory.
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        // A reparse point opens as itself, so the check below refuses it.
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    match options.open(path) {
        Ok(file) => {
            let metadata = file.metadata().map_err(|error| {
                format!("inspect sample cache entry {}: {error}", path.display())
            })?;
            if !metadata.is_file() || cache_entry_is_reparse_point(&metadata) {
                return Err(format!(
                    "sample cache entry {} is not a regular file",
                    path.display()
                ));
            }
            Ok(Some(file))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            if cache_open_followed_symlink(&error) {
                return Err(format!(
                    "sample cache entry {} is not a regular file",
                    path.display()
                ));
            }
            Err(format!(
                "open sample cache entry {}: {error}",
                path.display()
            ))
        }
    }
}

#[cfg(unix)]
fn cache_open_followed_symlink(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn cache_open_followed_symlink(_error: &std::io::Error) -> bool {
    false
}

/// Whether a Windows entry opened as itself is a reparse point: a link, or a
/// file whose data a filter serves and a raw read would not see.
#[cfg(windows)]
fn cache_entry_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn cache_entry_is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(path)
    }
}

pub(super) fn ensure_private_dir(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => Err(format!("{} is not a directory", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                match std::fs::symlink_metadata(parent) {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        ensure_private_dir(parent)?;
                    }
                    Err(error) => {
                        return Err(format!("inspect {}: {error}", parent.display()));
                    }
                }
            }
            match create_private_dir(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure_private_dir(path)
                }
                Err(error) => Err(format!("create {}: {error}", path.display())),
            }
        }
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

/// Read one already-located sample.
///
/// A `file://` URL reaches this function from two registrants only, and
/// each is a folder the host chose: `register_local_folder`, whose root an
/// explicit `--allow-local-samples` named, and `adopt_set_folder`, for the
/// studio's open set. Both write the URL as `file://` followed by the raw
/// filesystem path. Neither goes through `register_custom_value`, because
/// no part of the URL is score-chosen. This function does not decode the
/// path: a percent-encoded path is opened under its literal name.
///
/// A score's own sources never arrive here as `file://`. Every URL a score
/// writes goes through `register_custom_value`. A `samples('local:...')`
/// folder is published into `score_sources` before its banks, so the loader
/// routes it to that grant's reader, which parses and decodes the URL.
/// [`sample_fetch::fetch`] checks the URL again before it connects.
pub(crate) fn fetch_located(url: &str) -> Result<Vec<u8>, String> {
    if let Some(path) = url.strip_prefix("file://") {
        use std::io::Read;
        let ceiling = rustel_audio::sample_pcm_ceiling();
        let file = std::fs::File::open(path).map_err(|error| format!("read {path}: {error}"))?;
        // Ask the filesystem for the length first. One stat refuses a file
        // that is too big, and the error can state its size. Without the
        // stat, the read runs to the ceiling and the buffer doubles behind it.
        let length = file.metadata().ok().map(|meta| meta.len());
        if let Some(length) = length
            && length > ceiling as u64
        {
            return Err(format!(
                "{path} is {}, past the {} one sound can hold",
                rustel_audio::format_sample_bytes(length as usize),
                rustel_audio::format_sample_bytes(ceiling)
            ));
        }
        // The read stays bounded, because the reported length is not
        // reliable: a pipe or a device reports none (`/dev/zero` reports
        // zero and never ends), and a file can grow between the stat and
        // the read. `fs::read` reads the whole file before a caller can
        // compare its length, so the limit must apply during the read.
        let mut bytes = Vec::new();
        if let Some(length) = length.filter(|length| *length <= ceiling as u64) {
            // Known and inside the ceiling: take it in one allocation
            // rather than growing to twice the file on the way.
            bytes.reserve_exact(length as usize + 1);
        }
        file.take(ceiling as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read {path}: {error}"))?;
        if bytes.len() > ceiling {
            // The reader stopped at the ceiling, so this one cannot say the
            // size: nothing measured it, which is the point.
            return Err(format!(
                "{path} is over {}, the most one sound can hold",
                rustel_audio::format_sample_bytes(ceiling)
            ));
        }
        return Ok(bytes);
    }
    sample_fetch::fetch(url)
}

pub(super) fn fetch_cached_with_budget(
    dir: &Path,
    url: &str,
    budget: &sample_fetch::FetchBudget,
    publication: &PublicationGate,
) -> Result<Vec<u8>, String> {
    budget.check()?;
    // A file on disk is already its own cache, and copying it would mean an
    // edited sample keeps playing the stale copy.
    if url.starts_with("file://") {
        let bytes = fetch_located(url)?;
        budget.check()?;
        return Ok(bytes);
    }
    let path = cache_path(dir, url);
    if let Ok(bytes) = read_sample_cache(&path) {
        budget.check()?;
        return Ok(bytes);
    }
    let bytes = sample_fetch::fetch_audio_with_budget(url, budget)?;
    budget.check()?;
    if StagedCacheFile::create(&path, &bytes)
        .and_then(|staged| staged.publish(&path, publication, budget))
        .is_err()
    {
        // Caching is optional, but cancellation is not: a library dropped
        // while bytes were staged must not proceed as if the fetch completed.
        budget.check()?;
    }
    budget.check()?;
    Ok(bytes)
}

/// Fetch `url` through the host cache and decode it, evicting the entry when
/// `decode` refuses the bytes, whether they were fetched now or read from
/// disk. Mirrors the eviction half of [`fetch_score_source_cached_with_budget`].
pub(super) fn fetch_cached_decoded_with_budget<T>(
    dir: &Path,
    url: &str,
    budget: &sample_fetch::FetchBudget,
    publication: &PublicationGate,
    decode: impl FnOnce(&[u8]) -> Result<T, String>,
) -> Result<T, String> {
    let bytes = fetch_cached_with_budget(dir, url, budget, publication)?;
    decode(&bytes).inspect_err(|_| evict_host_cache_entry(dir, url))
}

/// Remove the host-cache entry for `url`, so the next ask fetches it again.
/// A `file://` URL is its own cache and is never removed.
pub(super) fn evict_host_cache_entry(dir: &Path, url: &str) {
    if !url.starts_with("file://") {
        let _ = std::fs::remove_file(cache_path(dir, url));
    }
}

impl StagedCacheFile {
    fn create(path: &Path, bytes: &[u8]) -> Result<Self, String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("cache path {} has no parent", path.display()))?;
        ensure_private_dir(parent).map_err(|error| format!("create sample cache {error}"))?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sample");
        let (staged, mut file) = loop {
            let sequence = NEXT_CACHE_STAGE.fetch_add(1, Ordering::Relaxed);
            let staged = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)
            {
                Ok(file) => break (staged, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("stage sample cache {}: {error}", staged.display()));
                }
            }
        };
        let staged = Self {
            path: staged,
            published: false,
        };
        if let Err(error) = file.write_all(bytes) {
            drop(file);
            return Err(format!(
                "write sample cache {}: {error}",
                staged.path.display()
            ));
        }
        if let Err(error) = file.sync_all() {
            drop(file);
            return Err(format!(
                "sync sample cache {}: {error}",
                staged.path.display()
            ));
        }
        drop(file);
        Ok(staged)
    }

    fn publish(
        mut self,
        destination: &Path,
        publication: &PublicationGate,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<(), String> {
        let _gate = publication.enter(PublicationKind::Cache, budget)?;
        std::fs::rename(&self.path, destination)
            .map_err(|error| format!("publish sample cache {}: {error}", destination.display()))?;
        self.published = true;
        Ok(())
    }
}

impl Drop for StagedCacheFile {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Mirrors [`read_sample_cache`] for manifests, under the manifest size limit.
pub(super) fn read_manifest_cache(path: &Path) -> Result<Vec<u8>, String> {
    let file = open_regular_cache_entry(path)?
        .ok_or_else(|| format!("read sample cache {}: not found", path.display()))?;
    let mut bytes = Vec::new();
    file.take(sample_fetch::MAX_REMOTE_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read sample cache {}: {error}", path.display()))?;
    if bytes.len() > sample_fetch::MAX_REMOTE_MANIFEST_BYTES {
        return Err(format!(
            "{} exceeds the sample manifest size limit",
            path.display()
        ));
    }
    Ok(bytes)
}

pub(super) fn publish_manifest_cache(
    path: &Path,
    bytes: &[u8],
    publication: &PublicationGate,
    budget: &sample_fetch::FetchBudget,
) -> Result<(), String> {
    budget.check()?;
    StagedCacheFile::create(path, bytes)?.publish(path, publication, budget)
}

pub(super) fn fetch_manifest_cached_with_budget(
    dir: &Path,
    url: &str,
    budget: &sample_fetch::FetchBudget,
    publication: &PublicationGate,
) -> Result<Vec<u8>, String> {
    budget.check()?;
    if url.starts_with("file://") {
        return fetch_manifest_located_with_budget(url, budget);
    }
    let path = cache_path(dir, url);
    if let Ok(bytes) = read_manifest_cache(&path) {
        budget.check()?;
        return Ok(bytes);
    }
    let bytes = fetch_manifest_located_with_budget(url, budget)?;
    budget.check()?;
    if publish_manifest_cache(&path, &bytes, publication, budget).is_err() {
        // Caches are an optimization, so an unwritable cache cannot make a
        // reachable manifest unusable. Cancellation and expiry are different:
        // neither may be hidden by falling through to map publication.
        budget.check()?;
    }
    budget.check()?;
    Ok(bytes)
}

/// A score-selected fetch that may use the on-disk cache.
///
/// The grant is checked before the cache is read. A score with no grant, or
/// with a grant for a different origin, must not read bytes that a granted
/// score left in the cache.
///
/// An approved remote URL may persist under the score cache. CORS-consented
/// bytes and exact-grant bytes keep separate identities, so a grant-only
/// admission cannot later satisfy the public-https/CORS policy for the same
/// URL. A hit serves the bytes whether or not the server is reachable, so
/// an offline render of a score that fetches its samples still works.
///
/// Entries are keyed by the full URL, kind, and trust mode. Reads are
/// size-bounded like a network body.
#[cfg(test)]
fn fetch_score_source_cached<T>(
    cache: &ScoreCache,
    url: &str,
    access: &ScoreFetchAccess,
    kind: ScoreCacheKind,
    limit: usize,
    validate: impl Fn(&[u8]) -> Result<T, String>,
) -> Result<T, String> {
    let publication = PublicationGate::new();
    let budget = sample_fetch::FetchBudget::for_one_fetch();
    fetch_score_source_cached_with_budget(
        cache,
        url,
        access,
        kind,
        limit,
        CacheFetchScope {
            budget: &budget,
            publication: &publication,
        },
        validate,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(test, unix))]
    mod cache_read_tests {
        //! The plain-cache readers open entries through
        //! [`open_regular_cache_entry`], so a link at an entry name is refused.

        use std::os::unix::fs::symlink;

        use super::*;

        /// Pins that both readers refuse a symlink at a cache entry name without
        /// reading its target, and still read a regular entry.
        #[test]
        fn cache_reads_refuse_a_symlink_at_the_entry_name() {
            let dir = tempfile::tempdir().expect("cache root");
            let victim = dir.path().join("victim");
            std::fs::write(&victim, b"secret").expect("victim");

            let sample = cache_path(dir.path(), "https://samples.example/kick.wav");
            symlink(&victim, &sample).expect("plant sample symlink");
            let error = read_sample_cache(&sample).expect_err("a sample symlink is refused");
            assert!(error.contains("not a regular file"), "{error}");

            let manifest = cache_path(dir.path(), "https://samples.example/kit.json");
            symlink(&victim, &manifest).expect("plant manifest symlink");
            let error = read_manifest_cache(&manifest).expect_err("a manifest symlink is refused");
            assert!(error.contains("not a regular file"), "{error}");
            assert_eq!(std::fs::read(&victim).expect("victim remains"), b"secret");

            let honest = cache_path(dir.path(), "https://samples.example/snare.wav");
            std::fs::write(&honest, b"sample").expect("regular sample entry");
            assert_eq!(read_sample_cache(&honest).expect("sample entry"), b"sample");
            std::fs::write(&honest, b"{}").expect("regular manifest entry");
            assert_eq!(read_manifest_cache(&honest).expect("manifest entry"), b"{}");
        }
    }

    #[cfg(test)]
    mod host_cache_eviction_tests {
        //! A host-cache entry whose decode or parse fails is evicted, so the next
        //! ask fetches it again; see [`evict_host_cache_entry`].

        use std::io::Write;
        use std::net::TcpListener;

        use super::*;
        use crate::samples::codec_tests::{sample_test_shared, two_zone_test_font};
        use crate::samples::manifest_worker_tests::{drain_request, tiny_wav};

        const IDLE: Duration = Duration::from_secs(5);

        /// Answer one request per body, in order, each with a 200, at a url ending
        /// in `name` that no earlier process can have cached.
        fn serve(name: &str, bodies: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let url = format!(
                "http://{}/{}-{unique}/{name}",
                listener.local_addr().expect("listener address"),
                std::process::id(),
            );
            let server = std::thread::spawn(move || {
                for body in bodies {
                    let (stream, _) = listener.accept().expect("a request");
                    let mut stream = drain_request(stream);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .expect("response head");
                    stream.write_all(&body).expect("response body");
                }
            });
            (url, server)
        }

        /// Pins that the sample loader evicts a trusted body that does not decode,
        /// whether it was fetched now or read from disk, and that the retry after
        /// the rest fetches again and keeps a body that decodes.
        #[test]
        fn a_sample_that_does_not_decode_is_evicted_and_fetched_again() {
            let dir = tempfile::tempdir().expect("cache root");
            let (url, server) = serve(
                "bd.wav",
                vec![b"<html>not audio</html>".to_vec(), tiny_wav()],
            );
            let url: Arc<str> = Arc::from(url);
            let entry = cache_path(dir.path(), &url);
            let new_library = || {
                SampleLibrary::with_background_loaders_at(
                    HashMap::new(),
                    Vec::new(),
                    HashMap::new(),
                    String::new(),
                    dir.path().to_path_buf(),
                    Loading::InBackground,
                )
                .expect("library")
            };
            let ask = |library: &SampleLibrary| {
                ensure_loading_shared(
                    &library.shared,
                    &url,
                    DecodeRate::Context,
                    LoadPriority::Now,
                );
                library.wait_until_idle(IDLE);
                match library.shared.by_url.read().expect("url table").get(&url) {
                    Some(UrlState::Ready { .. }) => Ok(()),
                    Some(UrlState::Failed { .. }) => Err(()),
                    _ => panic!("the load did not settle"),
                }
            };

            let library = new_library();
            assert!(ask(&library).is_err(), "an HTML page is not audio");
            assert!(
                !entry.exists(),
                "a fetched body that does not decode is evicted"
            );

            let rested = Instant::now() - FAILED_RETRY_AFTER - Duration::from_secs(1);
            library
                .shared
                .by_url
                .write()
                .expect("url table")
                .insert(url.clone(), UrlState::Failed { at: rested });
            assert!(
                ask(&library).is_ok(),
                "the retry after the rest reaches the server again"
            );
            server.join().expect("server");
            assert!(entry.exists(), "a body that decodes is kept");

            std::fs::write(&entry, b"corrupted").expect("corrupt the entry");
            assert!(
                ask(&new_library()).is_err(),
                "a later library does not serve a corrupt entry"
            );
            assert!(
                !entry.exists(),
                "a stored entry that does not decode is evicted"
            );
        }

        /// Pins that a soundfont body that does not decode is evicted, while a font
        /// refused only because the bank is full keeps its bytes.
        #[test]
        fn a_font_that_does_not_decode_is_evicted_but_one_the_bank_cannot_seat_is_kept() {
            let dir = tempfile::tempdir().expect("cache root");
            let url = "http://127.0.0.1:9/0000_Test.js";
            let entry = cache_path(dir.path(), url);
            let budget = sample_fetch::FetchBudget::for_one_fetch();
            let shared = sample_test_shared(1);

            std::fs::write(&entry, b"<html>not a font</html>").expect("a bad entry");
            assert!(
                load_font(dir.path(), url, &shared, &budget).is_err(),
                "an HTML page is not a font"
            );
            assert!(!entry.exists(), "bytes that do not decode are evicted");

            std::fs::write(&entry, two_zone_test_font()).expect("a good entry");
            let full = sample_test_shared((SAMPLE_BANK_CAPACITY - 1) as u32);
            let Err(error) = load_font(dir.path(), url, &full, &budget) else {
                panic!("two zones cannot fit in one remaining slot");
            };
            assert!(error.contains("sample bank capacity"), "{error}");
            assert!(
                entry.exists(),
                "a font the bank cannot seat keeps its bytes"
            );

            let zones =
                load_font(dir.path(), url, &shared, &budget).expect("the kept font installs");
            assert_eq!(zones.len(), 2);
        }

        /// Pins that a trusted `samples()` map whose body is not JSON leaves no
        /// host-cache entry, so registering it again fetches the map anew.
        #[test]
        fn a_manifest_that_is_not_json_is_evicted_and_fetched_again() {
            let (url, server) = serve(
                "strudel.json",
                vec![
                    b"<html>not a map</html>".to_vec(),
                    br#"{"tone":"http://127.0.0.1:9/tone.wav"}"#.to_vec(),
                ],
            );
            let map = serde_json::to_string(&url).expect("a quoted url");
            let entry = cache_path(&cache_dir(), &url);
            let library = SampleLibrary::empty();

            let error = library
                .register_trusted_custom(&map, None)
                .expect_err("an HTML page is not a map");
            assert!(error.contains("not JSON"), "{error}");
            assert!(!entry.exists(), "a body that is not JSON is evicted");

            library
                .register_trusted_custom(&map, None)
                .expect("the next registration fetches the map again");
            server.join().expect("server");
            library.wait_until_idle(IDLE);
            assert!(library.knows("tone"), "the map fetched again registers");
            assert!(entry.exists(), "a body that is JSON is kept");
            let _ = std::fs::remove_file(entry);
        }
    }

    #[cfg(test)]
    mod score_access_tests {
        use super::*;

        fn test_cache_base(label: &str) -> PathBuf {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            std::env::temp_dir().join(format!(
                "rustel-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
        }

        fn empty_library() -> SampleLibrary {
            SampleLibrary::with_background_loaders_at(
                HashMap::new(),
                Vec::new(),
                HashMap::new(),
                String::new(),
                test_cache_base("score-test-cache"),
                Loading::InBackground,
            )
            .expect("empty score-access library")
        }

        fn set_folder(files: &[&str]) -> tempfile::TempDir {
            let dir = tempfile::tempdir().expect("temp set folder");
            for file in files {
                let path = dir.path().join(file);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("sample folder");
                }
                // The scan reads names, not audio: an extension is the whole
                // of what makes a file a sample to it.
                std::fs::write(&path, b"").expect("sample file");
            }
            dir
        }

        /// The set's own folder is a sample source with nothing written and no
        /// grant passed, and it names what it finds the way a player would say
        /// it: a folder is a bank of variants, loose root files share one compact
        /// bank named after their folder while their filenames remain playable,
        /// and what the studio itself wrote into the set is not an instrument.
        #[test]
        fn a_set_folder_is_adopted_under_the_names_a_player_would_type() {
            let dir = set_folder(&[
                "deep bass.wav",
                "kicks/1.wav",
                "kicks/2.wav",
                "sessions/take 1.wav",
                "exports/drums-2026-08-25T18-04-11.wav",
                "song.strudel",
            ]);
            let library = empty_library();
            assert_eq!(library.adopt_set_folder(dir.path()), Ok(2));

            let catalogue = library.catalogue();
            let named = |name: &str| catalogue.iter().find(|entry| entry.name == name);
            let root_name = dir
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .expect("temporary folder name");
            let loose = named(root_name).expect("loose files share the root bank");
            assert_eq!(loose.variants, 1);
            assert_eq!(loose.variant_names, ["deep bass"]);
            assert_eq!(loose.origin, SoundOrigin::Set);
            assert_eq!(loose.category, SoundCategory::Set);
            // Being named is half of it. The URL has to be the one the loader
            // opens, and a name the filesystem spells with a space in it is
            // exactly where a percent-encoding would quietly lose the file.
            let location = loose.location.clone().expect("a location");
            assert!(
                fetch_located(&location).is_ok(),
                "the set's own sample reads back through the loader: {location}"
            );
            assert_eq!(
                named("kicks").map(|entry| entry.variants),
                Some(2),
                "a folder is one bank of variants"
            );
            assert!(
                named("take 1").is_none() && named("sessions").is_none(),
                "the set's own takes are not instruments"
            );
            assert!(
                named("drums-2026-08-25T18-04-11").is_none() && named("exports").is_none(),
                "and neither are its bounces"
            );
            assert!(library.knows("kicks"), "and the score can name it");
        }

        /// A set's samples belong to that set: opening another one takes them
        /// back out, or one set's `kicks` would play in the next. A folder that
        /// will not read is still another set, so it takes them out too.
        #[test]
        fn the_set_that_was_open_takes_its_samples_with_it() {
            let library = empty_library();
            let first = set_folder(&["kicks/1.wav"]);
            let second = set_folder(&["claps/1.wav"]);
            assert_eq!(library.adopt_set_folder(first.path()), Ok(1));
            assert!(library.knows("kicks"));
            assert_eq!(library.adopt_set_folder(second.path()), Ok(1));
            assert!(library.knows("claps"));
            assert!(!library.knows("kicks"), "the first set's bank went with it");

            // Leaving the set that was open does not depend on the next one
            // reading: a folder that will not open still closed the last.
            assert!(
                library
                    .adopt_set_folder(&second.path().join("nope"))
                    .is_err()
            );
            assert!(
                !library.knows("claps"),
                "a set that would not read still took the last one's banks with it"
            );
        }

        /// A failed rescan of the same set must not remove sounds already on stage.
        #[test]
        fn a_failed_set_refresh_keeps_the_current_samples() {
            let library = empty_library();
            let set = set_folder(&["kicks/1.wav"]);
            assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
            assert!(
                library
                    .refresh_set_folder(&set.path().join("missing"))
                    .is_err()
            );
            assert!(
                library.knows("kicks"),
                "the last good bank remains playable"
            );
        }

        /// A new bank that would exceed the session cap cannot evict the set's old
        /// bank or replace its variant list during a refresh.
        #[test]
        fn a_full_session_keeps_the_old_set_bank_when_refresh_exceeds_capacity() {
            let library = empty_library();
            let set = set_folder(&["kicks/1.wav"]);
            assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
            let limit = SAMPLE_BANK_CAPACITY - 1;
            {
                let mut custom = library.custom.write().expect("custom banks");
                for index in 0..limit - 1 {
                    custom.insert(
                        format!("bank{index}"),
                        Bank::Array(vec![Arc::from("https://samples.example/a.wav")]),
                    );
                }
            }
            std::fs::write(set.path().join("kicks/2.wav"), b"").expect("second variant");
            std::fs::create_dir(set.path().join("extra")).expect("new bank folder");
            std::fs::write(set.path().join("extra/1.wav"), b"").expect("new bank");
            let error = library
                .refresh_set_folder(set.path())
                .expect_err("the refresh would exceed capacity");
            assert!(error.contains("limit"), "{error}");
            assert!(!library.knows("extra"));
            assert_eq!(
                library
                    .catalogue()
                    .iter()
                    .find(|entry| entry.name == "kicks")
                    .map(|entry| entry.variants),
                Some(1),
                "the original set bank remains playable and unchanged"
            );
        }

        /// Re-reading the open set replaces its banks and their variant lists.
        #[test]
        fn a_set_refresh_picks_up_added_audio() {
            let library = empty_library();
            let set = set_folder(&["kicks/1.wav"]);
            assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
            std::fs::write(set.path().join("kicks/2.wav"), b"").expect("second variant");
            std::fs::create_dir(set.path().join("fresh")).expect("new bank folder");
            std::fs::write(set.path().join("fresh/1.wav"), b"").expect("new bank");
            assert_eq!(library.refresh_set_folder(set.path()), Ok(2));
            assert!(library.knows("fresh"));
            let catalogue = library.catalogue();
            assert_eq!(
                catalogue
                    .iter()
                    .find(|entry| entry.name == "kicks")
                    .map(|entry| entry.variants),
                Some(2)
            );
        }

        /// A refresh leaves a name a score redefined with the score, so an update
        /// cannot change which `bd` plays.
        #[test]
        fn a_set_refresh_leaves_a_name_the_score_redefined_with_the_score() {
            let library = empty_library();
            let set = set_folder(&["bd/0.wav"]);
            assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
            let mut access = ScoreSampleAccess::denied();
            access
                .permit_origin("https://samples.example")
                .expect("origin");
            library
                .register_score_custom(r#"{"bd":"https://samples.example/bd.wav"}"#, None, &access)
                .expect("a granted origin");

            assert_eq!(library.refresh_set_folder(set.path()), Ok(0));
            let bd = library
                .catalogue()
                .into_iter()
                .find(|entry| entry.name == "bd")
                .expect("a bd bank");
            assert_eq!(
                bd.location.as_deref(),
                Some("https://samples.example/bd.wav")
            );
            assert_eq!(bd.origin, SoundOrigin::Score);
        }

        /// Most sets hold no audio at all, which is not a failure.
        #[test]
        fn a_set_folder_with_no_audio_in_it_adopts_nothing() {
            let dir = set_folder(&["song.strudel"]);
            let library = empty_library();
            assert_eq!(library.adopt_set_folder(dir.path()), Ok(0));
        }

        /// The `kicks` bank holds the same files in the same order whatever the
        /// set's folder is called. A loose `kicks.wav` belongs to the bank named
        /// after the folder and does not join `kicks`.
        #[test]
        fn a_merged_bank_orders_its_variants_the_same_way_in_every_set() {
            let variants = |set: &str| {
                let parent = tempfile::tempdir().expect("temp parent");
                let dir = parent.path().join(set);
                for file in ["kicks.wav", "kicks/1.wav", "kicks/2.wav"] {
                    let path = dir.join(file);
                    if let Some(folder) = path.parent() {
                        std::fs::create_dir_all(folder).expect("bank folder");
                    }
                    std::fs::write(&path, b"").expect("sample file");
                }
                let library = empty_library();
                library.adopt_set_folder(&dir).expect("adopted");
                let root = dir.canonicalize().expect("canonical set folder");
                // Use this platform's separator: `root.join(...)` builds the
                // URLs, and on Windows a prefix that ends in `/` matches nothing.
                let prefix = format!("{}{}", local_file_url(&root), std::path::MAIN_SEPARATOR);
                let custom = library.custom.read().expect("custom banks");
                let Bank::Array(urls) = custom.get("kicks").expect("a kicks bank") else {
                    panic!("a folder of audio is an array bank");
                };
                // Relative to the set, so two sets named differently compare.
                urls.iter()
                    .map(|url| url.strip_prefix(prefix.as_str()).unwrap_or(url).to_owned())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                variants("aaa techno"),
                variants("zzz techno"),
                "the same audio in the same shape is the same bank whatever the set is called"
            );
            assert_eq!(
                variants("zzz techno").first().map(String::as_str),
                Some(format!("kicks{}1.wav", std::path::MAIN_SEPARATOR).as_str()),
                "the explicit folder bank keeps its own numbered files"
            );
        }

        /// A bank the set's folder takes a name from is the set's while that set
        /// is open - and the score's again at the next one, because the set only
        /// ever owned the folder it was handed, not the name.
        #[test]
        fn a_name_the_set_took_over_goes_back_to_the_score_that_held_it() {
            let library = empty_library();
            let mut access = ScoreSampleAccess::denied();
            access
                .permit_origin("https://samples.example")
                .expect("origin");
            library
                .register_score_custom(r#"{"bd":"https://samples.example/bd.wav"}"#, None, &access)
                .expect("a granted origin");
            let score_url = "https://samples.example/bd.wav";
            let location = |library: &SampleLibrary| {
                library
                    .catalogue()
                    .into_iter()
                    .find(|entry| entry.name == "bd")
                    .expect("a bd bank")
            };
            assert_eq!(location(&library).location.as_deref(), Some(score_url));

            let set = set_folder(&["bd/0.wav"]);
            assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
            let taken = location(&library);
            assert!(
                taken.location.is_some_and(|url| url.starts_with("file://")),
                "the set's own bd is the one sounding while the set is open"
            );
            assert_eq!(taken.origin, SoundOrigin::Set, "and the browser says so");

            let next = set_folder(&["song.strudel"]);
            assert_eq!(library.adopt_set_folder(next.path()), Ok(0));
            let given_back = location(&library);
            assert_eq!(
                given_back.location.as_deref(),
                Some(score_url),
                "the score's bd came back with the set that took it"
            );
            assert_eq!(
                given_back.origin,
                SoundOrigin::Score,
                "and reads as the score's"
            );
        }

        /// And the other way round. A score that redefines a name the set's
        /// folder brought owns it from then on: the browser says whose it is,
        /// and the next set leaves it alone rather than putting the set's old
        /// bank back over the score's.
        #[test]
        fn a_score_that_redefines_a_set_bank_owns_the_name_from_then_on() {
            let library = empty_library();
            let set = set_folder(&["bd/0.wav"]);
            assert_eq!(library.adopt_set_folder(set.path()), Ok(1));
            let bd = |library: &SampleLibrary| {
                library
                    .catalogue()
                    .into_iter()
                    .find(|entry| entry.name == "bd")
                    .expect("a bd bank")
            };
            assert_eq!(bd(&library).origin, SoundOrigin::Set, "the set brought it");

            let mut access = ScoreSampleAccess::denied();
            access
                .permit_origin("https://samples.example")
                .expect("origin");
            library
                .register_score_custom(r#"{"bd":"https://samples.example/bd.wav"}"#, None, &access)
                .expect("a granted origin");
            assert_eq!(
                bd(&library).origin,
                SoundOrigin::Score,
                "a score's redefinition is the score's, not the set's"
            );

            let next = set_folder(&["song.strudel"]);
            assert_eq!(library.adopt_set_folder(next.path()), Ok(0));
            assert_eq!(
                bd(&library).location.as_deref(),
                Some("https://samples.example/bd.wav"),
                "and the set that closed did not put its own bd back over it"
            );
        }

        /// The bank limit is the session's, not the set folder's: the message
        /// says which, and a name the set shares with a bank already registered
        /// replaces that bank rather than adding to the count.
        #[test]
        fn the_bank_limit_counts_what_the_session_holds() {
            let library = empty_library();
            let limit = SAMPLE_BANK_CAPACITY - 1;
            {
                let mut custom = library.custom.write().expect("custom banks");
                for index in 0..limit {
                    custom.insert(
                        format!("bank{index}"),
                        Bank::Array(vec![Arc::from("https://samples.example/a.wav")]),
                    );
                }
            }
            // At the limit, and the set's folder shares a name with a bank that
            // is already there. Replacing it adds nothing, so there is room.
            let shared = set_folder(&["bank0/0.wav"]);
            assert_eq!(library.adopt_set_folder(shared.path()), Ok(1));
            assert!(library.knows("bank0"), "the name is still a bank");

            // One past it, and the refusal names the session that is full rather
            // than the folder that holds one file.
            let error = library
                .adopt_set_folder(set_folder(&["extra/0.wav"]).path())
                .expect_err("a session at its limit has no room for another bank");
            assert!(
                error.contains("session") && !error.contains("folder"),
                "the limit is the session's, not the set folder's: {error}"
            );
        }

        #[test]
        fn score_sample_access_is_denied_until_the_host_grants_it() {
            let library = empty_library();
            let error = library
                .register_score_custom(
                    r#"{"bank":"https://samples.example/kick.wav"}"#,
                    None,
                    &ScoreSampleAccess::denied(),
                )
                .expect_err("the default policy must not register a URL");
            assert!(error.contains("without a host grant"), "{error}");
            assert!(library.custom.read().expect("custom banks").is_empty());
            assert!(
                library
                    .shared
                    .score_sources
                    .read()
                    .expect("score sources")
                    .is_empty()
            );
        }

        /// A score map is bounded by the files it may NAME, not by the decoded
        /// bank's slots: naming costs a grant, decoding happens per played sound.
        /// `github:bubobubobubobubo/dough-waveforms` names 4,358 wavetables
        /// across 65 banks; under the old 2,047 bound the whole map was refused
        /// and every `s("wt_…")` reached the voice as unknown.
        #[test]
        fn a_wavetable_library_larger_than_the_decoded_bank_registers() {
            fn library_map(banks: usize, files_per_bank: usize) -> String {
                let banks = (0..banks)
                    .map(|bank| {
                        let files = (0..files_per_bank)
                            .map(|file| format!("\"wt_{bank:02}/AKWF_{file:04}.wav\""))
                            .collect::<Vec<_>>()
                            .join(",");
                        format!("\"wt_{bank:02}\":[{files}]")
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{{{banks}}}")
            }
            let mut access = ScoreSampleAccess::denied();
            access
                .permit_origin("https://samples.example")
                .expect("origin");

            let library = empty_library();
            library
                .register_score_custom(
                    &library_map(65, 68),
                    Some("https://samples.example/dough/"),
                    &access,
                )
                .expect("4,420 named files fit: none of them is decoded yet");
            assert_eq!(
                library
                    .shared
                    .score_sources
                    .read()
                    .expect("score sources")
                    .len(),
                65 * 68
            );
            assert!(library.knows("wt_64"), "the last bank is there to play");

            let error = library
                .register_score_custom(
                    &library_map(1, MAX_SCORE_SAMPLE_FILES + 1),
                    Some("https://samples.example/huge/"),
                    &access,
                )
                .expect_err("a map past the naming bound is still refused");
            assert!(
                error.contains(&format!("{MAX_SCORE_SAMPLE_FILES}-file limit")),
                "{error}"
            );
        }

        #[test]
        fn an_origin_grant_is_exact_and_applies_to_every_map_entry() {
            let library = empty_library();
            let mut access = ScoreSampleAccess::denied();
            access
                .permit_origin("https://samples.example")
                .expect("origin");
            library
                .register_score_custom(
                    r#"{"safe":["kick.wav","https://samples.example/snare.wav?take=2"]}"#,
                    Some("https://samples.example/kit/"),
                    &access,
                )
                .expect("same-origin entries");
            let sources = library.shared.score_sources.read().expect("score sources");
            assert!(sources.contains_key("https://samples.example/kit/kick.wav"));
            assert!(sources.contains_key("https://samples.example/snare.wav?take=2"));
            drop(sources);

            for (map, expected) in [
                (
                    r#"{"blocked":"https://other.example/kick.wav"}"#,
                    "outside the permitted sample origins",
                ),
                (
                    r#"{"blocked":"file:///tmp/not-a-score-sample.wav"}"#,
                    "is a file: address; only http and https are allowed",
                ),
            ] {
                let error = library
                    .register_score_custom(map, None, &access)
                    .expect_err("an entry outside the grant must fail the whole map");
                assert!(error.contains(expected), "{error}");
            }
            assert!(
                !library
                    .custom
                    .read()
                    .expect("custom banks")
                    .contains_key("blocked"),
                "a rejected map was partially published"
            );

            let credentialed = format!(
                r#"{{"blocked":"https://name:secret{}samples.example/kick.wav"}}"#,
                char::from(64)
            );
            let error = library
                .register_score_custom(&credentialed, None, &access)
                .expect_err("credentials must not be accepted in a score-selected URL");
            assert!(error.contains("contains credentials"), "{error}");
        }

        /// Read a request head off `stream` before answering it.
        ///
        /// Writing a response and closing while the client is still sending
        /// resets the connection, which surfaces as a network error instead of
        /// the response - intermittently, depending on scheduling. Every test
        /// server here consumes the request first for that reason.
        #[cfg(test)]
        fn drain_request_head(stream: &std::net::TcpStream) {
            let Ok(clone) = stream.try_clone() else {
                return;
            };
            let mut reader = std::io::BufReader::new(clone);
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::BufRead::read_line(&mut reader, &mut line) {
                    Ok(0) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }

        /// Bytes reach the cache only once something has agreed they are what
        /// they claim to be, and an entry that stops validating is dropped rather
        /// than served forever.
        ///
        /// The case that motivates it: an origin returns an HTML error page with
        /// status 200. Committing first would make that page the permanent
        /// manifest for the URL, surviving restarts, with no way to retry.
        #[test]
        fn only_validated_bytes_are_committed_and_bad_entries_are_evicted() {
            use std::io::Write;
            use std::net::TcpListener;

            let dir =
                std::env::temp_dir().join(format!("rustel-cache-valid-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(&dir);
            let cache = ScoreCache::new(dir.clone());

            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let url = format!("{origin}/kit.json");
            let server = std::thread::spawn(move || {
                for body in ["<html>not json</html>", "{\"ok\":true}"] {
                    let Ok((mut stream, _)) = listener.accept() else {
                        return;
                    };
                    let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match std::io::BufRead::read_line(&mut reader, &mut line) {
                            Ok(0) => break,
                            Ok(_) if line == "\r\n" || line == "\n" => break,
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.flush();
                }
            });

            let access = ScoreFetchAccess::Remote {
                origin: origin.clone(),
                cors_required: false,
            };
            let as_json = |bytes: &[u8]| {
                serde_json::from_slice::<serde_json::Value>(bytes)
                    .map_err(|error| error.to_string())
            };

            // A 200 that is not JSON is refused AND leaves nothing behind.
            assert!(
                fetch_score_source_cached(
                    &cache,
                    &url,
                    &access,
                    ScoreCacheKind::Manifest,
                    4096,
                    as_json,
                )
                .is_err(),
                "an unparseable body must not be accepted"
            );
            assert!(
                !cache
                    .path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant)
                    .exists(),
                "an unvalidated body must not be committed"
            );

            // The next attempt reaches the server again and succeeds, and THAT is
            // what gets stored.
            let value = fetch_score_source_cached(
                &cache,
                &url,
                &access,
                ScoreCacheKind::Manifest,
                4096,
                as_json,
            )
            .expect("the valid body is accepted");
            assert_eq!(value["ok"], serde_json::json!(true));
            assert!(
                cache
                    .path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant)
                    .exists(),
                "valid bytes are committed"
            );
            server.join().expect("server");

            // A stored entry that stops validating is evicted, not served. The
            // listener is closed now, so the retry has nowhere to go and errors -
            // which is exactly how eviction shows.
            std::fs::write(
                cache.path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant),
                b"corrupted",
            )
            .expect("corrupt the entry");
            assert!(
                fetch_score_source_cached(
                    &cache,
                    &url,
                    &access,
                    ScoreCacheKind::Manifest,
                    4096,
                    as_json,
                )
                .is_err(),
                "a corrupt entry must not be served"
            );
            assert!(
                !cache
                    .path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant)
                    .exists(),
                "a corrupt entry must be evicted"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn one_content_kind_cannot_evict_another_kinds_entry() {
            use std::io::Write;
            use std::net::TcpListener;

            let dir = std::env::temp_dir()
                .join(format!("rustel-cache-content-kind-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let cache = ScoreCache::new(dir.clone());
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let url = format!("{origin}/shared-resource");
            let access = ScoreFetchAccess::Remote {
                origin: origin.clone(),
                cors_required: false,
            };

            let manifest_path = cache.path(&url, ScoreCacheKind::Manifest, ScoreCacheTrust::Grant);
            cache
                .admit(&manifest_path, br#"{"bank":[]}"#)
                .expect("seed manifest cache");
            let audio_path = cache.path(&url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant);
            assert_ne!(manifest_path, audio_path, "content kinds shared one key");

            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("request");
                drain_request_head(&stream);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot audio"
                )
                .expect("response");
            });
            let error = fetch_score_source_cached(
                &cache,
                &url,
                &access,
                ScoreCacheKind::Audio,
                1024,
                |_| Err::<Vec<u8>, _>("audio validation failed".to_owned()),
            )
            .expect_err("the audio form must fail validation");
            assert!(error.contains("audio validation failed"), "{error}");
            server.join().expect("server");

            assert!(
                manifest_path.exists(),
                "an audio validator evicted the manifest cache entry"
            );
            let manifest: serde_json::Value = fetch_score_source_cached(
                &cache,
                &url,
                &access,
                ScoreCacheKind::Manifest,
                1024,
                |bytes| serde_json::from_slice(bytes).map_err(|error| error.to_string()),
            )
            .expect("the manifest remains available offline");
            assert_eq!(manifest["bank"], serde_json::json!([]));
            let _ = std::fs::remove_dir_all(&dir);
        }

        fn quota_limits(
            global_bytes: u64,
            global_entries: usize,
            session_bytes: u64,
            session_entries: usize,
        ) -> ScoreCacheLimits {
            ScoreCacheLimits {
                global_bytes,
                global_entries,
                session_bytes,
                session_entries,
            }
        }

        #[test]
        fn score_cache_refuses_before_staging_without_evicting_existing_entries() {
            let dir = std::env::temp_dir()
                .join(format!("rustel-cache-global-quota-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let cache = ScoreCache::with_limits(dir.clone(), quota_limits(6, 2, 100, 10));
            let first = cache.path(
                "https://samples.example/first.wav",
                ScoreCacheKind::Audio,
                ScoreCacheTrust::Grant,
            );
            let second = cache.path(
                "https://samples.example/second.wav",
                ScoreCacheKind::Audio,
                ScoreCacheTrust::Grant,
            );
            let third = cache.path(
                "https://samples.example/third.wav",
                ScoreCacheKind::Audio,
                ScoreCacheTrust::Grant,
            );

            assert_eq!(
                cache.admit(&first, b"1234").expect("first admission"),
                CacheAdmission::Stored
            );
            assert_eq!(
                cache.admit(&second, b"567").expect("byte refusal"),
                CacheAdmission::Refused,
                "a write crossing the byte ceiling must be refused"
            );
            assert_eq!(std::fs::read(&first).expect("first remains"), b"1234");
            assert!(!second.exists(), "a refused target was created");
            assert!(
                std::fs::read_dir(cache.dir())
                    .expect("cache entries")
                    .all(|entry| !is_score_cache_staging(&entry.expect("entry").path())),
                "a refused admission opened a staging file"
            );

            // The exact byte boundary is admitted. A second ScoreCache models a
            // second Session: its independent allowance cannot bypass the shared
            // on-disk entry ceiling.
            let other = ScoreCache::with_limits(dir.clone(), quota_limits(6, 2, 100, 10));
            assert_eq!(
                other.admit(&second, b"56").expect("boundary admission"),
                CacheAdmission::Stored
            );
            assert_eq!(
                other.admit(&third, b"").expect("entry refusal"),
                CacheAdmission::Refused
            );
            assert!(!third.exists(), "entry-limit refusal created a target");
            assert_eq!(
                score_cache_occupancy(&cache.dir()).expect("occupancy"),
                (2, 6)
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn each_session_has_an_independent_cache_write_allowance() {
            let dir = std::env::temp_dir()
                .join(format!("rustel-cache-session-quota-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let limits = quota_limits(100, 10, 4, 1);
            let first_session = ScoreCache::with_limits(dir.clone(), limits);
            let second_session = ScoreCache::with_limits(dir.clone(), limits);
            let first = first_session.path(
                "https://samples.example/first.wav",
                ScoreCacheKind::Audio,
                ScoreCacheTrust::Grant,
            );
            let second = first_session.path(
                "https://samples.example/second.wav",
                ScoreCacheKind::Audio,
                ScoreCacheTrust::Grant,
            );

            assert_eq!(
                first_session
                    .admit(&first, b"1234")
                    .expect("first session admission"),
                CacheAdmission::Stored,
                "the exact byte boundary must be admitted"
            );
            assert_eq!(
                first_session
                    .admit(&second, b"x")
                    .expect("first session refusal"),
                CacheAdmission::Refused
            );
            assert_eq!(
                second_session
                    .admit(&second, b"x")
                    .expect("second session admission"),
                CacheAdmission::Stored,
                "one Session's allowance leaked into another"
            );
            assert_eq!(
                first_session
                    .admit(&first, b"ignored")
                    .expect("existing entry"),
                CacheAdmission::AlreadyPresent,
                "hits must not consume or require Session admission allowance"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn concurrent_sessions_cannot_race_past_the_global_cache_bound() {
            let dir = std::env::temp_dir().join(format!(
                "rustel-cache-concurrent-quota-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let limits = quota_limits(4, 1, 100, 10);
            let first = Arc::new(ScoreCache::with_limits(dir.clone(), limits));
            let second = Arc::new(ScoreCache::with_limits(dir.clone(), limits));
            let barrier = Arc::new(std::sync::Barrier::new(3));

            let spawn =
                |cache: Arc<ScoreCache>, url: &'static str, barrier: Arc<std::sync::Barrier>| {
                    std::thread::spawn(move || {
                        let path = cache.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant);
                        barrier.wait();
                        cache.admit(&path, b"1234").expect("admission")
                    })
                };
            let a = spawn(
                Arc::clone(&first),
                "https://samples.example/a.wav",
                Arc::clone(&barrier),
            );
            let b = spawn(
                Arc::clone(&second),
                "https://samples.example/b.wav",
                Arc::clone(&barrier),
            );
            barrier.wait();
            let outcomes = [a.join().expect("first"), b.join().expect("second")];
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|outcome| **outcome == CacheAdmission::Stored)
                    .count(),
                1
            );
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|outcome| **outcome == CacheAdmission::Refused)
                    .count(),
                1
            );
            assert_eq!(
                score_cache_occupancy(&first.dir()).expect("occupancy"),
                (1, 4)
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn legacy_entries_copy_only_when_admitted_and_clear_disables_reimport() {
            let dir = std::env::temp_dir().join(format!(
                "rustel-cache-legacy-migration-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("cache root");
            let url = "http://127.0.0.1:9/kick.wav";
            let access = ScoreFetchAccess::Remote {
                origin: "http://127.0.0.1:9".to_owned(),
                cors_required: false,
            };
            let legacy = cache_path(&dir, url);
            std::fs::write(&legacy, b"legacy").expect("legacy entry");

            let refused = ScoreCache::with_limits(dir.clone(), quota_limits(0, 0, 0, 0));
            assert_eq!(
                fetch_score_source_cached(
                    &refused,
                    url,
                    &access,
                    ScoreCacheKind::Audio,
                    64,
                    |bytes| { Ok(bytes.to_vec()) },
                )
                .expect("legacy hit remains usable"),
                b"legacy"
            );
            assert!(
                legacy.exists(),
                "refused migration destroyed the offline copy"
            );
            assert!(
                !refused
                    .path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant)
                    .exists()
            );

            let admitted = ScoreCache::with_limits(dir.clone(), quota_limits(64, 4, 64, 4));
            assert_eq!(
                fetch_score_source_cached(
                    &admitted,
                    url,
                    &access,
                    ScoreCacheKind::Audio,
                    64,
                    |bytes| Ok(bytes.to_vec()),
                )
                .expect("legacy migration"),
                b"legacy"
            );
            assert!(
                admitted
                    .path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant)
                    .exists(),
                "legacy copy was not committed"
            );
            assert!(
                legacy.exists(),
                "migration removed a possibly host-trusted cache entry"
            );

            // Clearing leaves unrelated host-trusted cache files alone, but a
            // durable marker prevents one of those mixed-namespace files from
            // becoming score-selected cache data again.
            std::fs::write(&legacy, b"trusted cache").expect("trusted cache entry");
            let in_flight = admitted
                .read_legacy(&legacy, 64)
                .expect("legacy read before clear")
                .expect("legacy bytes before clear");
            clear_score_sample_cache_at(&dir).expect("clear score cache");
            // This models the exact clear/migration race: a fetch read and
            // validated legacy bytes, then clear completed before its admission.
            admitted.copy_legacy(
                &admitted.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant),
                &in_flight,
            );
            assert!(!admitted.dir().exists(), "score namespace survived clear");
            assert!(legacy.exists(), "clear removed a host-trusted cache entry");
            assert!(
                dir.join(SCORE_CACHE_NO_LEGACY_MARKER).is_file(),
                "clear did not disable legacy migration"
            );
            assert!(
                fetch_score_source_cached(
                    &admitted,
                    url,
                    &access,
                    ScoreCacheKind::Audio,
                    64,
                    |bytes| Ok(bytes.to_vec()),
                )
                .is_err(),
                "clear silently reimported a legacy entry"
            );
            assert!(
                !admitted
                    .path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant)
                    .exists()
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[cfg(unix)]
        #[test]
        fn score_cache_control_paths_refuse_symlinks() {
            use std::os::unix::fs::symlink;

            let dir = std::env::temp_dir().join(format!(
                "rustel-cache-control-symlinks-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("cache root");
            let victim = dir.join("victim");
            std::fs::write(&victim, b"untouched").expect("victim");

            let lock = score_cache_lock_path(&dir);
            symlink(&victim, &lock).expect("lock symlink");
            let cache = ScoreCache::with_limits(dir.clone(), quota_limits(64, 4, 64, 4));
            let target = cache.path(
                "https://samples.example/kick.wav",
                ScoreCacheKind::Audio,
                ScoreCacheTrust::Grant,
            );
            let error = cache
                .admit(&target, b"sample")
                .expect_err("a symlink lock must be refused");
            assert!(error.contains("lock is not a regular file"), "{error}");
            assert_eq!(
                std::fs::read(&victim).expect("victim remains"),
                b"untouched"
            );
            std::fs::remove_file(&lock).expect("remove lock symlink");

            std::fs::create_dir_all(cache.dir()).expect("score namespace");
            std::fs::write(cache.dir().join("existing"), b"cache").expect("score entry");
            let marker = dir.join(SCORE_CACHE_NO_LEGACY_MARKER);
            symlink(&victim, &marker).expect("marker symlink");
            let error = clear_score_sample_cache_at(&dir)
                .expect_err("a symlink marker must be refused before clearing");
            assert!(error.contains("marker") && error.contains("not a regular file"));
            assert!(cache.dir().is_dir(), "clear ran after refusing its marker");
            assert_eq!(
                std::fs::read(&victim).expect("victim remains"),
                b"untouched"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[cfg(unix)]
        #[test]
        fn score_cache_open_refuses_a_symlink_swap() {
            use std::os::unix::fs::symlink;

            let dir = test_cache_base("cache-entry-symlink-swap");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("cache root");
            let victim = dir.join("victim");
            std::fs::write(&victim, b"secret").expect("victim");
            let cache = ScoreCache::with_limits(dir.clone(), quota_limits(64, 4, 64, 4));
            let path = cache.path(
                "https://samples.example/kick.wav",
                ScoreCacheKind::Audio,
                ScoreCacheTrust::Grant,
            );
            cache.admit(&path, b"sample").expect("admit");
            std::fs::remove_file(&path).expect("remove cache file");
            symlink(&victim, &path).expect("plant symlink");
            let error = cache
                .open(&path)
                .expect_err("a swapped symlink must not be followed");
            assert!(error.contains("not a regular file"), "{error}");
            assert_eq!(std::fs::read(&victim).expect("victim remains"), b"secret");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// The identity a cache entry is keyed by, pinned as a matrix.
        ///
        /// Each row here is a defect that existed: the first two shared a file
        /// when they are different resources, and the next two failed to share
        /// one when they are the same resource.
        #[test]
        fn score_cache_identity_survives_product_renames() {
            let path = score_cache_path(
                Path::new("/cache"),
                "https://samples.example/kick.wav",
                ScoreCacheKind::Audio,
                ScoreCacheTrust::Grant,
            );
            assert_eq!(
                path.file_name().and_then(|name| name.to_str()),
                Some("f51176c6893fc2b1d0ea1ea37a6b171b.wav")
            );
            let cors = score_cache_path(
                Path::new("/cache"),
                "https://samples.example/kick.wav",
                ScoreCacheKind::Audio,
                ScoreCacheTrust::Cors,
            );
            assert_ne!(
                path, cors,
                "CORS-consented and grant-only bytes must not share a file"
            );
        }

        #[test]
        fn cors_required_fetches_do_not_reuse_grant_only_cache_bytes() {
            let dir = std::env::temp_dir()
                .join(format!("rustel-cache-cors-trust-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(&dir);
            let cache = ScoreCache::new(dir.clone());
            let url = "https://samples.example/kick.wav";
            let grant_path = cache.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant);
            std::fs::create_dir_all(grant_path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&grant_path, b"grant-only bytes").expect("seed grant entry");

            let cors_access = ScoreFetchAccess::Remote {
                origin: "https://samples.example".into(),
                cors_required: true,
            };
            // No server: a cors_required hit must not return the grant-only file.
            // The fetch fails on the wire instead of serving unconsented bytes.
            let error = fetch_score_source_cached(
                &cache,
                url,
                &cors_access,
                ScoreCacheKind::Audio,
                1024,
                |bytes| Ok(bytes.to_vec()),
            )
            .expect_err("cors policy must not read grant-only cache");
            assert!(
                !error.is_empty(),
                "refusal should name the failed fetch, got {error:?}"
            );
            assert!(
                !cache
                    .path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Cors)
                    .exists(),
                "a failed cors fetch must not invent a cors entry"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn cache_identity_follows_the_url_not_the_text() {
            let dir = Path::new("/cache");
            let same = |a: &str, b: &str| cache_path(dir, a) == cache_path(dir, b);

            // Distinct resources must not collide. A slash inside a query used to
            // satisfy the old trailing-slash test, which searched the whole URL.
            assert!(!same(
                "https://a.example/kick.wav?q=///",
                "https://a.example/kick.wav?q=//"
            ));
            assert!(!same("https://a.example/a", "https://a.example/a/"));
            assert!(!same(
                "https://a.example/kick.wav",
                "https://a.example/kick.wav?v=2"
            ));
            assert!(!same(
                "https://a.example/kick#2.wav",
                "https://a.example/kick.wav"
            ));

            // The same resource must share one entry, dotted host or not. The
            // extension used to come from the raw string, so a dotted HOST
            // supplied one for the bare form and not the slashed form.
            assert!(same("https://example.com", "https://example.com/"));
            assert!(same("http://localhost:5432", "http://localhost:5432/"));
            assert!(same(
                "https://a.example/kick#2.wav",
                "https://a.example/kick%232.wav"
            ));

            // The extension is for humans reading the cache directory; it comes
            // from the path's last segment and nowhere else.
            let named = cache_path(dir, "https://a.example/kits/kick.wav");
            assert_eq!(named.extension().and_then(|ext| ext.to_str()), Some("wav"));
            assert_eq!(
                cache_path(dir, "https://example.com/").extension(),
                None,
                "a dotted host must not masquerade as a file extension"
            );
            // A query does not change what the file is.
            assert_eq!(
                cache_path(dir, "https://a.example/kick.wav?v=2")
                    .extension()
                    .and_then(|ext| ext.to_str()),
                Some("wav")
            );
        }

        /// The grant decides before the cache does.
        ///
        /// Checking the cache first and the policy second would turn the cache
        /// into a way around the policy: a score with no grant, or a grant for
        /// somewhere else, could read whatever a granted score had left behind.
        #[test]
        fn a_denied_score_cannot_read_a_seeded_cache_entry() {
            let dir =
                std::env::temp_dir().join(format!("rustel-cache-deny-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let cache = ScoreCache::new(dir.clone());
            let url = "http://granted.example/kick.wav";
            std::fs::create_dir_all(cache.dir()).expect("score cache");
            std::fs::write(
                cache.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant),
                b"seeded bytes",
            )
            .expect("seed the cache");

            // A grant for a DIFFERENT origin must not reach this entry, and must
            // not fall through to the network either.
            let elsewhere = ScoreFetchAccess::Remote {
                origin: "http://other.example".to_owned(),
                cors_required: false,
            };
            let error = fetch_score_source_cached(
                &cache,
                url,
                &elsewhere,
                ScoreCacheKind::Audio,
                1024,
                |b| Ok(b.to_vec()),
            )
            .expect_err("a grant elsewhere must not read this entry");
            assert!(error.contains("outside its permitted origin"), "{error}");

            // The matching grant does read it, which is what proves the entry was
            // reachable all along and policy is what withheld it.
            let granted = ScoreFetchAccess::Remote {
                origin: "http://granted.example".to_owned(),
                cors_required: false,
            };
            assert_eq!(
                fetch_score_source_cached(
                    &cache,
                    url,
                    &granted,
                    ScoreCacheKind::Audio,
                    1024,
                    |b| Ok(b.to_vec()),
                )
                .expect("granted read"),
                b"seeded bytes",
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A hit serves the bytes whether or not the server is reachable. This is
        /// the property offline rendering depends on: the corpus fetches its
        /// samples once and every later comparison runs with nothing listening.
        #[test]
        fn a_granted_url_is_served_from_cache_with_no_server_running() {
            let dir = std::env::temp_dir().join(format!("rustel-cache-hit-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let cache = ScoreCache::new(dir.clone());
            // Port 9 is the discard port: nothing accepts there, so a fetch that
            // reached the network at all would fail rather than pass quietly.
            let url = "http://127.0.0.1:9/kick.wav";
            std::fs::create_dir_all(cache.dir()).expect("score cache");
            std::fs::write(
                cache.path(url, ScoreCacheKind::Audio, ScoreCacheTrust::Grant),
                b"cached audio",
            )
            .expect("seed");
            let access = ScoreFetchAccess::Remote {
                origin: "http://127.0.0.1:9".to_owned(),
                cors_required: false,
            };
            assert_eq!(
                fetch_score_source_cached(&cache, url, &access, ScoreCacheKind::Audio, 1024, |b| {
                    Ok(b.to_vec())
                },)
                .expect("served from cache"),
                b"cached audio",
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A miss fetches once and writes, so the NEXT run needs no server. Both
        /// halves are asserted here: the server is taken down between them.
        #[test]
        fn a_cache_miss_populates_for_the_next_offline_run() {
            use std::io::Write;
            use std::net::TcpListener;

            let dir =
                std::env::temp_dir().join(format!("rustel-cache-fill-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let cache = ScoreCache::new(dir.clone());
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let url = format!("{origin}/kick.wav");
            let body = "fetched once";
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("request");
                // Consume the request head before answering. Writing and closing
                // while the client is still sending resets the connection, which
                // surfaces as a network error rather than the response.
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                loop {
                    line.clear();
                    match std::io::BufRead::read_line(&mut reader, &mut line) {
                        Ok(0) => break,
                        Ok(_) if line == "\r\n" || line == "\n" => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response");
                let _ = stream.flush();
            });

            let access = ScoreFetchAccess::Remote {
                origin: origin.clone(),
                cors_required: false,
            };
            let first = fetch_score_source_cached(
                &cache,
                &url,
                &access,
                ScoreCacheKind::Audio,
                1024,
                |b| Ok(b.to_vec()),
            )
            .expect("first fetch");
            server.join().expect("server");
            assert_eq!(first, b"fetched once");

            // The listener is closed now. A second read can only come from disk.
            let second = fetch_score_source_cached(
                &cache,
                &url,
                &access,
                ScoreCacheKind::Audio,
                1024,
                |b| Ok(b.to_vec()),
            )
            .expect("second read must not need the server");
            assert_eq!(second, b"fetched once");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// An origin grant is not an address grant. A granted name that resolves
        /// somewhere private is still refused, at the resolver, on every hop.
        #[test]
        fn a_granted_origin_pointing_somewhere_private_is_still_refused() {
            let dir =
                std::env::temp_dir().join(format!("rustel-cache-priv-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let cache = ScoreCache::new(dir.clone());
            for target in [
                "http://10.255.0.7/kick.wav",
                "http://169.254.169.254/latest/meta-data",
            ] {
                let parsed = Url::parse(target).expect("test url");
                let access = ScoreFetchAccess::Remote {
                    origin: parsed.origin().ascii_serialization(),
                    cors_required: false,
                };
                let error = fetch_score_source_cached(
                    &cache,
                    target,
                    &access,
                    ScoreCacheKind::Audio,
                    1024,
                    |b| Ok(b.to_vec()),
                )
                .expect_err("a private target must be refused even when its origin is granted");
                assert!(
                    error.contains("not a public address"),
                    "{target}: refusal must name the address guard: {error}"
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn fetched_maps_cannot_expand_their_origin_grant() {
            use std::io::Write;
            use std::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").expect("map listener");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let map_url = format!("{origin}/strudel.json");
            let body = r#"{"escape":"http://127.0.0.1:9/private.wav"}"#;
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("map request");
                drain_request_head(&stream);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("map response");
            });

            let mut access = ScoreSampleAccess::denied();
            access.permit_origin(&origin).expect("map origin");
            let library = empty_library();
            let source = serde_json::to_string(&map_url).expect("source string");
            let error = library
                .register_score_custom(&source, None, &access)
                .expect_err("a fetched map must not introduce another origin");
            server.join().expect("map server");
            assert!(
                error.contains("outside the permitted sample origins"),
                "{error}"
            );
            assert!(library.custom.read().expect("custom banks").is_empty());
        }

        #[test]
        fn score_selected_http_redirects_cannot_leave_the_granted_origin() {
            use std::io::Write;
            use std::net::TcpListener;

            let destination = TcpListener::bind("127.0.0.1:0").expect("destination listener");
            destination
                .set_nonblocking(true)
                .expect("nonblocking destination");
            let destination_url = format!("http://{}/payload", destination.local_addr().unwrap());
            let redirect = TcpListener::bind("127.0.0.1:0").expect("redirect listener");
            let redirect_url = format!("http://{}/map", redirect.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = redirect.accept().expect("redirect request");
                drain_request_head(&stream);
                write!(
                    stream,
                    "HTTP/1.1 302 Found\r\nLocation: {destination_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .expect("redirect response");
            });
            let parsed = Url::parse(&redirect_url).expect("redirect URL");
            let access = ScoreFetchAccess::Remote {
                origin: parsed.origin().ascii_serialization(),
                cors_required: false,
            };
            let error = fetch_score_source(&redirect_url, &access, 1024)
                .expect_err("score fetch must stop at a cross-origin redirect");
            server.join().expect("redirect server");
            assert!(
                error.contains("outside the permitted sample origin"),
                "{error}"
            );
            assert!(
                matches!(destination.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                "the redirect target received a request"
            );
        }

        #[test]
        fn same_origin_sample_redirects_remain_compatible() {
            use std::io::Write;
            use std::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").expect("sample listener");
            let url = format!("http://{}/redirect", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut first, _) = listener.accept().expect("redirect request");
                drain_request_head(&first);
                write!(
                    first,
                    "HTTP/1.1 302 Found\r\nLocation: /payload\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .expect("redirect response");
                let (mut second, _) = listener.accept().expect("payload request");
                drain_request_head(&second);
                write!(
                    second,
                    "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nRIFF"
                )
                .expect("payload response");
            });
            let parsed = Url::parse(&url).expect("sample URL");
            let access = ScoreFetchAccess::Remote {
                origin: parsed.origin().ascii_serialization(),
                cors_required: false,
            };
            let bytes = fetch_score_source(&url, &access, 1024).expect("same-origin redirect");
            server.join().expect("sample server");
            assert_eq!(bytes, b"RIFF");
        }

        #[cfg(unix)]
        #[test]
        fn local_score_samples_are_confined_to_the_granted_root() {
            let root =
                std::env::temp_dir().join(format!("rustel-score-samples-{}", std::process::id()));
            let outside = root.with_extension("outside.wav");
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_file(&outside);
            std::fs::create_dir_all(root.join("kit")).expect("sample folder");
            std::fs::write(root.join("kit/kick.wav"), b"RIFF").expect("sample");
            std::fs::write(&outside, b"RIFF private").expect("outside file");
            std::os::unix::fs::symlink(&outside, root.join("kit/escape.wav")).expect("symlink");

            let mut access = ScoreSampleAccess::denied();
            access.permit_local_root(&root).expect("local root");
            let library = empty_library();
            library
                .register_score_custom(r#""local:kit""#, None, &access)
                .expect("local kit");
            let custom = library.custom.read().expect("custom banks");
            let Bank::Array(urls) = custom.get("kit").expect("kit bank") else {
                panic!("kit was not an array bank");
            };
            assert_eq!(urls.len(), 1, "a symlink escaped the granted root");
            assert!(urls[0].ends_with("/kit/kick.wav"));
            drop(custom);

            for source in [
                r#""local:..""#,
                r#""local:kit/../../password.txt""#,
                r#""local:/tmp""#,
            ] {
                assert!(
                    library
                        .register_score_custom(source, None, &access)
                        .is_err(),
                    "{source} escaped the granted root"
                );
            }

            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_file(&outside);
        }

        #[cfg(unix)]
        #[test]
        fn score_local_read_refuses_a_sample_swapped_to_an_escape() {
            use std::os::unix::fs::symlink;

            let root = test_cache_base("score-local-symlink-swap");
            let outside = root.with_extension("outside.wav");
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_file(&outside);
            std::fs::create_dir_all(root.join("kit")).expect("sample folder");
            let target = root.join("kit/kick.wav");
            let original = root.join("kit/original.wav");
            std::fs::write(&target, b"RIFF inside").expect("inside sample");
            std::fs::write(&outside, b"RIFF outside private").expect("outside sample");

            let mut grant = ScoreSampleAccess::denied();
            grant.permit_local_root(&root).expect("local root");
            let access = ScoreFetchAccess::Local {
                root: Arc::clone(grant.local_root.as_ref().expect("granted root")),
            };
            let url = Url::from_file_path(&target)
                .expect("sample file URL")
                .to_string();
            let error = fetch_score_source_with_budget(
                &url,
                &access,
                1024,
                &sample_fetch::FetchBudget::for_one_fetch(),
                || {
                    std::fs::rename(&target, &original).expect("move original sample");
                    symlink(&outside, &target).expect("install escaping symlink");
                },
            )
            .expect_err("an escaping replacement must be refused");
            assert!(
                // Which guard fires is the platform's business (macOS's upfront
                // containment check speaks first; Linux's open does) - the
                // contract is the refusal, always in the root's name.
                error.contains("outside the permitted root")
                    || error.contains("unavailable beneath the permitted root"),
                "{error}"
            );

            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_file(&outside);
        }

        #[cfg(unix)]
        #[test]
        fn score_local_read_refuses_a_fifo_swap_without_blocking() {
            use std::os::unix::ffi::OsStrExt;

            let root = test_cache_base("score-local-fifo-swap");
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("kit")).expect("sample folder");
            let target = root.join("kit/kick.wav");
            let original = root.join("kit/original.wav");
            std::fs::write(&target, b"RIFF inside").expect("inside sample");

            let mut grant = ScoreSampleAccess::denied();
            grant.permit_local_root(&root).expect("local root");
            let access = ScoreFetchAccess::Local {
                root: Arc::clone(grant.local_root.as_ref().expect("granted root")),
            };
            let url = Url::from_file_path(&target)
                .expect("sample file URL")
                .to_string();
            let started = std::time::Instant::now();
            let error = fetch_score_source_with_budget(
                &url,
                &access,
                1024,
                &sample_fetch::FetchBudget::for_one_fetch(),
                || {
                    std::fs::rename(&target, &original).expect("move original sample");
                    let path = std::ffi::CString::new(target.as_os_str().as_bytes())
                        .expect("fixture path without NUL");
                    // SAFETY: `path` is NUL-terminated and names a new fixture FIFO.
                    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
                    assert_eq!(
                        result,
                        0,
                        "create replacement FIFO: {}",
                        std::io::Error::last_os_error()
                    );
                },
            )
            .expect_err("a replacement FIFO must be refused");
            assert!(
                // Which guard fires is the platform's business (macOS's upfront
                // containment check speaks first; Linux's open does) - the
                // contract is the refusal, always in the root's name.
                error.contains("outside the permitted root")
                    || error.contains("unavailable beneath the permitted root"),
                "{error}"
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(1),
                "opening the replacement FIFO blocked"
            );

            let _ = std::fs::remove_dir_all(&root);
        }

        #[cfg(windows)]
        #[test]
        fn local_score_samples_are_confined_to_the_granted_root_on_windows() {
            let root = std::env::temp_dir().join(format!(
                "rustel-score-samples-windows-{}",
                std::process::id()
            ));
            let outside = root.with_extension("outside.wav");
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_file(&outside);
            std::fs::create_dir_all(root.join("kit")).expect("sample folder");
            std::fs::write(root.join("kit/kick.wav"), b"RIFF").expect("sample");
            std::fs::write(&outside, b"RIFF private").expect("outside file");

            let mut access = ScoreSampleAccess::denied();
            access.permit_local_root(&root).expect("local root");
            let library = empty_library();
            library
                .register_score_custom(r#""local:kit""#, None, &access)
                .expect("local kit");
            let custom = library.custom.read().expect("custom banks");
            let Bank::Array(urls) = custom.get("kit").expect("kit bank") else {
                panic!("kit was not an array bank");
            };
            assert_eq!(urls.len(), 1, "the kit registered files it does not hold");
            assert!(urls[0].ends_with("/kit/kick.wav"), "{}", urls[0]);
            let url = urls[0].to_string();
            drop(custom);

            for source in [
                r#""local:..""#,
                r#""local:kit/../../outside.wav""#,
                r#""local:/Windows""#,
                r#""local:C:/Windows""#,
            ] {
                assert!(
                    library
                        .register_score_custom(source, None, &access)
                        .is_err(),
                    "{source} escaped the granted root"
                );
            }

            // The Windows regression proper: registration spelled the canonical
            // verbatim path into the URL and playback gets a non-verbatim path
            // back from `to_file_path`. The registered URL must still load.
            let bytes = fetch_score_source(
                &url,
                &ScoreFetchAccess::Local {
                    root: Arc::clone(access.local_root.as_ref().expect("granted root")),
                },
                1024,
            )
            .expect("a registered local sample must load on Windows");
            assert_eq!(bytes, b"RIFF");

            // A URL naming the file beside the root must not.
            let outside_url =
                Url::from_file_path(outside.canonicalize().expect("canonical outside"))
                    .expect("outside URL")
                    .to_string();
            let error = fetch_score_source(
                &outside_url,
                &ScoreFetchAccess::Local {
                    root: Arc::clone(access.local_root.as_ref().expect("granted root")),
                },
                1024,
            )
            .expect_err("a file beside the granted root must be refused");
            assert!(error.contains("outside the permitted root"), "{error}");

            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_file(&outside);
        }
    }
}
