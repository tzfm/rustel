//! Remote sample fetches, and the boundary a score cannot cross.
//!
//! A score chooses sample sources: the map passed to `samples(...)`, and every
//! URL inside a fetched bank JSON. That choice is untrusted input to a process
//! running with the operator's permissions, so this layer enforces two rules
//! before any connection is made:
//!
//! - only `http` and `https`. A score names web resources, never files on this
//!   machine; `file://` exists solely for folders the operator registers as
//!   local samples (`samples('local:')`), which never goes through here.
//! - only public addresses, except when the granted URL explicitly names
//!   localhost, the `.localhost` tree, or a loopback literal. Such a name may
//!   reach loopback and nothing else. Private, link-local (where cloud
//!   metadata endpoints live), carrier-NAT, benchmarking, documentation,
//!   multicast, and selected special-purpose addresses are refused.
//!
//! Enforcement is at address resolution, not at string parsing. The agent's
//! resolver sees every connection that it attempts, including redirect
//! hops. A public URL that redirects to a private address fails at the
//! second hop, and a hostname that resolves to a private address is refused
//! even when the text hides it (`0x0a000001`, decimal and octal spellings).
//! The host is parsed out of the URL for one purpose only: an early refusal
//! at registration, with an error that names the fault.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const DNS_WORKERS: usize = 2;
const DNS_QUEUE_CAPACITY: usize = 8;
const MAX_DNS_ANSWERS: usize = 64;
const DNS_WAIT_SLICE: Duration = Duration::from_millis(10);
pub(crate) const MAX_REMOTE_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
/// Compressed bytes accepted for one score-selected Hydra image. Decoded
/// dimensions and allocations are capped separately by the image loader.
#[cfg(feature = "hydra")]
pub(crate) const MAX_REMOTE_IMAGE_BYTES: usize = 16 * 1024 * 1024;
/// Native scores are portable from the public Strudel editor. Sending its
/// origin reproduces an anonymous browser image request, and accepting only a
/// matching or wildcard response keeps CORS meaningful instead of treating it
/// as decorative metadata.
#[cfg(feature = "hydra")]
const HYDRA_IMAGE_REQUEST_ORIGIN: &str = "https://strudel.cc";

/// One wall-clock budget shared by every fetch in a manifest batch.
///
/// Ureq's request timeout includes redirects but cannot interrupt the system
/// resolver. The resolver below therefore observes this same absolute
/// deadline while it waits on a finite DNS pool. Passing an `Instant` rather
/// than a duration is what prevents recursion and successive manifests from
/// each receiving a fresh minute.
///
/// Cancellation prevents publication and promptly wakes callers waiting on
/// this module's DNS queue. Ureq cannot interrupt an active operating-system
/// connect or read; those calls remain bounded by this deadline and are
/// checked again before their bytes can be committed.
#[derive(Clone)]
pub(crate) struct FetchBudget {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    exhausted: Arc<AtomicBool>,
}

impl FetchBudget {
    pub(crate) fn until(deadline: Instant, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            deadline,
            cancelled,
            exhausted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn for_one_fetch() -> Self {
        Self::for_one_fetch_with_cancellation(Arc::new(AtomicBool::new(false)))
    }

    pub(crate) fn for_one_fetch_with_cancellation(cancelled: Arc<AtomicBool>) -> Self {
        Self::until(Instant::now() + FETCH_TIMEOUT, cancelled)
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        self.remaining().map(|_| ())
    }

    fn remaining_io(&self) -> io::Result<Duration> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "sample fetch cancelled",
            ));
        }
        if self.exhausted.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "sample manifest deadline exceeded",
            ));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "sample manifest deadline exceeded",
            ));
        }
        Ok(remaining)
    }

    pub(crate) fn remaining(&self) -> Result<Duration, String> {
        self.remaining_io().map_err(|error| error.to_string())
    }

    fn exhaust(&self) {
        self.exhausted.store(true, Ordering::Release);
    }
}

struct DnsJob {
    netloc: String,
    budget: FetchBudget,
    reply: mpsc::SyncSender<io::Result<Vec<SocketAddr>>>,
}

struct DnsPool {
    jobs: mpsc::SyncSender<DnsJob>,
}

type ResolveAddress = dyn Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync;

impl DnsPool {
    fn spawn(workers: usize, capacity: usize, resolve: Arc<ResolveAddress>) -> io::Result<Self> {
        let (jobs, receiver) = mpsc::sync_channel::<DnsJob>(capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..workers {
            let receiver = Arc::clone(&receiver);
            let resolve = Arc::clone(&resolve);
            std::thread::Builder::new()
                .name(format!("sample-dns-{index}"))
                .spawn(move || {
                    loop {
                        let job = { receiver.lock().expect("sample DNS queue").recv() };
                        let Ok(job) = job else {
                            return;
                        };
                        let result = job.budget.remaining_io().and_then(|_| resolve(&job.netloc));
                        let _ = job.reply.send(result);
                    }
                })?;
        }
        Ok(Self { jobs })
    }

    fn system() -> io::Result<&'static Self> {
        static POOL: OnceLock<Result<DnsPool, String>> = OnceLock::new();
        match POOL.get_or_init(|| {
            Self::spawn(
                DNS_WORKERS,
                DNS_QUEUE_CAPACITY,
                Arc::new(resolve_system_bounded),
            )
            .map_err(|error| format!("spawn bounded sample DNS workers: {error}"))
        }) {
            Ok(pool) => Ok(pool),
            Err(error) => Err(io::Error::other(error.clone())),
        }
    }

    fn resolve(&self, netloc: &str, budget: &FetchBudget) -> io::Result<Vec<SocketAddr>> {
        budget.remaining_io()?;
        // A literal address needs no name resolution: the parse is pure and
        // cannot block. It must not use the pool, because the pool has
        // shared, finite capacity, and one stuck lookup elsewhere in the
        // process could then stall a fetch to 127.0.0.1.
        if let Ok(addr) = netloc.parse::<SocketAddr>() {
            return Ok(vec![addr]);
        }
        let (reply, result) = mpsc::sync_channel(1);
        let mut job = DnsJob {
            netloc: netloc.to_owned(),
            budget: budget.clone(),
            reply,
        };
        // Resolution runs only on sample/manifest loader threads. Waiting for
        // finite queue capacity therefore cannot stall the live producer, and
        // avoids turning a momentary burst into a permanent Failed sample.
        loop {
            budget.remaining_io()?;
            match self.jobs.try_send(job) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    job = returned;
                    let remaining = budget.remaining_io()?;
                    std::thread::sleep(remaining.min(DNS_WAIT_SLICE));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "sample DNS workers stopped",
                    ));
                }
            }
        }

        loop {
            let remaining = budget.remaining_io()?;
            match result.recv_timeout(remaining.min(DNS_WAIT_SLICE)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "sample DNS worker stopped",
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod literal_address_tests {
    use super::*;

    /// Literal addresses parse without the shared DNS pool. A full DNS queue
    /// must not delay requests that need no name resolution.
    #[test]
    fn literal_addresses_parse_without_the_pool() {
        for (netloc, expected) in [
            ("127.0.0.1:8080", "127.0.0.1:8080"),
            ("0.0.0.0:1", "0.0.0.0:1"),
            ("[::1]:9000", "[::1]:9000"),
        ] {
            let parsed: SocketAddr = netloc.parse().expect("a literal address parses");
            assert_eq!(parsed.to_string(), expected);
        }
        // A name is NOT a literal and still needs resolving.
        assert!("localhost:8080".parse::<SocketAddr>().is_err());
        assert!("example.com:80".parse::<SocketAddr>().is_err());
    }
}

fn resolve_system_bounded(netloc: &str) -> io::Result<Vec<SocketAddr>> {
    let mut addrs = Vec::new();
    for addr in netloc.to_socket_addrs()? {
        if addrs.len() == MAX_DNS_ANSWERS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sample DNS answer limit exceeded",
            ));
        }
        addrs.push(addr);
    }
    Ok(addrs)
}

use url::Url;

/// The web address `text` names, or why it names none: the one reading of
/// an address a score or a bank file wrote, for the sample fetch
/// ([`wire_url`]) and for the page the studio's reveal opens alike.
///
/// - Whitespace around the text is not part of it: a `samples()` source
///   written in a multi-line template literal ends in a newline.
/// - It must be written `http://` or `https://` and name a host. Both are
///   read from the text, since the parser would take `https:///kit.wav`
///   for the host `kit.wav`.
/// - Every `#` is part of a file name, never the start of a fragment: bank
///   entries hold real names like `Kick #2.wav`, so it is encoded `%23`.
/// - The URL parser writes the rest out again: it percent-encodes what a
///   URL cannot carry as written - a `"` for a cymbal's size in inches, a
///   control character - and drops a tab or newline.
/// - A host is decoded rather than encoded, so `a%22b.example` holds a quote
///   once parsed. No real host does, and the address outlives the fetch, so
///   a quote or control character in the parsed address is refused.
pub fn web_address(text: &str) -> Result<Url, String> {
    let text = text.trim_ascii();
    // Schemes are case-insensitive (`HTTPS://` is legal), so the one read
    // here is lowercased before it is compared.
    let (scheme, rest) = text
        .split_once(':')
        .ok_or_else(|| format!("{text:?} names no scheme"))?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Err(format!(
            "{text:?} is a {scheme}: address; only http and https are allowed"
        ));
    }
    let rest = rest
        .strip_prefix("//")
        .ok_or_else(|| format!("{text:?} is not written {scheme}://host"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Userinfo ends at the last '@' of the authority; the host follows it.
    let host = match authority.rsplit_once('@') {
        Some((_, host)) => host,
        None => authority,
    };
    if host.is_empty() {
        return Err(format!("{text:?} names no host"));
    }
    let parsed = Url::parse(&text.replace('#', "%23"))
        .map_err(|error| format!("{text:?} is not a valid URL: {error}"))?;
    if let Some(bad) = forbidden_character(parsed.as_str()) {
        return Err(format!(
            "{text:?} holds {bad:?} once parsed, which no web address does"
        ));
    }
    Ok(parsed)
}

/// The first double quote or ASCII control character in `text`.
fn forbidden_character(text: &str) -> Option<char> {
    text.chars()
        .find(|character| *character == '"' || character.is_ascii_control())
}

/// Parse the exact HTTP(S) request identity used by every sample route: the
/// [`web_address`] a sample source names, refused in the source's name.
///
/// Called when a `samples()` source or bank entry is registered, so a bad one
/// fails loudly before anything tries to load, and again inside [`fetch`] so
/// the rule holds no matter who calls. Returning the parsed URL gives
/// authorization, cache keys, and the request itself one structural
/// identity instead of three text normalizations.
///
/// What is fetched is always this parsed address, and it is what a score's
/// bank registers. A trusted bank registers its text as written and is
/// parsed again here each time it is fetched; the reveal reads that same
/// text through [`web_address`] before it hands a page to the desktop.
pub(crate) fn wire_url(url: &str) -> Result<Url, String> {
    web_address(url).map_err(|error| format!("sample source {error}"))
}

pub fn remote_url_allowed(url: &str) -> Result<(), String> {
    wire_url(url).map(|_| ())
}

fn ipv4_allowed(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(
        ip.is_loopback()                                    // judged by the NAME, not here
            ||
            o[0] == 0                                       // "this network" (0.0.0.0/8)
            || o[0] == 10                                   // private (10.0.0.0/8)
            || (o[0] == 100 && (o[1] & 0xC0) == 64)         // carrier NAT (100.64/10)
            || (o[0] == 169 && o[1] == 254)                 // link-local / metadata endpoints
            || (o[0] == 172 && (o[1] & 0xF0) == 16)         // private (172.16/12)
            || (o[0] == 192 && o[1] == 0 && o[2] == 0)      // protocol assignments
            || (o[0] == 192 && o[1] == 0 && o[2] == 2)      // TEST-NET-1
            || (o[0] == 192 && o[1] == 88 && o[2] == 99)   // deprecated 6to4 relay anycast
            || (o[0] == 192 && o[1] == 168)                 // private (192.168/16)
            || (o[0] == 198 && (o[1] & 0xFE) == 18)         // benchmarking (198.18/15)
            || (o[0] == 198 && o[1] == 51 && o[2] == 100)  // TEST-NET-2
            || (o[0] == 203 && o[1] == 0 && o[2] == 113)   // TEST-NET-3
            || o[0] >= 224
        // multicast + reserved
    )
}

/// The IPv4 address hiding inside an IPv6 one, if there is one.
///
/// Several IPv6 forms carry a v4 address that something on the path translates
/// back. Judging only the sixteen bytes lets a private target wear an IPv6
/// costume: on an IPv6-only network `64:ff9b::a00:1` is `10.0.0.1` the moment
/// it reaches a NAT64 gateway.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let tail = || Ipv4Addr::from((u32::from(s[6]) << 16) | u32::from(s[7]));
    // ::ffff:a.b.c.d, the mapped form.
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return Some(mapped);
    }
    // The IPv4-translated `::ffff:0:a.b.c.d` form. Rust's
    // `to_ipv4_mapped` intentionally recognizes only the distinct
    // `::ffff:a.b.c.d` mapped form; translated addresses must expose their
    // tail here as well.
    if s[..4].iter().all(|&segment| segment == 0) && s[4] == 0xffff && s[5] == 0 {
        return Some(tail());
    }
    // ::a.b.c.d, the deprecated compatible form. Kernels do not route it
    // without an explicit tunnel, but it costs one arm to judge honestly.
    if s[..6].iter().all(|&segment| segment == 0) && !(s[6] == 0 && s[7] <= 1) {
        return Some(tail());
    }
    // NAT64 well-known prefix 64:ff9b::/96 (RFC 6052).
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6].iter().all(|&segment| segment == 0) {
        return Some(tail());
    }
    // 6to4, 2002::/16, carries the address in the two segments after it.
    if s[0] == 0x2002 {
        return Some(Ipv4Addr::from((u32::from(s[1]) << 16) | u32::from(s[2])));
    }
    None
}

fn ipv6_allowed(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // Teredo (2001:0000::/32) obscures an IPv4 endpoint rather than carrying
    // it in a stable plain tail. ISATAP uses a recognizable interface ID but
    // can sit beneath an arbitrary prefix. Neither is needed for public media
    // hosting, so refuse both instead of trying to reverse every tunnel form.
    let teredo = s[0] == 0x2001 && s[1] == 0;
    let isatap = matches!(s[4], 0 | 0x0200) && s[5] == 0x5efe;
    if teredo || isatap {
        return false;
    }
    // Anything carrying an IPv4 address is judged by that address, so
    // ::ffff:10.0.0.1 and 64:ff9b::a00:1 cannot dress a private target up as
    // sixteen bytes.
    if let Some(embedded) = embedded_ipv4(ip) {
        return ipv4_allowed(embedded);
    }
    // 64:ff9b:1::/48, the RFC 8215 local-use NAT64 prefix, places the embedded
    // address differently depending on the prefix length actually deployed.
    // Rather than guess which, refuse the range: nothing legitimate serves
    // samples from it.
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0x0001 {
        return false;
    }
    !({
        ip.is_loopback()                                    // ::1, judged by the NAME
            || s.iter().all(|&segment| segment == 0)        // unspecified ::
            || (s[0] & 0xfe00) == 0xfc00                    // unique-local fc00::/7
            || (s[0] & 0xffc0) == 0xfe80                    // link-local fe80::/10
            || (s[0] & 0xffc0) == 0xfec0                    // deprecated site-local fec0::/10
            || (s[0] == 0x0100 && s[1..4].iter().all(|&segment| segment == 0)) // discard-only 100::/64
            || (s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0) // benchmarking 2001:2::/48
            || (s[0] == 0x2001 && (s[1] & 0xfff0) == 0x0010) // deprecated ORCHID 2001:10::/28
            || (s[0] == 0x2001 && (s[1] & 0xfff0) == 0x0020) // special-purpose ORCHIDv2 2001:20::/28
            || (s[0] == 0x2001 && s[1] == 0x0db8)         // documentation 2001:db8::/32
            || (s[0] == 0x3fff && (s[1] & 0xf000) == 0)   // documentation 3fff::/20
            || s[0] == 0x5f00                             // SRv6 SIDs 5f00::/16
            || (s[0] & 0xff00) == 0xff00 // multicast ff00::/8
    })
}

/// Whether the host that the caller asked for explicitly names loopback.
///
/// The address alone cannot supply this. If any loopback answer were
/// accepted, a granted `http://samples.example` would reach a local service
/// when DNS or `/etc/hosts` points it there. An exact-origin grant controls
/// the URL text, and the resolver controls which machine that text reaches.
/// A loopback answer therefore counts only when the URL names loopback.
fn host_names_loopback(netloc: &str) -> bool {
    // `netloc` is `host:port`, and an IPv6 literal wears brackets.
    let host = if let Some(rest) = netloc.strip_prefix('[') {
        rest.split_once(']').map_or(rest, |(host, _)| host)
    } else {
        netloc.rsplit_once(':').map_or(netloc, |(host, _)| host)
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // RFC 6761 reserves the whole `.localhost` tree for loopback.
    if host.len() > "localhost".len()
        && host[host.len() - "localhost".len() - 1..].eq_ignore_ascii_case(".localhost")
    {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback(),
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback() || embedded_ipv4(ip).is_some_and(|v4| v4.is_loopback())
        }
        Err(_) => false,
    }
}

/// Whether a resolved address is one a score may make us talk to.
///
/// `loopback_named` is whether the granted URL names loopback
/// ([`url_names_loopback`]). It is decided once per grant and never from a
/// redirect hop's own host. Loopback is reachable only when the caller named
/// it, and a grant that names loopback reaches nothing else: a `localhost`
/// entry that resolves to a non-loopback address is refused too.
pub fn addr_allowed(ip: IpAddr, loopback_named: bool) -> bool {
    if addr_is_loopback(ip) {
        return loopback_named;
    }
    if loopback_named {
        return false;
    }
    addr_is_public(ip)
}

fn addr_is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback() || embedded_ipv4_is_loopback(ip)
}

fn embedded_ipv4_is_loopback(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V6(ip) if embedded_ipv4(ip).is_some_and(|v4| v4.is_loopback()))
}

fn addr_is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ipv4_allowed(ip),
        IpAddr::V6(ip) => ipv6_allowed(ip),
    }
}

/// The DNS view the agent connects through: refuse a name whose resolution
/// answers with ANY address outside the allowed set, rather than picking the
/// first acceptable one. Failing closed keeps a mixed answer from becoming a
/// coin flip about where the bytes go.
///
/// Production fetches pin addresses through [`resolve_guarded`] into a
/// [`ResolvedHop`] before connecting. This resolver remains for unit tests of
/// the address policy without building a full agent; it judges each name as
/// its own grant, which is what those tests resolve and ask about.
#[cfg_attr(not(test), allow(dead_code))]
struct GuardedResolver {
    timeout: Duration,
}

#[cfg_attr(not(test), allow(dead_code))]
impl ureq::Resolver for GuardedResolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        resolve_guarded(
            netloc,
            &FetchBudget::until(
                Instant::now() + self.timeout,
                Arc::new(AtomicBool::new(false)),
            ),
            host_names_loopback(netloc),
        )
    }
}

/// Resolve one hop and judge it under the address policy.
///
/// `loopback_named` is what the granted URL named. The caller computes it
/// once; this function does not derive it from the hop's own text. A hop
/// that named loopback itself would otherwise grant itself loopback access:
/// a server-chosen `Location: http://127.0.0.1:PORT/` is the inward second
/// hop that the module header forbids.
fn resolve_guarded(
    netloc: &str,
    budget: &FetchBudget,
    loopback_named: bool,
) -> io::Result<Vec<SocketAddr>> {
    let addrs = DnsPool::system()?.resolve(netloc, budget)?;
    guard_resolved_addresses(netloc, addrs, loopback_named)
}

/// Hydra images are internet resources, never the local sample-server escape
/// hatch. Every lookup, redirects included, must resolve entirely to public
/// addresses.
#[cfg(feature = "hydra")]
fn resolve_public(netloc: &str, budget: &FetchBudget) -> io::Result<Vec<SocketAddr>> {
    let addrs = DnsPool::system()?.resolve(netloc, budget)?;
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Hydra image host has no address",
        ));
    }
    if addrs.iter().any(|addr| !addr_is_public(addr.ip())) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Hydra image destination is not a public address",
        ));
    }
    Ok(addrs)
}

/// Judge every resolved address of one hop. `loopback_named` comes from the
/// GRANTED URL (see [`addr_allowed`] and [`resolve_guarded`]); this function
/// must not recompute it from `netloc`, or a redirect hop would name its own
/// grant.
fn guard_resolved_addresses(
    netloc: &str,
    addrs: Vec<SocketAddr>,
    loopback_named: bool,
) -> io::Result<Vec<SocketAddr>> {
    if addrs.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no address for {netloc}"),
        ));
    }
    match addrs
        .iter()
        .find(|addr| !addr_allowed(addr.ip(), loopback_named))
    {
        // A grant that named loopback is refused an address for leaving
        // loopback, which may well be public; saying "not a public address"
        // there would point at the wrong half of the rule.
        Some(addr) if loopback_named && !addr_is_loopback(addr.ip()) => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "sample fetch refused: {addr} is not loopback, and the granted URL named loopback"
            ),
        )),
        Some(addr) => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("sample fetch refused: {addr} is not a public address"),
        )),
        None => Ok(addrs),
    }
}

#[derive(Clone)]
struct ResolvedHop {
    netloc: String,
    addrs: Vec<SocketAddr>,
}

impl ureq::Resolver for ResolvedHop {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        if !netloc.eq_ignore_ascii_case(&self.netloc) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "sample fetch resolver was used for a different host",
            ));
        }
        Ok(self.addrs.clone())
    }
}

fn url_netloc(url: &Url) -> Result<String, String> {
    let host = url
        .host_str()
        .ok_or_else(|| format!("sample source {url:?} names no host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("sample source {url:?} names no port"))?;
    // `Url::host_str` preserves the brackets around an IPv6 literal, which
    // is already exactly the authority syntax expected by `ToSocketAddrs` and
    // ureq's resolver.
    Ok(format!("{host}:{port}"))
}

fn manifest_agent_with_resolver(
    url: &Url,
    budget: &FetchBudget,
    resolve: impl FnOnce(&str, &FetchBudget) -> io::Result<Vec<SocketAddr>>,
) -> Result<(ureq::Agent, Duration), String> {
    let netloc = url_netloc(url)?;
    // ureq 2.12 gives its configured connect timeout precedence over the
    // request-wide deadline, and its default connect timeout is thirty
    // seconds. Resolve under the aggregate clock first, then give this one-hop
    // agent only what remains. Otherwise DNS can spend almost the whole batch
    // budget before TCP receives a fresh copy of it.
    let addrs = resolve(&netloc, budget).map_err(|error| format!("resolve {netloc}: {error}"))?;
    let remaining = budget.remaining()?;
    Ok((
        ureq::AgentBuilder::new()
            .resolver(ResolvedHop { netloc, addrs })
            // A score's network boundary is the resolver above. Inheriting a
            // process proxy would silently replace the destination it inspects.
            .try_proxy_from_env(false)
            .redirects(0)
            // This is measured AFTER the guarded DNS result. See above:
            // omitting it would restore ureq's independent thirty-second
            // default.
            .timeout_connect(remaining)
            .timeout_read(remaining)
            .timeout_write(remaining)
            .build(),
        remaining,
    ))
}

/// Why one hop failed. An HTTP error status is kept as data, apart from the
/// URL its message names.
enum HopError {
    Status { url: Url, code: u16 },
    Other(String),
}

impl From<String> for HopError {
    fn from(message: String) -> Self {
        HopError::Other(message)
    }
}

impl From<HopError> for String {
    fn from(error: HopError) -> Self {
        match error {
            // Rendered without ureq's own status text, which names the URL
            // again.
            HopError::Status { url, code } => format!(
                "GET {url}: status code {code}{}",
                github_hint(url.as_str(), code)
            ),
            HopError::Other(message) => message,
        }
    }
}

/// Issue one request with redirects disabled, after resolving that hop through
/// the guarded address policy. `request_origin` and `accept` are the `Origin`
/// and `Accept` headers to send, when the caller wants them.
///
/// Score-source fetching needs both halves. Checking the origin of a URL is
/// text: it says nothing about the address the name resolves to, so an origin
/// a host granted in good faith can still point at a private target, and can
/// point somewhere different on the second lookup. Every hop goes through the
/// same address check as an ordinary sample fetch for that reason.
fn single_hop_get_with_resolver(
    url: &Url,
    budget: &FetchBudget,
    request_origin: Option<&str>,
    accept: Option<&str>,
    resolve: impl FnOnce(&str, &FetchBudget) -> io::Result<Vec<SocketAddr>>,
) -> Result<ureq::Response, HopError> {
    let (agent, remaining) = manifest_agent_with_resolver(url, budget, resolve)?;
    let mut request = agent.get(url.as_str()).timeout(remaining);
    if let Some(origin) = request_origin {
        // A server whose CORS response is dynamic answers only when asked.
        request = request.set("Origin", origin);
    }
    if let Some(accept) = accept {
        request = request.set("Accept", accept);
    }
    request.call().map_err(|error| {
        // Socket timeout messages are platform-specific. Once the shared
        // request budget is spent, expose the stable deadline contract rather
        // than requiring callers to recognize an OS error string.
        if request_error_is_timeout(&error) {
            // The request was configured with all time left in this budget.
            // Some hosts report that timeout a timer tick before `Instant`
            // reaches the same deadline. Mark every clone exhausted so a
            // recursive manifest or later batch effect cannot spend it again.
            budget.exhaust();
            return format!("GET {url}: sample manifest deadline exceeded: {error}").into();
        }
        if budget.remaining().is_err() {
            return format!("GET {url}: sample manifest deadline exceeded: {error}").into();
        }
        if let ureq::Error::Status(code, _) = error {
            return HopError::Status {
                url: url.clone(),
                code,
            };
        }
        format!("GET {url}: {error}").into()
    })
}

/// What a 404 on a GitHub manifest usually means.
///
/// `github:user` with no repository is read as a repository called
/// `samples`. That guess is usually wrong, and the bare 404 does not show
/// that the repository name was a guess. State it here, where the URL is
/// known.
fn github_hint(url: &str, code: u16) -> String {
    let guessed = url.starts_with("https://raw.githubusercontent.com/")
        && url.contains("/samples/")
        && code == 404;
    if guessed {
        " - `github:user` on its own looks for a repository called \
         `samples`; write `github:user/repo` if it is called something else"
            .to_owned()
    } else {
        String::new()
    }
}

fn request_error_is_timeout(error: &ureq::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = current {
        if let Some(error) = error.downcast_ref::<io::Error>()
            && matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            )
        {
            return true;
        }
        current = error.source();
    }
    false
}

/// `request_origin` is the `Origin` header to send, for fetches whose caller
/// will require the response to consent to cross-origin reads.
///
/// Callers must pin every redirect to the granted origin before this call,
/// as the score path does. The hop's own naming is then the grant's naming,
/// and loopback can be judged from it. A fetch that lets the server choose a
/// new host per hop must carry the granted URL's naming across its hops
/// itself; see [`fetch_following_redirects`].
pub(crate) fn guarded_single_hop_get(
    url: &Url,
    budget: &FetchBudget,
    request_origin: Option<&str>,
) -> Result<ureq::Response, String> {
    single_hop_get_with_resolver(url, budget, request_origin, None, |netloc, hop_budget| {
        resolve_guarded(netloc, hop_budget, host_names_loopback(netloc))
    })
    .map_err(String::from)
}

/// Whether a parsed URL's host explicitly names loopback.
///
/// The trusted fetch path asks this once per grant and judges every redirect
/// hop by the answer (see [`fetch_following_redirects`]). The default score
/// policy uses it to refuse: it never fetches a URL that names loopback,
/// whatever the name resolves to. The operator's own machine needs an
/// explicit grant.
pub(crate) fn url_names_loopback(url: &Url) -> bool {
    url_netloc(url).is_ok_and(|netloc| host_names_loopback(&netloc))
}

/// Fetch one remote sample, size-capped like the rest of the loader.
pub(crate) fn fetch(url: &str) -> Result<Vec<u8>, String> {
    fetch_audio_with_budget(url, &FetchBudget::for_one_fetch())
}

pub(crate) fn fetch_audio_with_budget(url: &str, budget: &FetchBudget) -> Result<Vec<u8>, String> {
    fetch_following_redirects(url, budget, rustel_audio::sample_pcm_ceiling(), "sample")
}

/// Fetch under an absolute budget owned by the surrounding manifest batch.
pub(crate) fn fetch_manifest_with_budget(
    url: &str,
    budget: &FetchBudget,
) -> Result<Vec<u8>, String> {
    fetch_following_redirects(url, budget, MAX_REMOTE_MANIFEST_BYTES, "sample manifest")
}

/// Fetch one Hydra image through the same per-hop DNS/redirect boundary as
/// samples. An image URL is score text too: it must not gain a second, weaker
/// network path merely because its bytes are destined for the GPU.
#[cfg(feature = "hydra")]
pub(crate) fn fetch_image_with_budget(
    url: &str,
    budget: &FetchBudget,
    access: &crate::samples::ScoreSampleAccess,
) -> Result<Vec<u8>, String> {
    let mut current = hydra_image_url(url)?;
    // A score-selected image must not start a request solely because it
    // satisfies the image loader's address and CORS restrictions.
    access.approve_hydra_image_url(current.as_str())?;
    let origin = current.origin();
    budget.check()?;
    for redirects in 0..=5 {
        let response = hydra_image_hop(&current, budget, resolve_public)?;
        let action = inspect_hydra_image_response(
            &current,
            &origin,
            response.status(),
            response.header("Location"),
            response.header("Access-Control-Allow-Origin"),
        )?;
        if let HydraImageResponseAction::Redirect(next) = action {
            if redirects == 5 {
                return Err("Hydra image request followed too many redirects".into());
            }
            current = next;
            continue;
        }

        refuse_web_page(response.content_type())?;
        if response
            .header("Content-Length")
            .and_then(|length| length.parse::<u64>().ok())
            .is_some_and(|length| length > MAX_REMOTE_IMAGE_BYTES as u64)
        {
            return Err("Hydra image exceeds the 16 MiB transfer limit".into());
        }
        let mut bytes = Vec::new();
        if let Some(length) = response
            .header("Content-Length")
            .and_then(|length| length.parse::<usize>().ok())
        {
            bytes
                .try_reserve_exact(length.min(MAX_REMOTE_IMAGE_BYTES))
                .map_err(|_| "Hydra image response exceeds host memory".to_owned())?;
        }
        use std::io::Read;
        response
            .into_reader()
            .take(MAX_REMOTE_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Hydra image response could not be read".to_owned())?;
        budget.check()?;
        if bytes.len() > MAX_REMOTE_IMAGE_BYTES {
            return Err("Hydra image exceeds the 16 MiB transfer limit".into());
        }
        return Ok(bytes);
    }
    unreachable!("Hydra image redirect loop returns at its fixed bound")
}

/// One hop of a Hydra image fetch, sent as a browser sends an anonymous image
/// request. An error names the HTTP status when there is one, and never the
/// URL.
#[cfg(feature = "hydra")]
fn hydra_image_hop(
    url: &Url,
    budget: &FetchBudget,
    resolve: impl FnOnce(&str, &FetchBudget) -> io::Result<Vec<SocketAddr>>,
) -> Result<ureq::Response, String> {
    single_hop_get_with_resolver(
        url,
        budget,
        Some(HYDRA_IMAGE_REQUEST_ORIGIN),
        Some(&hydra_image_accept()),
        resolve,
    )
    .map_err(|error| match error {
        HopError::Status { code, .. } => hydra_status_error(code),
        HopError::Other(message) => safe_hydra_request_error(&message),
    })
}

/// The `Accept` header of a Hydra image request: the MIME type of every format
/// the image decoder reads, so a server that negotiates answers with one.
#[cfg(feature = "hydra")]
fn hydra_image_accept() -> String {
    image::ImageFormat::all()
        .filter(image::ImageFormat::reading_enabled)
        .map(|format| format.to_mime_type())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(feature = "hydra")]
fn hydra_status_error(status: u16) -> String {
    format!("Hydra image request returned HTTP {status}")
}

#[cfg(feature = "hydra")]
#[derive(Debug, Eq, PartialEq)]
enum HydraImageResponseAction {
    Redirect(Url),
    Body,
}

/// Parse a browser image URL without the sample loader's compatibility rewrite
/// for `#` in legacy sample filenames. A fragment is ordinary URL metadata and
/// is removed before the request, exactly as it is by a browser.
#[cfg(feature = "hydra")]
fn hydra_image_url(raw: &str) -> Result<Url, String> {
    let mut url = Url::parse(raw).map_err(|_| "Hydra image URL is not valid".to_owned())?;
    validate_hydra_image_url(&url)?;
    url.set_fragment(None);
    Ok(url)
}

#[cfg(feature = "hydra")]
fn validate_hydra_image_url(url: &Url) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err("Hydra images require a public HTTPS URL".into());
    }
    if url.host_str().is_none() {
        return Err("Hydra image URL names no host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Hydra image URLs may not contain credentials".into());
    }
    let netloc = url_netloc(url).map_err(|_| "Hydra image URL names no host".to_owned())?;
    if host_names_loopback(&netloc) {
        return Err("Hydra images require a public HTTPS URL".into());
    }
    if let Some(ip) = url.host_str().and_then(|host| host.parse::<IpAddr>().ok())
        && !addr_is_public(ip)
    {
        return Err("Hydra images require a public HTTPS URL".into());
    }
    Ok(())
}

#[cfg(feature = "hydra")]
fn inspect_hydra_image_response(
    current: &Url,
    original_origin: &url::Origin,
    status: u16,
    location: Option<&str>,
    allow_origin: Option<&str>,
) -> Result<HydraImageResponseAction, String> {
    let cors = allow_origin.is_some_and(|allowed| {
        let allowed = allowed.trim();
        allowed == "*" || allowed == HYDRA_IMAGE_REQUEST_ORIGIN
    });
    if !cors {
        return Err(format!(
            "Hydra image response did not grant CORS access to {HYDRA_IMAGE_REQUEST_ORIGIN}"
        ));
    }
    if (200..300).contains(&status) {
        return Ok(HydraImageResponseAction::Body);
    }
    if !(300..400).contains(&status) {
        // Ureq currently turns 4xx/5xx into an error before this helper, but
        // retain the status boundary here as well so a transport upgrade
        // cannot accidentally reinterpret an error page as image bytes.
        return Err(hydra_status_error(status));
    }
    let location = location.ok_or_else(|| "Hydra image redirect has no Location".to_owned())?;
    let mut next = current
        .join(location)
        .map_err(|_| "Hydra image redirect Location is invalid".to_owned())?;
    validate_hydra_image_url(&next)?;
    if &next.origin() != original_origin {
        return Err("Hydra image redirects must stay on the original HTTPS origin".into());
    }
    next.set_fragment(None);
    Ok(HydraImageResponseAction::Redirect(next))
}

/// Refuse a final response whose media type is a web page: a link to an
/// image's page rather than the image itself.
#[cfg(feature = "hydra")]
fn refuse_web_page(media_type: &str) -> Result<(), String> {
    if media_type.trim().eq_ignore_ascii_case("text/html") {
        return Err(
            "Hydra image URL returned a web page, not an image; use the direct image link".into(),
        );
    }
    Ok(())
}

/// Network-library errors may contain the full request URL. Scores commonly
/// put signed tokens in queries/fragments, so translate to a small stable set
/// without reflecting the error string into studio diagnostics.
#[cfg(feature = "hydra")]
fn safe_hydra_request_error(error: &str) -> String {
    if error.contains("cancelled") {
        "Hydra image request was cancelled".into()
    } else if error.contains("deadline") || error.contains("timed out") {
        "Hydra image request timed out".into()
    } else if error.contains("public address") || error.contains("not a public") {
        "Hydra image destination is not a public address".into()
    } else {
        "Hydra image request failed".into()
    }
}

fn fetch_following_redirects(
    url: &str,
    budget: &FetchBudget,
    max_bytes: usize,
    kind: &str,
) -> Result<Vec<u8>, String> {
    fetch_following_redirects_via(url, budget, max_bytes, kind, resolve_guarded)
}

/// [`fetch_following_redirects`] with the resolver passed in, so a test can
/// stand a loopback listener in for a public host and drive the real hop loop.
/// `resolve` receives the grant's loopback naming, never the hop's own.
fn fetch_following_redirects_via(
    url: &str,
    budget: &FetchBudget,
    max_bytes: usize,
    kind: &str,
    resolve: impl Fn(&str, &FetchBudget, bool) -> io::Result<Vec<SocketAddr>>,
) -> Result<Vec<u8>, String> {
    remote_url_allowed(url)?;
    budget.check()?;
    let encoded = url.replace('#', "%23");
    let mut current = url::Url::parse(&encoded)
        .map_err(|error| format!("sample source {url:?} is not a valid URL: {error}"))?;
    // Loopback is reachable only when the granted URL names it. That is this
    // URL, not a later redirect target. The naming is decided once here and
    // carried across every hop, so a server-chosen
    // `Location: http://127.0.0.1:PORT/` cannot grant itself loopback access.
    let loopback_named = url_names_loopback(&current);
    for redirects in 0..=5 {
        remote_url_allowed(current.as_str())?;
        // Manifest and audio fetches both resolve under the caller's
        // FetchBudget, so a lookup honours the deadline and cancellation.
        let response =
            single_hop_get_with_resolver(&current, budget, None, None, |netloc, hop_budget| {
                resolve(netloc, hop_budget, loopback_named)
            })?;
        if (300..400).contains(&response.status()) {
            if redirects == 5 {
                return Err(format!("GET {url}: too many redirects"));
            }
            let location = response
                .header("Location")
                .ok_or_else(|| format!("GET {url}: redirect has no Location"))?
                .to_owned();
            current = current
                .join(&location)
                .map_err(|error| format!("GET {url}: invalid redirect: {error}"))?;
            continue;
        }

        if response
            .header("Content-Length")
            .and_then(|length| length.parse::<u64>().ok())
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(format!("{url} exceeds the {kind} size limit"));
        }
        let mut bytes = Vec::new();
        if let Some(length) = response
            .header("Content-Length")
            .and_then(|length| length.parse::<usize>().ok())
        {
            bytes
                .try_reserve_exact(length.min(max_bytes))
                .map_err(|_| format!("{kind} response exceeds host memory"))?;
        }
        use std::io::Read;
        response
            .into_reader()
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read {url}: {error}"))?;
        budget.check()?;
        if bytes.len() > max_bytes {
            return Err(format!("{url} exceeds the {kind} size limit"));
        }
        return Ok(bytes);
    }
    unreachable!("redirect loop returns at its fixed bound")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use ureq::Resolver;

    fn test_budget(duration: Duration) -> FetchBudget {
        FetchBudget::until(Instant::now() + duration, Arc::new(AtomicBool::new(false)))
    }

    fn guarded_resolver() -> GuardedResolver {
        GuardedResolver {
            timeout: Duration::from_secs(5),
        }
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_images_default_to_public_https_without_sample_hash_rewriting() {
        for refused in [
            "http://images.example/texture.png",
            "HTTP://images.example/texture.png",
            "https://localhost/texture.png",
            "https://127.0.0.1/texture.png",
            "https://0x7f000001/texture.png",
            "https://2130706433/texture.png",
            "https://[::1]/texture.png",
            "https://192.168.1.4/texture.png",
            "https://user:secret@images.example/texture.png",
        ] {
            assert!(hydra_image_url(refused).is_err(), "accepted {refused}");
        }
        let parsed =
            hydra_image_url("https://i.imgur.com/zFttbWq.jpg?variant=large#browser-fragment")
                .expect("the gallery sketch remains a valid source");
        assert_eq!(parsed.path(), "/zFttbWq.jpg");
        assert_eq!(parsed.query(), Some("variant=large"));
        assert_eq!(parsed.fragment(), None, "fragments never go on the wire");

        // Unlike the sample compatibility path, a literal hash is URL syntax,
        // not rewritten into a `%23` filename.
        assert_eq!(
            hydra_image_url("https://images.example/take#2.png")
                .unwrap()
                .path(),
            "/take"
        );
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_image_redirects_cannot_change_origin_or_reach_loopback() {
        let current = hydra_image_url("https://images.example/first.png").unwrap();
        let origin = current.origin();
        for target in [
            "https://cdn.example/second.png",
            "https://images.example:444/second.png",
            "https://127.0.0.1/private.png",
            "http://images.example/downgrade.png",
        ] {
            assert!(
                inspect_hydra_image_response(&current, &origin, 302, Some(target), Some("*"))
                    .is_err(),
                "followed {target}"
            );
        }
        assert!(resolve_public("127.0.0.1:443", &test_budget(Duration::from_secs(1))).is_err());

        let default_port = inspect_hydra_image_response(
            &current,
            &origin,
            302,
            Some("https://IMAGES.EXAMPLE:443/second.png"),
            Some("*"),
        )
        .expect("URL origins normalize host case and the default HTTPS port");
        assert!(matches!(
            default_port,
            HydraImageResponseAction::Redirect(_)
        ));
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_images_require_cors_on_redirects_and_final_responses() {
        let current = hydra_image_url("https://images.example/first.png").unwrap();
        let origin = current.origin();
        assert!(
            inspect_hydra_image_response(&current, &origin, 200, None, None).is_err(),
            "a response without CORS was accepted"
        );
        assert!(
            inspect_hydra_image_response(&current, &origin, 302, Some("/next.png"), None).is_err(),
            "a redirect without CORS was accepted"
        );
        assert_eq!(
            inspect_hydra_image_response(
                &current,
                &origin,
                200,
                None,
                Some(HYDRA_IMAGE_REQUEST_ORIGIN),
            ),
            Ok(HydraImageResponseAction::Body)
        );
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_image_non_success_statuses_are_rejected() {
        let current = hydra_image_url("https://images.example/a.png").unwrap();
        let origin = current.origin();
        for status in [101, 404, 500] {
            assert!(
                inspect_hydra_image_response(&current, &origin, status, None, Some("*")).is_err(),
                "accepted HTTP status {status}"
            );
        }
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_same_origin_cors_redirect_and_imgur_wildcard_succeed() {
        let current = hydra_image_url("https://images.example/a.png").unwrap();
        let origin = current.origin();
        let next = inspect_hydra_image_response(
            &current,
            &origin,
            302,
            Some("/b.png?signature=secret#not-sent"),
            Some("*"),
        )
        .expect("same-origin redirect with CORS");
        let HydraImageResponseAction::Redirect(next) = next else {
            panic!("expected redirect");
        };
        assert_eq!(next.origin(), origin);
        assert_eq!(next.fragment(), None);
        assert_eq!(
            inspect_hydra_image_response(
                &next,
                &origin,
                200,
                None,
                Some(HYDRA_IMAGE_REQUEST_ORIGIN)
            ),
            Ok(HydraImageResponseAction::Body)
        );

        let imgur = hydra_image_url("https://i.imgur.com/zFttbWq.jpg").unwrap();
        assert_eq!(
            inspect_hydra_image_response(&imgur, &imgur.origin(), 200, None, Some("*")),
            Ok(HydraImageResponseAction::Body)
        );
    }

    /// Optional release smoke test for the URL used by the upstream Hydra
    /// gallery sketch. Deterministic unit tests cover policy semantics;
    /// this one catches an external host changing its live CORS headers.
    #[cfg(feature = "hydra")]
    #[test]
    #[ignore = "requires internet access to i.imgur.com"]
    fn hydra_exact_gallery_imgur_image_fetches_with_cors() {
        let mut access = crate::samples::ScoreSampleAccess::denied();
        access.permit_public_cors_origins();
        let bytes = fetch_image_with_budget(
            "https://i.imgur.com/zFttbWq.jpg",
            &test_budget(Duration::from_secs(15)),
            &access,
        )
        .expect("the exact gallery image should allow anonymous Strudel CORS reads");
        assert!(bytes.starts_with(&[0xff, 0xd8]), "expected a JPEG response");
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_image_diagnostics_do_not_reflect_secret_urls() {
        let library_error =
            "GET https://user:pass@images.example/a.png?token=secret#private: failed";
        let safe = safe_hydra_request_error(library_error);
        assert_eq!(safe, "Hydra image request failed");
        for secret in [
            "user",
            "pass",
            "token",
            "secret",
            "private",
            "images.example",
        ] {
            assert!(!safe.contains(secret), "leaked {secret}: {safe}");
        }
    }

    /// Answer one loopback request with `response`, handing back the request's
    /// header lines.
    #[cfg(feature = "hydra")]
    fn answer_once(response: &'static str) -> (SocketAddr, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("request");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = std::io::BufReader::new(stream);
            let mut lines = Vec::new();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && !line.trim().is_empty() {
                lines.push(line.trim_end().to_owned());
                line.clear();
            }
            writer.write_all(response.as_bytes()).expect("response");
            lines
        });
        (address, server)
    }

    /// One Hydra image hop to `address`, its query carrying a token no error may
    /// repeat.
    #[cfg(feature = "hydra")]
    fn hydra_image_hop_to(address: SocketAddr) -> Result<ureq::Response, String> {
        let url = Url::parse(&format!("http://{address}/a.png?token=secret")).expect("URL");
        hydra_image_hop(&url, &test_budget(Duration::from_secs(5)), |_, _| {
            Ok(vec![address])
        })
    }

    /// A Hydra image request accepts exactly the formats the decoder reads, so a
    /// host that negotiates content answers with an image.
    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_image_requests_accept_the_formats_the_decoder_reads() {
        let (address, server) = answer_once(
            "HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Type: image/png\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let response = hydra_image_hop_to(address).expect("the image");
        let request = server.join().expect("server thread");
        let accept = request
            .iter()
            .filter(|line| line.to_ascii_lowercase().starts_with("accept:"))
            .collect::<Vec<_>>();
        assert_eq!(
            accept,
            ["Accept: image/gif,image/jpeg,image/png,image/webp"]
        );
        assert_eq!(refuse_web_page(response.content_type()), Ok(()));
    }

    /// A final response that is a web page is refused as one, rather than handed
    /// to the decoder.
    #[cfg(feature = "hydra")]
    #[test]
    fn a_hydra_image_url_that_answers_with_a_web_page_says_so() {
        let (address, server) = answer_once(
            "HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\n\
         Content-Type: Text/HTML; charset=utf-8\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n",
        );
        let response = hydra_image_hop_to(address).expect("the page");
        server.join().expect("server thread");
        assert_eq!(
            refuse_web_page(response.content_type()),
            Err(
                "Hydra image URL returned a web page, not an image; use the direct image link"
                    .to_owned()
            )
        );
    }

    /// An HTTP error status is reported by its code, never with the URL.
    #[cfg(feature = "hydra")]
    #[test]
    fn a_hydra_image_error_status_is_reported_by_its_code() {
        let (address, server) =
            answer_once("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let error = hydra_image_hop_to(address).map(|_| ()).expect_err("a 404");
        server.join().expect("server thread");
        assert_eq!(error, "Hydra image request returned HTTP 404");
    }

    /// An exact-origin grant controls the URL TEXT. It says nothing about the
    /// machine that text reaches, and DNS or `/etc/hosts` decides that. So a
    /// granted `http://samples.example` must not become a request to a local
    /// service the moment a name points inward.
    #[test]
    fn loopback_is_reachable_only_when_the_requested_host_named_it() {
        for named in [
            "localhost:5432",
            "LOCALHOST:80",
            "api.localhost:8080",
            "127.0.0.1:5432",
            "[::1]:5432",
            "[::ffff:127.0.0.1]:5432",
        ] {
            assert!(host_names_loopback(named), "{named} names loopback");
        }
        for public in [
            "samples.example:5432",
            "localhost.example.com:80",
            "8.8.8.8:80",
            "[2606:4700::1111]:443",
            "notlocalhost:80",
        ] {
            assert!(
                !host_names_loopback(public),
                "{public} does not name loopback"
            );
        }

        // The pairing that matters: same answer, different question.
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(addr_allowed(
            loopback,
            host_names_loopback("localhost:5432")
        ));
        assert!(!addr_allowed(
            loopback,
            host_names_loopback("samples.example:5432")
        ));

        // A name that claims loopback may reach nothing else, so a `localhost`
        // entry pointing outward is refused too.
        let public: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(!addr_allowed(public, true));

        // A mixed answer fails closed because the resolver refuses on the
        // FIRST unacceptable address; assert both halves of such a pair.
        assert!(!addr_allowed(public, true));
        assert!(!addr_allowed(loopback, false));

        // Deprecated site-local space is not public either.
        assert!(!addr_allowed("fec0::1".parse().unwrap(), false));
    }

    #[test]
    fn public_and_loopback_addresses_are_allowed() {
        for ok in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            assert!(
                addr_allowed(ok.parse().unwrap(), false),
                "{ok} must stay reachable"
            );
        }
        // Loopback is reachable only when loopback is what was asked for.
        for loopback in ["127.0.0.1", "127.255.255.254", "::1"] {
            let ip: IpAddr = loopback.parse().unwrap();
            assert!(
                addr_allowed(ip, true),
                "{loopback} must be reachable when the host named loopback"
            );
            assert!(
                !addr_allowed(ip, false),
                "{loopback} must NOT be reachable for a public-looking host"
            );
        }
    }

    #[test]
    fn non_public_addresses_are_refused() {
        for bad in [
            "10.1.2.3",        // RFC1918
            "172.16.0.1",      // RFC1918 low edge
            "172.31.255.255",  // RFC1918 high edge
            "192.168.1.1",     // RFC1918
            "169.254.169.254", // link-local cloud metadata
            "100.64.0.1",      // CGNAT
            "198.18.0.5",      // benchmarking
            "224.0.0.1",       // multicast
            "240.0.0.1",       // reserved
            "0.0.0.0",         // unspecified
            "::",              // unspecified v6
            "fd12::1",         // unique-local v6
            "fe80::1",         // link-local v6
            "ff02::1",         // multicast v6
            "::ffff:10.0.0.1", // v4-mapped private
            "::ffff:169.254.169.254",
            "::ffff:0:a00:1",                       // v4-translated private
            "::ffff:0:a9fe:a9fe",                   // v4-translated cloud metadata
            "2001:0:4136:e378:8000:63bf:3fff:fdd2", // Teredo
            "2001:4860:0:0:0:5efe:a00:1",           // ISATAP interface ID
            "2001:4860:0:0:200:5efe:5db8:d822",     // ISATAP public tail is refused too
        ] {
            assert!(
                !addr_allowed(bad.parse::<IpAddr>().unwrap(), false),
                "{bad} must be refused"
            );
        }
        assert!(
            ipv4_allowed(Ipv4Addr::new(172, 32, 0, 1)),
            "172.32.0.0/12 sits outside RFC1918 and stays public"
        );
    }

    /// Several IPv6 forms carry an IPv4 address that a gateway translates
    /// back. The policy judges the embedded address: on an IPv6-only network
    /// `64:ff9b::a00:1` reaches `10.0.0.1` through a NAT64 gateway.
    #[test]
    fn ipv6_forms_carrying_an_ipv4_address_are_judged_by_that_address() {
        for refused in [
            "::ffff:10.0.0.1",                      // mapped
            "::ffff:169.254.169.254",               // mapped cloud metadata
            "::ffff:0:a00:1",                       // translated private
            "::ffff:0:a9fe:a9fe",                   // translated cloud metadata
            "::10.0.0.1",                           // deprecated compatible form
            "64:ff9b::a00:1",                       // NAT64 well-known prefix, RFC 6052
            "64:ff9b::a9fe:a9fe",                   // NAT64 pointing at cloud metadata
            "2002:a00:1::",                         // 6to4 carrying 10.0.0.1
            "64:ff9b:1::a00:1",                     // RFC 8215 local-use NAT64, refused whole
            "2001:0:4136:e378:8000:63bf:3fff:fdd2", // Teredo, refused whole
            "2001:4860:0:0:0:5efe:a00:1",           // ISATAP, refused whole
        ] {
            let ip: IpAddr = refused.parse().expect("parses");
            assert!(
                !addr_allowed(ip, false),
                "{refused} must not be treated as public"
            );
        }
        // The same shapes carrying a reachable address stay reachable, or this
        // would refuse legitimate NAT64 and 6to4 networks outright.
        for allowed in [
            "::ffff:93.184.216.34",
            "::ffff:0:5db8:d822",
            "64:ff9b::5db8:d822",
            "2002:5db8:d822::",
        ] {
            let ip: IpAddr = allowed.parse().expect("parses");
            assert!(
                addr_allowed(ip, false),
                "{allowed} carries a public address"
            );
        }
    }

    #[test]
    fn scores_may_name_web_resources_only() {
        assert!(remote_url_allowed("https://example.com/kick.wav").is_ok());
        assert!(remote_url_allowed("HTTPS://EXAMPLE.com/kick.wav").is_ok());
        assert!(remote_url_allowed("http://localhost:5432").is_ok());
        assert!(remote_url_allowed("http://127.0.0.1:5432/kit/strudel.json").is_ok());
    }

    #[test]
    fn wire_identity_matches_the_strudel_filename_encoding() {
        let literal =
            wire_url("HTTPS://EXAMPLE.com:443/kit/take#2.wav?q=//").expect("literal hash filename");
        let encoded =
            wire_url("https://example.com/kit/take%232.wav?q=//").expect("encoded hash filename");

        assert_eq!(literal, encoded);
        assert_eq!(
            literal.as_str(),
            "https://example.com/kit/take%232.wav?q=//"
        );
        assert_ne!(
            literal,
            wire_url("https://example.com/kit/take.wav?q=//").expect("different filename")
        );
    }

    #[test]
    fn one_hop_netloc_preserves_ipv6_authority_syntax() {
        let url = Url::parse("http://[::1]/manifest.json").expect("IPv6 URL");
        assert_eq!(url_netloc(&url).expect("network location"), "[::1]:80");
    }

    #[test]
    fn scores_may_not_name_other_kinds_of_resource() {
        for refused in [
            "file:///etc/passwd",
            "file:///sample-root/kit/kick.wav",
            "data:text/plain,hello",
            "ftp://example.com/kick.wav",
            "/etc/passwd",
            "example.com/kick.wav",
            "javascript:alert(1)",
            "file:/etc/passwd",
        ] {
            assert!(
                remote_url_allowed(refused).is_err(),
                "{refused} must be refused from a score"
            );
        }
        // A scheme alone is not enough; the host must be there too.
        assert!(remote_url_allowed("https:///no-host.wav").is_err());
        assert!(remote_url_allowed("https://user:@example.com/kick.wav").is_ok());
    }

    /// A file name may hold what a URL cannot carry as written: a `"` for a
    /// cymbal's size in inches, a control character, a tab. None of it is
    /// refused. The parser percent-encodes it, or drops a tab or newline, and
    /// the parsed address is the one fetched (and the one a score's bank
    /// registers; a trusted bank keeps its text and is parsed when fetched), so
    /// a bank served by a sample server with `Crash 18".wav` in it loads as it
    /// does in a browser. `&` and `%` stay as written.
    #[test]
    fn a_quote_or_control_character_in_a_file_name_is_encoded_not_refused() {
        for (written, parsed) in [
            (
                "https://host/kit/Crash 18\".wav",
                "https://host/kit/Crash%2018%22.wav",
            ),
            ("https://host/kit/a\u{1b}.wav", "https://host/kit/a%1B.wav"),
            ("https://host/kit/\u{7f}x.wav", "https://host/kit/%7Fx.wav"),
            (
                "https://host/kit/a.wav?take=\"2\"&y=%41",
                "https://host/kit/a.wav?take=%222%22&y=%41",
            ),
            ("https://host/kit/\nbd.wav", "https://host/kit/bd.wav"),
            ("https://host/kit\t/bd.wav", "https://host/kit/bd.wav"),
            ("https://ho\rst/bd.wav", "https://host/bd.wav"),
        ] {
            assert_eq!(
                wire_url(written).expect(written).as_str(),
                parsed,
                "{written:?}"
            );
            assert!(remote_url_allowed(written).is_ok(), "{written:?}");
        }
    }

    /// The one place parsing makes a quote rather than encoding one is a host,
    /// which the parser percent-decodes: `a%22b.example` holds a `"` once
    /// parsed. No real host does, and the address outlives the fetch, so that
    /// is refused, and says so.
    #[test]
    fn a_quote_decoded_into_a_host_is_refused() {
        let refusal = wire_url("https://a%22b.example/bd.wav").expect_err("a quote once parsed");
        assert!(refusal.contains("holds '\"' once parsed"), "{refusal}");
        assert!(remote_url_allowed("https://a%22b.example/bd.wav").is_err());
    }

    /// A `#` is part of a file or folder name, never the start of a fragment:
    /// bank entries hold real names like `Kick #2.wav`, and the folder a reveal
    /// opens is named the same way.
    #[test]
    fn a_hash_is_part_of_the_name() {
        assert_eq!(
            web_address("https://host/kit #2/Kick #2.wav")
                .unwrap()
                .as_str(),
            "https://host/kit%20%232/Kick%20%232.wav"
        );
        assert_eq!(
            web_address("https://host/kit #2/").unwrap().fragment(),
            None
        );
    }

    /// `web_address` says what is wrong with an address in its own words; the
    /// fetch names it as a sample source. The scheme is read from the text, so
    /// `javascript:` is named as what it is, and an http address that is not
    /// written `https://host` is refused rather than guessed at.
    #[test]
    fn a_refused_address_says_what_is_wrong_and_the_fetch_names_its_source() {
        for (written, says) in [
            (
                "javascript:alert(1)",
                r#""javascript:alert(1)" is a javascript: address"#,
            ),
            ("calc.exe", r#""calc.exe" names no scheme"#),
            (
                "https:evil.example",
                r#""https:evil.example" is not written https://host"#,
            ),
            ("https:///kit.wav", r#""https:///kit.wav" names no host"#),
            (
                "https://a%01b.example/",
                r#""https://a%01b.example/" is not a valid URL"#,
            ),
        ] {
            let refusal = web_address(written).expect_err(written);
            assert!(refusal.starts_with(says), "{refusal}");
            assert_eq!(
                wire_url(written).expect_err(written),
                format!("sample source {refusal}")
            );
        }
    }

    /// Whitespace around a source is not part of it: a `samples()` source
    /// written in a multi-line template literal ends in a newline. Leading
    /// whitespace is trimmed too, before the scheme is read from the text.
    #[test]
    fn whitespace_around_a_source_is_trimmed() {
        for written in [
            "https://host/strudel.json\n",
            "https://host/strudel.json\r\n",
            "\thttps://host/strudel.json ",
        ] {
            assert_eq!(
                wire_url(written).expect(written).as_str(),
                "https://host/strudel.json",
                "{written:?}"
            );
            assert!(remote_url_allowed(written).is_ok(), "{written:?}");
        }
    }

    #[test]
    fn the_resolver_refuses_a_private_answer_before_any_connection() {
        let resolver = guarded_resolver();
        assert!(resolver.resolve("127.0.0.1:80").is_ok());
        assert_eq!(
            resolver.resolve("169.254.169.254:80").unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            resolver.resolve("192.168.0.14:443").unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        // Numeric spellings of a private address reach the same refusal,
        // because the check runs on the resolved address, not on the text.
        assert_eq!(
            resolver
                .resolve("[::ffff:10.9.9.9]:443")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn the_resolver_still_serves_the_local_sample_server_path() {
        // `localhost` resolves to loopback on ordinary systems; an explicit
        // grant for the documented local sample server depends on that.
        let resolver = guarded_resolver();
        let addrs = resolver
            .resolve("localhost:5432")
            .expect("loopback resolves");
        assert!(!addrs.is_empty());
        assert!(
            addrs.iter().all(|addr| addr.ip().is_loopback()),
            "unexpected non-loopback answer for localhost: {addrs:?}"
        );
    }

    #[test]
    fn a_loopback_fetch_round_trips_through_the_guarded_agent() {
        // The compatibility path in product form: the agent must still complete
        // a real GET against this machine.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let (stream, _) = listener.accept().expect("accept");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("request line");
            let request_line = line.trim_end().to_owned();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            writer
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nRIFF")
                .unwrap();
            request_line
        });
        let bytes = fetch(&format!("http://127.0.0.1:{port}/kick#2.wav")).expect("fetch");
        assert_eq!(bytes, b"RIFF");
        assert_eq!(
            server.join().expect("server thread"),
            "GET /kick%232.wav HTTP/1.1"
        );
    }

    #[test]
    fn an_ipv6_loopback_manifest_round_trips_when_available() {
        let Ok(listener) = std::net::TcpListener::bind("[::1]:0") else {
            return;
        };
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};

            let (stream, _) = listener.accept().expect("request");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            writer
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .expect("response");
        });

        let bytes = fetch_manifest_with_budget(
            &format!("http://{address}/manifest.json"),
            &test_budget(Duration::from_secs(1)),
        )
        .expect("IPv6 manifest fetch");
        server.join().expect("server thread");
        assert_eq!(bytes, b"{}");
    }

    #[test]
    fn a_redirect_fragment_is_not_sent_as_part_of_the_next_request_target() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};

            let mut request_lines = Vec::new();
            for response in [
                b"HTTP/1.1 302 Found\r\nLocation: /payload#client-only\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .as_slice(),
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                    .as_slice(),
            ] {
                let (stream, _) = listener.accept().expect("request");
                let mut writer = stream.try_clone().expect("clone");
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("request line");
                request_lines.push(line.trim_end().to_owned());
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                writer.write_all(response).expect("response");
            }
            request_lines
        });

        let bytes = fetch_manifest_with_budget(
            &format!("{origin}/redirect"),
            &test_budget(Duration::from_secs(1)),
        )
        .expect("same-origin redirect");
        let request_lines = server.join().expect("server thread");
        assert_eq!(bytes, b"{}");
        assert_eq!(
            request_lines,
            [
                "GET /redirect HTTP/1.1".to_owned(),
                "GET /payload HTTP/1.1".to_owned(),
            ],
            "a Location fragment crossed the HTTP wire"
        );
    }

    /// The module header promises a public URL that redirects inward "dies at
    /// the second hop". The grant names a public host, a loopback listener
    /// standing in for its address, and that server answers `Location:
    /// http://127.0.0.1:PORT/...`. The hop's own text names loopback; only the
    /// grant may, so the real redirect loop must refuse the hop before any
    /// connection reaches the loopback listener.
    #[test]
    fn a_server_chosen_loopback_hop_cannot_grant_itself_loopback_access() {
        let target_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let target = target_listener.local_addr().expect("listener address");

        let origin_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = origin_listener.local_addr().expect("listener address");
        let origin_server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let (stream, _) = origin_listener.accept().expect("request");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            write!(
                writer,
                "HTTP/1.1 302 Found\r\nLocation: http://{target}/admin\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("redirect");
        });

        let granted = format!("samples.example:{}", origin.port());
        let error = fetch_following_redirects_via(
            &format!("http://{granted}/manifest.json"),
            &test_budget(Duration::from_secs(5)),
            1024,
            "manifest",
            |netloc, hop_budget, loopback_named| {
                if netloc.eq_ignore_ascii_case(&granted) {
                    // What public DNS would answer for the granted host.
                    Ok(vec![origin])
                } else {
                    resolve_guarded(netloc, hop_budget, loopback_named)
                }
            },
        )
        .expect_err("a redirect hop must not grant itself loopback access");
        origin_server.join().expect("origin server thread");

        // Inspect the real socket before asserting on prose: the machine
        // boundary is that no connection ever reached the loopback listener.
        target_listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let connected = match target_listener.accept() {
            Ok((stream, _)) => {
                drop(stream);
                true
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
            Err(error) => panic!("inspect listener: {error}"),
        };
        assert!(
            !connected,
            "the self-granted hop reached the loopback listener"
        );
        assert!(error.contains("not a public address"), "{error}");
    }

    /// A grant that names loopback may reach nothing else, redirects included:
    /// a local server whose `Location:` leaves loopback is refused at that hop,
    /// before any connection, and the refusal names the half of the rule that
    /// failed. 192.0.2.1 is public to the address policy, so judging the hop by
    /// its own text would try to connect to it instead.
    #[test]
    fn a_loopback_grant_cannot_be_redirected_out_of_loopback() {
        let origin_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = origin_listener.local_addr().expect("listener address");
        let origin_server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let (stream, _) = origin_listener.accept().expect("request");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            writer
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://192.0.2.1:9/manifest.json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("redirect");
        });

        let error = fetch_manifest_with_budget(
            &format!("http://{origin}/manifest.json"),
            &test_budget(Duration::from_secs(1)),
        )
        .expect_err("a loopback grant must not follow a redirect out of loopback");
        origin_server.join().expect("origin server thread");
        assert!(
            error.contains("192.0.2.1:9 is not loopback, and the granted URL named loopback"),
            "{error}"
        );
    }

    /// A grant that names loopback still follows redirects, as long as every
    /// hop stays inside loopback. An operator's local sample server keeps
    /// redirect support.
    #[test]
    fn a_granted_loopback_url_still_follows_redirects_within_loopback() {
        let target_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let target = target_listener.local_addr().expect("listener address");
        let target_server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let (stream, _) = target_listener.accept().expect("request");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            writer
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .expect("response");
        });

        let origin_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = origin_listener.local_addr().expect("listener address");
        let origin_server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let (stream, _) = origin_listener.accept().expect("request");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            write!(
                writer,
                "HTTP/1.1 302 Found\r\nLocation: http://{target}/manifest.json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("redirect");
        });

        let bytes = fetch_manifest_with_budget(
            &format!("http://{origin}/manifest.json"),
            &test_budget(Duration::from_secs(5)),
        )
        .expect("a granted loopback URL may redirect within loopback");
        assert_eq!(bytes, b"{}");
        origin_server.join().expect("origin server thread");
        target_server.join().expect("target server thread");
    }

    #[test]
    fn dns_cannot_leave_tcp_a_fresh_copy_of_the_aggregate_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let target = listener.local_addr().expect("listener address");
        let url = wire_url(&format!("http://{target}/manifest.json")).expect("URL");
        let budget = test_budget(Duration::from_millis(30));

        let error = single_hop_get_with_resolver(&url, &budget, None, None, |netloc, _| {
            std::thread::sleep(Duration::from_millis(70));
            guard_resolved_addresses(netloc, vec![target], host_names_loopback(netloc))
        })
        .map_err(String::from)
        .expect_err("DNS consumed the complete aggregate budget");

        // Inspect the real socket before asserting on prose. Computing the
        // connect timeout before the delayed resolver makes ureq connect here
        // with a fresh interval even though the aggregate deadline is spent.
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let connected = match listener.accept() {
            Ok((stream, _)) => {
                drop(stream);
                true
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
            Err(error) => panic!("inspect listener: {error}"),
        };
        assert!(error.contains("deadline exceeded"), "{error}");
        assert!(!connected, "TCP started after DNS exhausted the deadline");
    }

    #[test]
    fn partial_dns_time_is_subtracted_from_the_one_hop_timeout() {
        let target: SocketAddr = "127.0.0.1:9".parse().expect("socket address");
        let url = wire_url("http://127.0.0.1:9/manifest.json").expect("URL");
        let budget = test_budget(Duration::from_secs(1));
        let resolved_at = std::cell::Cell::new(None);

        let (_, remaining) = manifest_agent_with_resolver(&url, &budget, |netloc, _| {
            std::thread::sleep(Duration::from_millis(50));
            let result =
                guard_resolved_addresses(netloc, vec![target], host_names_loopback(netloc));
            resolved_at.set(Some(Instant::now()));
            result
        })
        .expect("partly consumed budget still builds the hop");

        let available_after_dns = budget
            .deadline
            .saturating_duration_since(resolved_at.get().expect("resolver completion"));
        assert!(
            remaining <= available_after_dns,
            "one-hop timeout was captured before DNS: {remaining:?} > {available_after_dns:?}"
        );
    }

    #[test]
    fn remote_manifests_have_a_dedicated_four_mib_response_limit() {
        assert_eq!(MAX_REMOTE_MANIFEST_BYTES, 4 * 1024 * 1024);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let (stream, _) = listener.accept().expect("accept");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            write!(
                writer,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_REMOTE_MANIFEST_BYTES + 1
            )
            .expect("response");
        });
        let error = fetch_manifest_with_budget(
            &format!("http://127.0.0.1:{port}/strudel.json"),
            &test_budget(Duration::from_secs(1)),
        )
        .expect_err("oversized manifest response");
        assert!(error.contains("sample manifest size limit"), "{error}");
        server.join().expect("server thread");
    }

    #[test]
    fn redirects_share_the_original_absolute_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            for hop in 0..2 {
                let (stream, _) = listener.accept().expect("accept");
                let mut writer = stream.try_clone().expect("clone");
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                if hop == 0 {
                    writer
                        .write_all(
                            b"HTTP/1.1 302 Found\r\nLocation: /slow\r\nContent-Length: 0\r\n\r\n",
                        )
                        .unwrap();
                } else {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        });
        let budget = test_budget(Duration::from_millis(70));
        let started = Instant::now();
        let error = fetch_manifest_with_budget(&format!("{origin}/first"), &budget)
            .expect_err("second hop must not receive a fresh timeout");
        assert!(
            error.contains("timed out") || error.contains("deadline"),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_millis(250));
        server.join().unwrap();
    }

    #[test]
    fn a_public_looking_name_that_resolves_private_is_refused_at_fetch() {
        // Not a network test: the refusal fires during resolution of a literal
        // private address, before any connection could exist.
        let error = fetch("http://10.255.0.7/kick.wav").unwrap_err();
        assert!(error.contains("not a public address"), "{error}");
    }

    #[test]
    fn a_stalled_dns_lookup_cannot_outlive_the_callers_deadline() {
        let pool = DnsPool::spawn(
            1,
            1,
            Arc::new(|_| {
                std::thread::sleep(Duration::from_millis(500));
                Ok(vec!["127.0.0.1:80".parse().unwrap()])
            }),
        )
        .expect("DNS pool");
        let started = Instant::now();
        let error = pool
            .resolve("stalled.test:80", &test_budget(Duration::from_millis(30)))
            .expect_err("deadline");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "the caller waited for the system resolver: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn cancellation_interrupts_a_waiting_dns_caller() {
        let pool = DnsPool::spawn(
            1,
            1,
            Arc::new(|_| {
                std::thread::sleep(Duration::from_millis(500));
                Ok(vec!["127.0.0.1:80".parse().unwrap()])
            }),
        )
        .expect("DNS pool");
        let cancelled = Arc::new(AtomicBool::new(false));
        let budget = FetchBudget::until(
            Instant::now() + Duration::from_secs(1),
            Arc::clone(&cancelled),
        );
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            cancelled.store(true, Ordering::Release);
        });
        let error = pool
            .resolve("stalled.test:80", &budget)
            .expect_err("cancelled");
        trigger.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn saturated_dns_work_waits_for_capacity_within_the_callers_budget() {
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let pool = DnsPool::spawn(
            1,
            1,
            Arc::new(move |netloc| {
                if netloc.starts_with("first.") {
                    let _ = entered.send(());
                    let _ = release_rx.lock().unwrap().recv();
                }
                Ok(vec!["127.0.0.1:80".parse().unwrap()])
            }),
        )
        .expect("DNS pool");

        let (first_reply, _first_result) = mpsc::sync_channel(1);
        pool.jobs
            .try_send(DnsJob {
                netloc: "first.test:80".into(),
                budget: test_budget(Duration::from_secs(1)),
                reply: first_reply,
            })
            .expect("first job");
        entered_rx.recv().expect("worker entered resolver");
        let (queued_reply, _queued_result) = mpsc::sync_channel(1);
        pool.jobs
            .try_send(DnsJob {
                netloc: "queued.test:80".into(),
                budget: test_budget(Duration::from_secs(1)),
                reply: queued_reply,
            })
            .expect("queued job");

        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            release.send(()).unwrap();
        });
        let started = Instant::now();
        let addrs = pool
            .resolve("waiting.test:80", &test_budget(Duration::from_secs(1)))
            .expect("capacity becomes available within the budget");
        trigger.join().unwrap();
        assert_eq!(addrs, vec!["127.0.0.1:80".parse().unwrap()]);
        assert!(started.elapsed() >= Duration::from_millis(20));
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}

#[cfg(test)]
mod github_hint_tests {
    use super::github_hint;

    /// A 404 identifies the guessed `samples` repository when `github:user` omits
    /// the repository name. Explicit repository names need no such hint.
    #[test]
    fn a_guessed_repository_says_so_when_it_is_not_there() {
        let guessed = "https://raw.githubusercontent.com/yaxu/samples/main/strudel.json";
        let hint = github_hint(guessed, 404);
        assert!(hint.contains("github:user/repo"), "{hint}");
        assert!(hint.contains("called `samples`"), "{hint}");

        // A repository that was named, and one that answered: nothing to
        // explain, so nothing is said.
        assert!(
            github_hint(
                "https://raw.githubusercontent.com/yaxu/clean-breaks/main/strudel.json",
                404
            )
            .is_empty()
        );
        assert!(github_hint(guessed, 500).is_empty());
        assert!(github_hint("https://example.com/samples/strudel.json", 404).is_empty());
    }
}

#[cfg(test)]
mod special_purpose_tests {
    use super::*;

    #[test]
    fn special_purpose_addresses_cannot_become_score_fetch_destinations() {
        // The shared fetch policy refuses both edges of each restricted block.
        for refused in [
            "192.0.2.0",
            "192.0.2.255", // TEST-NET-1
            "192.88.99.0",
            "192.88.99.255", // deprecated 6to4 relay anycast
            "198.51.100.0",
            "198.51.100.255", // TEST-NET-2
            "203.0.113.0",
            "203.0.113.255", // TEST-NET-3
            "100::",
            "100::ffff:ffff:ffff:ffff", // discard-only /64
            "2001:2::",
            "2001:2:0:ffff:ffff:ffff:ffff:ffff", // benchmark /48
            "2001:10::",
            "2001:1f:ffff:ffff:ffff:ffff:ffff:ffff", // ORCHID /28
            "2001:20::",
            "2001:2f:ffff:ffff:ffff:ffff:ffff:ffff", // ORCHIDv2 /28
            "2001:db8::",
            "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff", // docs /32
            "3fff::",
            "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff", // docs /20
            "5f00::",
            "5f00:ffff:ffff:ffff:ffff:ffff:ffff:ffff", // SRv6 SIDs /16
            "::ffff:192.0.2.1",
            "64:ff9b::c000:201", // embedded TEST-NET-1
        ] {
            let ip = refused.parse::<IpAddr>().expect("valid IP literal");
            assert!(!addr_allowed(ip, false), "{refused} must be refused");
        }

        for public in [
            "192.0.3.1",
            "198.51.101.1",
            "203.0.114.1",
            "2001:30::1",
            "3fff:1000::1",
            "5f01::1",
        ] {
            let ip = public.parse::<IpAddr>().expect("valid IP literal");
            assert!(addr_allowed(ip, false), "{public} is outside these blocks");
        }
    }
}

#[cfg(all(test, feature = "hydra"))]
mod hydra_access_tests {
    use super::*;
    use crate::samples::ScoreSampleAccess;

    #[test]
    fn hydra_image_fetch_requires_the_sessions_sample_origin_grant() {
        let url = "https://images.example/texture.png?private=redacted";
        let budget = FetchBudget::until(
            Instant::now() + Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
        );
        let denied = ScoreSampleAccess::denied();
        assert_eq!(
            fetch_image_with_budget(url, &budget, &denied),
            Err("Hydra image URL is outside the permitted sample origins".to_owned()),
            "the default policy must refuse before DNS or HTTP"
        );

        let mut exact = ScoreSampleAccess::denied();
        exact.permit_origin("https://images.example").unwrap();
        assert!(exact.approve_hydra_image_url(url).is_ok());
        assert!(
            exact
                .approve_hydra_image_url("https://different.example/texture.png")
                .is_err()
        );

        let mut public = ScoreSampleAccess::denied();
        public.permit_public_cors_origins();
        assert!(public.approve_hydra_image_url(url).is_ok());
    }
}
