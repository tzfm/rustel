//! Optional release notices for interactive applications.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const CHECK_INTERVAL: u64 = 24 * 60 * 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CACHE_BYTES: u64 = 4096;
const MAX_RELEASE_BYTES: u64 = 1024 * 1024;
const RELEASE_URL: &str = "https://api.github.com/repos/tzfm/rustel/releases/latest";

/// Report a newer stable release at most once, using the cache or a background request.
/// Disabled settings and failures stay silent. The caller controls where notices appear.
pub fn check_for_updates(notify: impl FnOnce(String) + Send + 'static) {
    if std::env::var_os("RUSTEL_NO_UPDATE_CHECK").is_some_and(|value| !value.is_empty())
        || !crate::settings::check_updates().unwrap_or(false)
    {
        return;
    }
    let Some(directory) = crate::config_dir::canonical() else {
        return;
    };
    let Ok(current) = Version::parse(crate::product::VERSION) else {
        return;
    };
    let Some(now) = current_time() else {
        return;
    };
    let path = directory.join("cache/update-check.json");
    let cache = read_cache(&path).unwrap_or_default();
    let mut notify = Some(notify);
    notify_newer(&cache, &current, &mut notify);
    if !cache.needs_refresh(now) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("rustel-update-check".into())
        .spawn(move || {
            if let Some(cache) = refresh(&path, now, fetch_latest) {
                notify_newer(&cache, &current, &mut notify);
            }
        });
}

#[derive(Default, Deserialize, Serialize)]
struct Cache {
    checked_at: u64,
    latest_version: Option<String>,
}

impl Cache {
    fn needs_refresh(&self, now: u64) -> bool {
        self.checked_at == 0
            || now
                .checked_sub(self.checked_at)
                .is_none_or(|elapsed| elapsed >= CHECK_INTERVAL)
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

fn current_time() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn stable_version(text: &str) -> Option<Version> {
    if text.len() > 128 {
        return None;
    }
    let version = Version::parse(text.strip_prefix('v').unwrap_or(text)).ok()?;
    version.pre.is_empty().then_some(version)
}

fn notify_newer<F: FnOnce(String)>(cache: &Cache, current: &Version, notify: &mut Option<F>) {
    let Some(latest) = cache.latest_version.as_deref().and_then(stable_version) else {
        return;
    };
    if latest.cmp_precedence(current).is_gt()
        && let Some(notify) = notify.take()
    {
        let command = if cfg!(windows) {
            "rustelup.cmd"
        } else {
            "rustelup"
        };
        notify(format!(
            "Rustel {latest} is available. Run {command} to update."
        ));
    }
}

fn read_json<T: DeserializeOwned>(reader: impl Read, limit: u64) -> Option<T> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn read_cache(path: &Path) -> Option<Cache> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CACHE_BYTES {
        return None;
    }
    read_json(file, MAX_CACHE_BYTES)
}

fn write_cache(path: &Path, cache: &Cache) -> io::Result<()> {
    let bytes = serde_json::to_vec(cache).map_err(io::Error::other)?;
    crate::atomic_file::replace_file(path, ".update-check-", &bytes, |pending, target| {
        std::fs::rename(pending, target)
    })
}

fn refresh(path: &Path, now: u64, fetch: impl FnOnce() -> Option<Version>) -> Option<Cache> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .ok()?;
    if !lock.metadata().ok()?.is_file() {
        return None;
    }
    lock.try_lock().ok()?;
    let mut cache = read_cache(path).unwrap_or_default();
    if !cache.needs_refresh(now) {
        return Some(cache);
    }
    // Check cache writes before fetching. Interrupted requests leave the attempt due.
    write_cache(path, &cache).ok()?;
    if let Some(latest) = fetch() {
        cache.latest_version = Some(latest.to_string());
    }
    cache.checked_at = now;
    let _ = write_cache(path, &cache);
    Some(cache)
}

fn fetch_latest() -> Option<Version> {
    let response = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(REQUEST_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent("rustel-update-check")
        .build()
        .get(RELEASE_URL)
        .set("Accept", "application/vnd.github+json")
        .call()
        .ok()?;
    if response.status() != 200 {
        return None;
    }
    release_version(response.into_reader())
}

fn release_version(reader: impl Read) -> Option<Version> {
    let release: Release = read_json(reader, MAX_RELEASE_BYTES)?;
    if release.draft || release.prerelease {
        return None;
    }
    stable_version(&release.tag_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_use_semantic_precedence_and_fire_once() {
        let mut messages = Vec::new();
        let current = Version::parse("1.9.0+local").unwrap();
        let mut notify = Some(|message| messages.push(message));
        for version in ["1.8.0", "1.9.0+other", "1.10.0-rc.1", "1.10.0", "2.0.0"] {
            let cache = Cache {
                checked_at: 1,
                latest_version: Some(version.into()),
            };
            notify_newer(&cache, &current, &mut notify);
        }
        assert!(notify.is_none());
        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with("Rustel 1.10.0 is available. Run rustelup"));
    }

    #[test]
    fn release_metadata_must_describe_a_stable_version() {
        for tag in ["v1.2.3", "1.2.3"] {
            let json = format!(r#"{{"tag_name":"{tag}","draft":false,"prerelease":false}}"#);
            assert_eq!(
                release_version(json.as_bytes()),
                Some(Version::parse("1.2.3").unwrap())
            );
        }
        for json in [
            r#"{"tag_name":"v1.2.3","draft":true,"prerelease":false}"#,
            r#"{"tag_name":"v1.2.3","draft":false,"prerelease":true}"#,
            r#"{"tag_name":"v1.2.3-rc.1","draft":false,"prerelease":false}"#,
            r#"{"tag_name":"latest","draft":false,"prerelease":false}"#,
            r#"{"tag_name":"1.2.3\nrun something","draft":false,"prerelease":false}"#,
            r#"{"tag_name":"v1.2.3"}"#,
            "{",
        ] {
            assert!(release_version(json.as_bytes()).is_none(), "{json}");
        }
        assert!(stable_version(&format!("1.2.3+{}", "a".repeat(128))).is_none());
    }

    #[test]
    fn successful_and_failed_attempts_share_the_daily_interval() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache/update-check.json");
        let now = 100_000;
        let fetched = refresh(&path, now, || Some(Version::parse("1.2.3").unwrap())).unwrap();
        assert_eq!(fetched.latest_version.as_deref(), Some("1.2.3"));
        let cached = refresh(&path, now + CHECK_INTERVAL - 1, || panic!("too soon")).unwrap();
        assert_eq!(cached.checked_at, now);
        let failed = refresh(&path, now + CHECK_INTERVAL, || None).unwrap();
        assert_eq!(failed.checked_at, now + CHECK_INTERVAL);
        assert_eq!(failed.latest_version.as_deref(), Some("1.2.3"));
        refresh(&path, now + CHECK_INTERVAL + 1, || {
            panic!("retried failure")
        })
        .unwrap();
        let saved = read_cache(&path).unwrap();
        assert_eq!(saved.checked_at, failed.checked_at);
        assert_eq!(saved.latest_version, failed.latest_version);
    }

    #[test]
    fn the_first_failed_attempt_is_cached_without_a_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update-check.json");
        let failed = refresh(&path, 100_000, || None).unwrap();
        assert!(failed.latest_version.is_none());
        refresh(&path, 100_001, || panic!("retried failure")).unwrap();
    }

    #[test]
    fn interrupted_requests_leave_the_next_attempt_due() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update-check.json");
        assert!(
            std::panic::catch_unwind(|| {
                refresh(&path, 100_000, || panic!("interrupted request"));
            })
            .is_err()
        );
        assert_eq!(read_cache(&path).unwrap().checked_at, 0);
        let cache = refresh(&path, 100_001, || Some(Version::parse("1.2.3").unwrap())).unwrap();
        assert_eq!(cache.checked_at, 100_001);
        assert_eq!(cache.latest_version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn future_timestamps_allow_a_new_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update-check.json");
        write_cache(
            &path,
            &Cache {
                checked_at: 200_000,
                latest_version: None,
            },
        )
        .unwrap();
        let cache = refresh(&path, 100_000, || Some(Version::parse("1.2.3").unwrap())).unwrap();
        assert_eq!(cache.checked_at, 100_000);
        assert_eq!(cache.latest_version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn simultaneous_checks_do_not_wait_or_fetch_twice() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update-check.json");
        refresh(&path, 100_000, || {
            assert!(refresh(&path, 200_000, || panic!("second fetch")).is_none());
            None
        })
        .unwrap();
        assert!(refresh(&path, 200_000, || None).is_some());
    }

    #[test]
    fn unreadable_cache_files_do_not_start_a_request() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update-check.json");
        std::fs::create_dir(&path).unwrap();
        assert!(read_cache(&path).is_none());
        assert!(refresh(&path, 100_000, || panic!("cache cannot be written")).is_none());
    }

    #[test]
    fn cache_and_release_reads_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update-check.json");
        for bytes in [b"{".to_vec(), vec![b' '; MAX_CACHE_BYTES as usize + 1]] {
            std::fs::write(&path, bytes).unwrap();
            assert!(read_cache(&path).is_none());
        }
        let oversized = io::repeat(b' ').take(MAX_RELEASE_BYTES + 1);
        assert!(release_version(oversized).is_none());
    }
}
