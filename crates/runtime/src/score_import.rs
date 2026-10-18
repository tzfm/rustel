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
    local_root: Option<Arc<crate::sample_server::ServedRoot>>,
    csound: bool,
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

    /// Allow native orchestra code, including its filesystem and process opcodes.
    pub fn permit_csound(&mut self) {
        self.set_csound(true);
    }

    /// Turn native orchestra permission on or off. The studio's permissions
    /// page flips it during a run; the launch flag only turns it on.
    pub fn set_csound(&mut self, allowed: bool) {
        self.csound = allowed;
    }

    pub fn csound_allowed(&self) -> bool {
        self.csound
    }

    /// Trust the scripts of one exact origin, such as `https://glossing.dev`.
    ///
    /// The origin has no credentials, path, query or fragment. It uses
    /// https. Only an origin that names loopback accepts http, so a script
    /// comes from a server on this machine.
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

    /// Trust local orchestra files beneath this directory. Symlinks cannot
    /// leave the directory, and sample access grants do not imply this grant.
    pub fn permit_local_root(&mut self, root: impl AsRef<std::path::Path>) -> Result<(), String> {
        self.local_root = Some(Arc::new(crate::sample_server::ServedRoot::open(
            root.as_ref(),
        )?));
        Ok(())
    }

    pub(crate) fn local_orchestra(&self, specifier: &str) -> Result<String, String> {
        use std::io::Read;

        let specifier = specifier.strip_prefix("local:").unwrap_or(specifier);
        let root = self
            .local_root
            .as_ref()
            .ok_or("local orchestra access is not enabled")?;
        let path = if specifier.starts_with("file:") {
            Url::parse(specifier)
                .ok()
                .and_then(|url| url.to_file_path().ok())
                .ok_or("invalid local orchestra URL")?
        } else {
            std::path::PathBuf::from(specifier)
        };
        let path = if path.is_absolute() {
            path
        } else {
            // The root holds a verbatim path on Windows, and joining onto
            // one resolves `..` against the root itself: `nested/../x`
            // would reach the root's own `x`, and more `..` climbs out of
            // the directory. Refuse the traversal while the specifier
            // still reads as plain components.
            if path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(
                    "local orchestra is outside the trusted directory or cannot be read".to_owned(),
                );
            }
            root.path().join(path)
        };
        let (file, _) = root
            .open_score_file(&path)
            .map_err(|_| "local orchestra is outside the trusted directory or cannot be read")?;
        let mut bytes = Vec::new();
        file.take(MAX_IMPORT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read local orchestra")?;
        if bytes.len() > MAX_IMPORT_BYTES {
            return Err("local orchestra is larger than 1024 KiB".into());
        }
        String::from_utf8(bytes).map_err(|_| "local orchestra is not UTF-8 text".into())
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
    fn orchestra_files_need_a_separate_confined_grant() {
        let root = tempfile::tempdir().expect("orchestra directory");
        let source = "instr probe\nendin";
        std::fs::write(root.path().join("probe.orc"), source).expect("orchestra");
        let mut access = ScoreImportAccess::denied();
        assert!(access.local_orchestra("local:probe.orc").is_err());
        access.permit_local_root(root.path()).expect("grant");
        assert!(!access.csound_allowed());
        assert_eq!(access.local_orchestra("local:probe.orc").unwrap(), source);
        assert_eq!(access.local_orchestra("probe.orc").unwrap(), source);
        let absolute = root.path().join("probe.orc");
        assert_eq!(
            access.local_orchestra(absolute.to_str().unwrap()).unwrap(),
            source
        );
        assert_eq!(
            access
                .local_orchestra(Url::from_file_path(&absolute).unwrap().as_str())
                .unwrap(),
            source
        );
        assert!(access.local_orchestra("../outside.orc").is_err());
        assert!(access.local_orchestra("nested/../probe.orc").is_err());
        std::fs::create_dir(root.path().join("directory.orc")).expect("directory");
        assert!(access.local_orchestra("directory.orc").is_err());
        let outside = tempfile::NamedTempFile::new().expect("outside file");
        assert!(
            access
                .local_orchestra(outside.path().to_str().unwrap())
                .is_err()
        );
        assert!(
            access
                .local_orchestra(Url::from_file_path(outside.path()).unwrap().as_str())
                .is_err()
        );
        std::fs::write(root.path().join("binary.orc"), [0xff]).expect("binary");
        assert!(
            access
                .local_orchestra("binary.orc")
                .unwrap_err()
                .contains("UTF-8")
        );
        std::fs::write(
            root.path().join("large.orc"),
            vec![b'a'; MAX_IMPORT_BYTES + 1],
        )
        .expect("large orchestra");
        assert!(
            access
                .local_orchestra("large.orc")
                .unwrap_err()
                .contains("larger than")
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("escape.orc"))
                .expect("symlink");
            assert!(access.local_orchestra("escape.orc").is_err());
            std::fs::write(root.path().join(".hidden.orc"), source).expect("hidden file");
            std::os::unix::fs::symlink(".hidden.orc", root.path().join("hidden.orc"))
                .expect("hidden symlink");
            assert!(access.local_orchestra("hidden.orc").is_err());
        }
    }

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

    /// A loopback server that answers each request with `reply(path)`.
    fn serve(reply: fn(&str) -> Vec<u8>) -> String {
        use std::io::{BufRead, BufReader, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut header = String::new();
                while reader.read_line(&mut header).is_ok_and(|read| read > 2) {
                    header.clear();
                }
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                (&stream).write_all(&reply(path)).ok();
            }
        });
        origin
    }

    #[test]
    fn a_fetch_follows_redirects_on_its_origin_and_caps_the_size() {
        fn reply(path: &str) -> Vec<u8> {
            let response = |status: &str, headers: &str, body: &[u8]| {
                let head = format!(
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                [head.as_bytes(), body].concat()
            };
            match path {
                "/script.js" => response("200 OK", "", b"globalThis.a = 1"),
                "/same" => response("302 Found", "Location: /script.js\r\n", b""),
                "/away" => response("302 Found", "Location: http://localhost:1/x.js\r\n", b""),
                _ => response("200 OK", "", &vec![b'a'; MAX_IMPORT_BYTES + 1]),
            }
        }
        let origin = serve(reply);
        let get = |path: &str| fetch(&Url::parse(&format!("{origin}{path}")).expect("url"));
        assert_eq!(get("/script.js").as_deref(), Ok("globalThis.a = 1"));
        assert_eq!(get("/same").as_deref(), Ok("globalThis.a = 1"));
        assert!(
            get("/away")
                .unwrap_err()
                .contains("leaves the trusted origin")
        );
        assert!(get("/big").unwrap_err().contains("larger than"));
    }
}
