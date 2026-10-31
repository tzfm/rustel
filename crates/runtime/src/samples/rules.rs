//! The sources a score may load samples, images and videos from.
//!
//! A source is a domain, such as `example.com`, or an address, such as
//! `https://example.com/kits/`. Nothing else loads: a server's own headers
//! never widen the list.

use url::{Host, Url};

/// The domains every policy starts with, unless the host turns them off.
/// They serve the `github:`, `bubo:` and `shabda:` shorthands and the
/// default banks.
pub const BUILTIN_SOURCES: &[&str] = &[
    "raw.githubusercontent.com",
    "strudel.b-cdn.net",
    "shabda.ndre.gr",
];

/// One allowed source.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SourceRule {
    /// `example.com`: HTTPS from this domain and its subdomains, on the
    /// default port, at any path.
    Domain(String),
    /// `https://example.com/kits/`: the addresses on this origin whose path
    /// is `path` or below it. A path of `/` is the whole origin. This is
    /// the only form that allows plain HTTP or a server on this computer.
    Address { origin: String, path: String },
}

impl SourceRule {
    /// Read a source as the player or a flag writes it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("write a domain, such as example.com, or an address".to_owned());
        }
        if text.contains("://") {
            return Self::address(text);
        }
        Self::domain(text)
    }

    fn domain(text: &str) -> Result<Self, String> {
        let full = || {
            format!(
                "{text} is not a domain. Write the full address instead, such as https://{text}/"
            )
        };
        if text.contains(['/', ':', '@', '?', '#']) || text.ends_with('.') {
            return Err(full());
        }
        let parsed = Url::parse(&format!("https://{text}/")).map_err(|_| full())?;
        let Some(Host::Domain(host)) = parsed.host() else {
            return Err(full());
        };
        // One label is a name on this network or this computer, never a
        // public domain. Such a server needs its address written out.
        if !host.contains('.') || crate::sample_fetch::url_names_loopback(&parsed) {
            return Err(full());
        }
        Ok(Self::Domain(host.to_owned()))
    }

    fn address(text: &str) -> Result<Self, String> {
        let parsed =
            Url::parse(text).map_err(|error| format!("{text} is not an address: {error}"))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(format!("{text} is not an http or https address"));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(format!("{text} must not hold a user name or password"));
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(format!("{text} must not hold a ? or # part"));
        }
        Ok(Self::Address {
            origin: parsed.origin().ascii_serialization(),
            path: parsed.path().to_owned(),
        })
    }

    /// Whether this source allows `url`.
    pub fn allows(&self, url: &Url) -> bool {
        if !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        match self {
            Self::Domain(domain) => {
                url.scheme() == "https"
                    && url.port().is_none()
                    && matches!(url.host(), Some(Host::Domain(host))
                        if host == domain
                            || host.strip_suffix(domain.as_str()).is_some_and(|rest| rest.ends_with('.')))
            }
            Self::Address { origin, path } => {
                url.origin().ascii_serialization() == *origin && path_is_below(url.path(), path)
            }
        }
    }

    /// The source that allows `url` and the rest of its server: its
    /// domain for HTTPS on the default port, otherwise its origin. `None`
    /// for an address that no source can allow, such as a `data:` URL.
    pub fn for_url(url: &Url) -> Option<Self> {
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        if url.scheme() == "https"
            && url.port().is_none()
            && let Some(Host::Domain(host)) = url.host()
            && let Ok(domain) = Self::domain(host)
        {
            return Some(domain);
        }
        let origin = url.origin().ascii_serialization();
        (origin != "null").then(|| Self::Address {
            origin,
            path: "/".to_owned(),
        })
    }

    /// The source that allows `url`'s folder and nothing else on its
    /// server: `https://a.example/kits/` for `https://a.example/kits/kick.wav`.
    pub fn folder_of(url: &Url) -> Option<Self> {
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return None;
        }
        let path = url.path();
        let folder = &path[..path.rfind('/').map_or(1, |slash| slash + 1)];
        Some(Self::Address {
            origin: url.origin().ascii_serialization(),
            path: folder.to_owned(),
        })
    }
}

/// Whether `path` is `folder` or inside it. A folder that does not end in
/// `/` still holds only whole names: `/kits` holds `/kits/a` but not
/// `/kitsune`.
fn path_is_below(path: &str, folder: &str) -> bool {
    path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| folder.ends_with('/') || rest.starts_with('/'))
}

impl std::fmt::Display for SourceRule {
    /// The source as it is written: `example.com`, or the address with no
    /// slash after a bare origin.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(domain) => f.write_str(domain),
            Self::Address { origin, path } if path == "/" => f.write_str(origin),
            Self::Address { origin, path } => write!(f, "{origin}{path}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(text: &str) -> SourceRule {
        SourceRule::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn url(text: &str) -> Url {
        Url::parse(text).expect("a test address")
    }

    #[test]
    fn a_domain_allows_https_on_itself_and_its_subdomains() {
        let domain = rule("Example.com");
        assert_eq!(domain, SourceRule::Domain("example.com".to_owned()));
        for allowed in [
            "https://example.com/kit.json",
            "https://cdn.example.com/a/b.wav",
        ] {
            assert!(domain.allows(&url(allowed)), "{allowed}");
        }
        for refused in [
            "http://example.com/kit.json",
            "https://example.com:8443/kit.json",
            "https://badexample.com/kit.json",
            "https://example.com.evil.net/kit.json",
            "https://user@example.com/kit.json",
        ] {
            assert!(!domain.allows(&url(refused)), "{refused}");
        }
    }

    #[test]
    fn an_address_allows_its_folder_and_nothing_beside_it() {
        let folder = rule("https://example.com/kits/");
        assert!(folder.allows(&url("https://example.com/kits/808/bd.wav")));
        assert!(!folder.allows(&url("https://example.com/other/bd.wav")));
        assert!(!folder.allows(&url("https://cdn.example.com/kits/bd.wav")));

        let bare = rule("https://example.com/kits");
        assert!(bare.allows(&url("https://example.com/kits/bd.wav")));
        assert!(!bare.allows(&url("https://example.com/kitsune.wav")));

        let file = rule("https://example.com/kits/bd.wav");
        assert!(file.allows(&url("https://example.com/kits/bd.wav")));
        assert!(!file.allows(&url("https://example.com/kits/sd.wav")));

        let local = rule("http://localhost:8000");
        assert_eq!(local.to_string(), "http://localhost:8000");
        assert!(local.allows(&url("http://localhost:8000/kit.json")));
        assert!(!local.allows(&url("http://localhost:9000/kit.json")));
    }

    #[test]
    fn what_is_not_a_source_says_how_to_write_it() {
        for (text, says) in [
            ("", "write a domain"),
            ("localhost", "https://localhost/"),
            ("example.com:8443", "Write the full address"),
            ("example.com/kits", "Write the full address"),
            ("127.0.0.1", "Write the full address"),
            ("ftp://example.com/", "not an http or https address"),
            ("https://u:p@example.com/", "user name or password"),
            ("https://example.com/kit.json?x=1", "? or # part"),
        ] {
            let error = SourceRule::parse(text).expect_err(text);
            assert!(error.contains(says), "{text}: {error}");
        }
    }

    #[test]
    fn a_refused_address_suggests_its_domain_or_its_origin() {
        let suggest = |text: &str| SourceRule::for_url(&url(text)).map(|rule| rule.to_string());
        assert_eq!(
            suggest("https://cdn.kit.example/a.wav").as_deref(),
            Some("cdn.kit.example")
        );
        assert_eq!(
            suggest("http://kit.example/a.wav").as_deref(),
            Some("http://kit.example")
        );
        assert_eq!(
            suggest("https://kit.example:8443/a.wav").as_deref(),
            Some("https://kit.example:8443")
        );
        assert_eq!(
            suggest("https://localhost/a.wav").as_deref(),
            Some("https://localhost")
        );
        assert_eq!(suggest("data:audio/wav;base64,AAAA"), None);

        let folder = SourceRule::folder_of(&url("https://kit.example/kits/808/bd.wav"));
        assert_eq!(
            folder.map(|rule| rule.to_string()).as_deref(),
            Some("https://kit.example/kits/808/")
        );
    }

    #[test]
    fn the_builtin_sources_are_domains() {
        for source in BUILTIN_SOURCES {
            assert_eq!(rule(source).to_string(), *source);
        }
    }
}
