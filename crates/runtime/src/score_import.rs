//! Script imports: `await import('https://...')` in a score or a prebake.
//!
//! The host fetches the script and the score realm runs it as setup code
//! before the text that imports it. The realm itself opens no connection.
//! Nothing loads by default: the host trusts each origin by name.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use url::Url;

use crate::sample_fetch;

/// The largest script that an import fetches, in bytes.
const MAX_IMPORT_BYTES: usize = 1024 * 1024;

/// How long the fetch of one script takes at most. The evaluation waits for
/// it, so a live update is late by this time on a host that does not answer.
const IMPORT_FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// The origins a score imports scripts from. The default trusts none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScoreImportAccess {
    origins: Vec<String>,
}

/// Why an import does not load.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ImportRefusal {
    /// The host does not trust this origin.
    Untrusted {
        origin: String,
    },
    Invalid(String),
}

impl ScoreImportAccess {
    pub fn denied() -> Self {
        Self::default()
    }

    /// Trust the scripts of one exact origin, such as `https://glossing.dev`.
    ///
    /// The origin has no credentials, path, query or fragment. It uses
    /// https. Plain http is for an origin that names loopback, so a script
    /// can come from a server on this machine.
    pub fn permit_origin(&mut self, origin: &str) -> Result<(), String> {
        let origin = import_origin(origin)?;
        if !self.origins.contains(&origin) {
            self.origins.push(origin);
        }
        Ok(())
    }

    /// The trusted origins, in the order they were added.
    pub fn origins(&self) -> &[String] {
        &self.origins
    }

    pub(crate) fn approve(&self, specifier: &str) -> Result<Url, ImportRefusal> {
        let url = sample_fetch::web_address(specifier)
            .map_err(|error| ImportRefusal::Invalid(format!("import URL {error}")))?;
        check_transport(&url).map_err(ImportRefusal::Invalid)?;
        let origin = url.origin().ascii_serialization();
        if self.origins.contains(&origin) {
            Ok(url)
        } else {
            Err(ImportRefusal::Untrusted { origin })
        }
    }
}

/// The canonical form of an origin that [`ScoreImportAccess::permit_origin`]
/// accepts.
pub fn import_origin(origin: &str) -> Result<String, String> {
    let parsed = Url::parse(origin).map_err(|error| format!("invalid import origin: {error}"))?;
    if parsed.host_str().is_none() {
        return Err("import origin must be an https origin".to_owned());
    }
    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("import origin must not contain a path, query, or fragment".to_owned());
    }
    check_transport(&parsed).map_err(|error| error.replace("import URL", "import origin"))?;
    Ok(parsed.origin().ascii_serialization())
}

fn check_transport(url: &Url) -> Result<(), String> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err("import URL must not contain credentials".to_owned());
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if sample_fetch::url_names_loopback(url) => Ok(()),
        _ => Err("import URL must use https".to_owned()),
    }
}

/// Fetch the text of an approved script. Every redirect stays on the origin
/// of `url`, and every connection passes the address rules of the sample
/// client.
pub(crate) fn fetch(url: &Url) -> Result<String, String> {
    use std::io::Read;

    let budget = sample_fetch::FetchBudget::until(
        Instant::now() + IMPORT_FETCH_TIMEOUT,
        Arc::new(AtomicBool::new(false)),
    );
    let origin = url.origin();
    let timed_out = |_| format!("no answer in {} s", IMPORT_FETCH_TIMEOUT.as_secs());
    let mut current = url.clone();
    for _ in 0..=5 {
        budget.check().map_err(timed_out)?;
        let response = sample_fetch::guarded_single_hop_get(&current, &budget, None)?;
        if (300..400).contains(&response.status()) {
            let location = response
                .header("Location")
                .ok_or("the redirect has no Location header")?;
            let next = current
                .join(location)
                .map_err(|error| format!("invalid redirect target: {error}"))?;
            if next.origin() != origin || check_transport(&next).is_err() {
                return Err("the redirect leaves the trusted origin".to_owned());
            }
            current = next;
            continue;
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_IMPORT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read failed: {error}"))?;
        budget.check().map_err(timed_out)?;
        if bytes.len() > MAX_IMPORT_BYTES {
            return Err(format!(
                "the script is larger than {} KiB",
                MAX_IMPORT_BYTES / 1024
            ));
        }
        return String::from_utf8(bytes).map_err(|_| "the script is not UTF-8 text".to_owned());
    }
    Err("too many redirects".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_trusted_origin_is_approved() {
        let mut access = ScoreImportAccess::denied();
        assert_eq!(
            access.approve("https://glossing.dev/scripts.js"),
            Err(ImportRefusal::Untrusted {
                origin: "https://glossing.dev".to_owned()
            })
        );
        access.permit_origin("https://glossing.dev/").unwrap();
        assert!(access.approve("https://glossing.dev/scripts.js").is_ok());
        assert!(matches!(
            access.approve("https://other.example/scripts.js"),
            Err(ImportRefusal::Untrusted { .. })
        ));
        for refused in [
            "http://glossing.dev/scripts.js",
            "https://user:secret@glossing.dev/scripts.js",
            "file:///etc/passwd",
        ] {
            assert!(
                matches!(access.approve(refused), Err(ImportRefusal::Invalid(_))),
                "{refused}"
            );
        }
    }

    #[test]
    fn an_origin_is_https_or_names_loopback() {
        let mut access = ScoreImportAccess::denied();
        access.permit_origin("http://localhost:8000").unwrap();
        assert_eq!(access.origins(), ["http://localhost:8000"]);
        for refused in [
            "http://example.com",
            "https://example.com/scripts.js",
            "https://user@example.com",
            "example.com",
        ] {
            assert!(access.permit_origin(refused).is_err(), "{refused}");
        }
    }
}
