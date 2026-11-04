//! The sources a score may load samples, images and videos from.
//!
//! A source is a domain, such as `example.com`, or an address, such as
//! `example.com/kits/`. Nothing else loads: a server's own headers
//! never widen the list.

use url::{Host, Url};

/// The domains every policy starts with, unless the host turns them off.
/// They serve the `github:`, `bubo:` and `shabda:` shorthands, the default
/// banks, and the freesound sounds that shared scores name.
pub const BUILTIN_SOURCES: &[&str] = &[
    "raw.githubusercontent.com",
    "strudel.b-cdn.net",
    "shabda.ndre.gr",
    "cdn.freesound.org",
];

/// What [`crate::ScoreSampleAccess::take_refused_sources`] gives for a score
/// that asked for `local:` files while the host chose no folder.
pub const LOCAL_FILES: &str = "local:";

/// One allowed source.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SourceRule {
    /// `example.com`: HTTPS from this domain and its subdomains, on the
    /// default port, at any path.
    Domain(String),
    /// `example.com/kits/`, which is `https://example.com/kits/`: the
    /// addresses on this origin whose path is `path` or below it. A path of
    /// `/` is the whole origin. This is the only form that allows plain
    /// HTTP or a server on this computer.
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
        // A path or a port makes an HTTPS address: `example.com/kits/`.
        if text.contains(['/', ':']) {
            return Self::address(&format!("https://{text}"));
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

    /// The address this source names: `https://example.com/` for a domain,
    /// and the address itself otherwise.
    pub fn url(&self) -> Option<Url> {
        match self {
            Self::Domain(domain) => Url::parse(&format!("https://{domain}/")).ok(),
            Self::Address { origin, path } => Url::parse(&format!("{origin}{path}")).ok(),
        }
    }

    /// Whether this source allows the address `other` names, as a source
    /// cut down from `other` does.
    pub fn covers(&self, other: &Self) -> bool {
        other.url().is_some_and(|url| self.allows(&url))
    }

    /// The source that allows `url`'s folder and nothing else on its
    /// server: `a.example/kits/` for `https://a.example/kits/kick.wav`. `None`
    /// for an address that no source can allow, such as a `data:` URL.
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
    /// The source as it is written: `example.com`, `example.com/kits/` for
    /// HTTPS on the default port, and the full address otherwise, with no
    /// slash after a bare origin.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(domain) => f.write_str(domain),
            Self::Address { origin, path } => match short_https(origin) {
                Some(host) => write!(f, "{host}{path}"),
                None if path == "/" => f.write_str(origin),
                None => write!(f, "{origin}{path}"),
            },
        }
    }
}

/// The host of an HTTPS origin on the default port, which a source can name
/// without its scheme: `example.com` for `https://example.com`.
fn short_https(origin: &str) -> Option<&str> {
    let host = origin.strip_prefix("https://")?;
    // An IPv6 host holds colons of its own; a port follows its bracket.
    let after_ip6 = host.rsplit(']').next().unwrap_or(host);
    (!after_ip6.contains(':')).then_some(host)
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
        assert_eq!(folder.to_string(), "example.com/kits/");
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
            ("127.0.0.1", "Write the full address"),
            ("ftp://example.com/", "not an http or https address"),
            ("https://u:p@example.com/", "user name or password"),
            ("https://example.com/kit.json?x=1", "? or # part"),
            ("example.com/kit.json#x", "? or # part"),
        ] {
            let error = SourceRule::parse(text).expect_err(text);
            assert!(error.contains(says), "{text}: {error}");
        }
    }

    /// A path or a port without a scheme is an HTTPS address, and an
    /// HTTPS address on the default port reads back without its scheme.
    #[test]
    fn a_path_or_a_port_without_a_scheme_is_https() {
        for (text, written) in [
            ("example.com/kits", "example.com/kits"),
            ("https://example.com/kits/", "example.com/kits/"),
            ("example.com:8443/kits/", "https://example.com:8443/kits/"),
            ("example.com:8443", "https://example.com:8443"),
            ("https://example.com", "example.com/"),
            ("[::1]/kits/", "[::1]/kits/"),
            ("http://example.com/kits/", "http://example.com/kits/"),
        ] {
            assert_eq!(rule(text).to_string(), written, "{text}");
            assert_eq!(rule(written), rule(text), "{written} reads back");
        }
        let short = rule("github.com/me/kit/");
        assert!(short.allows(&url("https://github.com/me/kit/raw/main/bd.wav")));
        assert!(!short.allows(&url("http://github.com/me/kit/bd.wav")));
        assert!(!short.allows(&url("https://raw.github.com/me/kit/bd.wav")));
    }

    /// A source cut down toward its domain covers the one it was. One that
    /// moves to another host, another scheme or a narrower path does not.
    #[test]
    fn a_wider_source_covers_a_narrower_one() {
        let folder = rule("github.com/me/kit/raw/main/");
        for wider in [
            "github.com/me/kit/raw/main/",
            "github.com/me/",
            "github.com",
            "https://github.com",
        ] {
            assert!(rule(wider).covers(&folder), "{wider}");
        }
        for other in [
            "gihtub.com",
            "raw.github.com",
            "http://github.com/me/",
            "github.com/me/kit/raw/main/drums/",
            "github.com:8443",
        ] {
            assert!(!rule(other).covers(&folder), "{other}");
        }
        assert!(!rule("github.com/me/").covers(&rule("github.com")));
    }

    #[test]
    fn a_refused_file_suggests_its_folder() {
        let folder = |text: &str| SourceRule::folder_of(&url(text)).map(|rule| rule.to_string());
        assert_eq!(
            folder("https://kit.example/kits/808/bd.wav").as_deref(),
            Some("kit.example/kits/808/")
        );
        assert_eq!(
            folder("https://kit.example/bd.wav").as_deref(),
            Some("kit.example/")
        );
        assert_eq!(
            folder("http://localhost:8000/kit.json").as_deref(),
            Some("http://localhost:8000")
        );
        assert_eq!(folder("data:audio/wav;base64,AAAA"), None);
    }

    #[test]
    fn the_builtin_sources_are_domains() {
        for source in BUILTIN_SOURCES {
            assert_eq!(rule(source).to_string(), *source);
        }
    }
}
