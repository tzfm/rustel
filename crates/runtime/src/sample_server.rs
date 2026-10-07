//! Serve a folder of samples to a browser, the way `@strudel/sampler` does.
//!
//! Our own engine reads local sample folders directly. A browser cannot, so
//! this server exposes the sampler-compatible bank map and audio-file routes.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::samples::{MAX_SAMPLE_MANIFEST_BYTES, MAX_SAMPLE_SCAN_WORK_BYTES};
use crate::samples::{SampleAudioKind, SampleFolderScanError, SampleScanLimits, sample_audio_kind};

const DEFAULT_BROWSER_ORIGIN: &str = "https://strudel.cc";
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_IN_FLIGHT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_HANDLERS: usize = 64;
const FILE_CHUNK_BYTES: usize = 64 * 1024;
const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;
const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;
const MAX_REQUEST_HEADER_BYTES: usize = 32 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

static IN_FLIGHT_BYTES: InFlightBudget = InFlightBudget::new(MAX_IN_FLIGHT_BYTES);
static HANDLERS: HandlerSlots = HandlerSlots::new(MAX_HANDLERS);

/// Serve `root` until the process is stopped.
///
/// Browser requests are accepted from `https://strudel.cc`. Command-line
/// clients without an `Origin` header can also read the server, but receive no
/// cross-origin grant.
pub fn serve(root: &Path, host: &str, port: u16) -> Result<(), String> {
    serve_until(root, host, port, &|| false)
}

/// [`serve`] that stops when `should_stop` says so.
pub fn serve_until(
    root: &Path,
    host: &str,
    port: u16,
    should_stop: &dyn Fn() -> bool,
) -> Result<(), String> {
    serve_until_with_origins(root, host, port, &[], should_stop)
}

/// Serve with an explicit browser-origin policy.
///
/// An empty list uses `https://strudel.cc`. A non-empty list replaces that
/// default, so every permitted browser origin must be named explicitly.
pub fn serve_until_with_origins(
    root: &Path,
    host: &str,
    port: u16,
    allowed_origins: &[String],
    should_stop: &dyn Fn() -> bool,
) -> Result<(), String> {
    let origins = Arc::new(OriginPolicy::new(allowed_origins)?);
    let root = Arc::new(ServedRoot::open(root)?);

    // Fail before listening: an empty folder is almost always the wrong
    // folder, and the artist is standing at this terminal to correct it.
    let banks =
        server_banks(&root, SampleScanLimits::DEFAULT).map_err(|error| error.to_string())?;
    let listener = TcpListener::bind((host, port))
        .map_err(|error| format!("cannot listen on {host}:{port}: {error}"))?;
    let actual_port = listener
        .local_addr()
        .map_err(|error| format!("cannot inspect the listener: {error}"))?
        .port();
    let hosts = Arc::new(HostPolicy::new(host, actual_port)?);
    let advertised_url = advertised_url(host, actual_port);

    eprintln!(
        "{}",
        serde_json::json!({
            "sample_server": {
                "folder": root.path.display().to_string(),
                "url": advertised_url.clone(),
                "banks": banks.len(),
                "files": banks.values().map(Vec::len).sum::<usize>(),
                "message": format!(
                    "in a browser score: samples('{advertised_url}'); for native rustel, bind this server to loopback and pass its URL with --allow-sample-origin"
                ),
            }
        })
    );

    listener
        .set_nonblocking(true)
        .map_err(|error| format!("cannot poll the listener: {error}"))?;
    loop {
        if should_stop() {
            eprintln!(
                "{}",
                serde_json::json!({ "sample_server": { "status": "stopped" } })
            );
            return Ok(());
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(handler) = HANDLERS.acquire() else {
                    // Refuse before creating another thread. A client already
                    // occupying every bounded handler does not get to make the
                    // refusal path itself unbounded.
                    drop(stream);
                    continue;
                };
                if stream.set_nonblocking(false).is_err() {
                    drop(stream);
                    continue;
                }
                let root = Arc::clone(&root);
                let origins = Arc::clone(&origins);
                let hosts = Arc::clone(&hosts);
                let _ = std::thread::Builder::new()
                    .name("rustel-sample-server".to_owned())
                    .spawn(move || {
                        let _handler = handler;
                        let _ = handle(
                            stream,
                            &root,
                            &origins,
                            &hosts,
                            &IN_FLIGHT_BYTES,
                            ServerLimits::DEFAULT,
                        );
                    });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => continue,
        }
    }
}

fn advertised_url(host: &str, port: u16) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    }
}

#[derive(Clone, Debug)]
struct OriginPolicy {
    allowed: BTreeSet<String>,
}

impl OriginPolicy {
    fn new(configured: &[String]) -> Result<Self, String> {
        let configured: Vec<&str> = if configured.is_empty() {
            vec![DEFAULT_BROWSER_ORIGIN]
        } else {
            configured.iter().map(String::as_str).collect()
        };
        let mut allowed = BTreeSet::new();
        for origin in configured {
            let normalized = normalize_origin(origin)
                .map_err(|error| format!("invalid --allow-origin {origin:?}: {error}"))?;
            allowed.insert(normalized);
        }
        Ok(Self { allowed })
    }

    fn authorize(&self, supplied: Option<&str>) -> Result<Option<String>, ()> {
        let Some(supplied) = supplied else {
            return Ok(None);
        };
        let normalized = normalize_origin(supplied).map_err(|_| ())?;
        self.allowed
            .contains(&normalized)
            .then_some(Some(normalized))
            .ok_or(())
    }
}

fn normalize_origin(origin: &str) -> Result<String, &'static str> {
    let parsed = url::Url::parse(origin.trim()).map_err(|_| "expected an absolute URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("only http and https origins are supported");
    }
    if parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("expected only a scheme, host, and optional port");
    }
    let normalized = parsed.origin().ascii_serialization();
    (normalized != "null")
        .then_some(normalized)
        .ok_or("origin is not serializable")
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AuthorityHost {
    Name(String),
    Ip(IpAddr),
}

#[derive(Clone, Debug)]
struct HostPolicy {
    configured: AuthorityHost,
    port: u16,
}

impl HostPolicy {
    fn new(configured: &str, port: u16) -> Result<Self, String> {
        let configured = parse_configured_host(configured)
            .ok_or_else(|| format!("invalid sample-server host {configured:?}"))?;
        Ok(Self { configured, port })
    }

    fn authorize(&self, supplied: &str, local: std::net::SocketAddr) -> bool {
        let Some((supplied, port)) = parse_request_authority(supplied) else {
            return false;
        };
        if port != self.port || local.port() != self.port {
            return false;
        }
        match supplied {
            AuthorityHost::Ip(ip) => {
                ip_equivalent(ip, local.ip()) || self.configured == AuthorityHost::Ip(ip)
            }
            AuthorityHost::Name(name) => {
                self.configured == AuthorityHost::Name(name.clone())
                    || (ip_is_loopback(local.ip()) && is_localhost_name(&name))
            }
        }
    }
}

fn parse_configured_host(host: &str) -> Option<AuthorityHost> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(AuthorityHost::Ip(ip));
    }
    let parsed = url::Url::parse(&format!("http://{host}/")).ok()?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    match parsed.host()? {
        url::Host::Domain(name) => Some(AuthorityHost::Name(name.to_owned())),
        url::Host::Ipv4(ip) => Some(AuthorityHost::Ip(IpAddr::V4(ip))),
        url::Host::Ipv6(ip) => Some(AuthorityHost::Ip(IpAddr::V6(ip))),
    }
}

fn parse_request_authority(authority: &str) -> Option<(AuthorityHost, u16)> {
    let parsed = url::Url::parse(&format!("http://{authority}/")).ok()?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    let host = match parsed.host()? {
        url::Host::Domain(name) => AuthorityHost::Name(name.to_owned()),
        url::Host::Ipv4(ip) => AuthorityHost::Ip(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => AuthorityHost::Ip(IpAddr::V6(ip)),
    };
    Some((host, parsed.port_or_known_default()?))
}

fn ip_equivalent(left: IpAddr, right: IpAddr) -> bool {
    normalize_ip(left) == normalize_ip(right)
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    }
}

fn ip_is_loopback(ip: IpAddr) -> bool {
    normalize_ip(ip).is_loopback()
}

fn is_localhost_name(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name);
    name == "localhost" || name.ends_with(".localhost")
}

#[derive(Debug)]
struct Request {
    method: String,
    target: String,
    host: String,
    origin: Option<String>,
    requested_method: Option<String>,
}

#[derive(Debug)]
enum RequestError {
    Io(std::io::Error),
    Malformed,
    TooLarge,
}

#[derive(Clone, Copy)]
struct ConnectionTimeouts {
    request: Duration,
    response: Duration,
}

impl ConnectionTimeouts {
    const DEFAULT: Self = Self {
        request: REQUEST_TIMEOUT,
        response: RESPONSE_TIMEOUT,
    };
}

#[derive(Clone, Copy)]
struct ServerLimits {
    scan: SampleScanLimits,
    file_bytes: u64,
    timeouts: ConnectionTimeouts,
}

impl ServerLimits {
    const DEFAULT: Self = Self {
        scan: SampleScanLimits::DEFAULT,
        file_bytes: MAX_FILE_BYTES,
        timeouts: ConnectionTimeouts::DEFAULT,
    };
}

#[derive(Clone, Copy)]
struct Deadline {
    at: Instant,
}

impl Deadline {
    fn after(duration: Duration) -> Self {
        Self {
            at: Instant::now()
                .checked_add(duration)
                .unwrap_or_else(Instant::now),
        }
    }

    fn remaining(self) -> std::io::Result<Duration> {
        self.at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::TimedOut))
    }

    fn install_read_timeout(self, stream: &TcpStream) -> std::io::Result<()> {
        stream.set_read_timeout(Some(self.remaining()?))
    }

    fn install_write_timeout(self, stream: &TcpStream) -> std::io::Result<()> {
        stream.set_write_timeout(Some(self.remaining()?))
    }
}

fn read_request(stream: &TcpStream, deadline: Deadline) -> Result<Request, RequestError> {
    let mut reader = BufReader::new(stream.try_clone().map_err(RequestError::Io)?);
    let request_line = read_bounded_line(&mut reader, MAX_REQUEST_LINE_BYTES, deadline)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or(RequestError::Malformed)?.to_owned();
    let target = parts.next().ok_or(RequestError::Malformed)?.to_owned();
    if parts.next() != Some("HTTP/1.1") || parts.next().is_some() {
        return Err(RequestError::Malformed);
    }

    let mut remaining = MAX_REQUEST_HEADER_BYTES;
    let mut host = None;
    let mut origin = None;
    let mut requested_method = None;
    loop {
        let limit = remaining.min(MAX_HEADER_LINE_BYTES);
        if limit == 0 {
            return Err(RequestError::TooLarge);
        }
        let line = read_bounded_line(&mut reader, limit, deadline)?;
        let read = line.len();
        remaining -= read;
        if line == "\r\n" || line == "\n" {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(RequestError::Malformed);
        };
        if name.is_empty()
            || name.trim() != name
            || !name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
            })
        {
            return Err(RequestError::Malformed);
        }
        let value = value.trim().to_owned();
        if name.eq_ignore_ascii_case("host") {
            if value.is_empty() || host.replace(value).is_some() {
                return Err(RequestError::Malformed);
            }
        } else if name.eq_ignore_ascii_case("origin") {
            if origin.replace(value).is_some() {
                return Err(RequestError::Malformed);
            }
        } else if name.eq_ignore_ascii_case("access-control-request-method")
            && requested_method.replace(value).is_some()
        {
            return Err(RequestError::Malformed);
        }
    }

    Ok(Request {
        method,
        target,
        host: host.ok_or(RequestError::Malformed)?,
        origin,
        requested_method,
    })
}

fn read_bounded_line(
    reader: &mut BufReader<TcpStream>,
    limit: usize,
    deadline: Deadline,
) -> Result<String, RequestError> {
    let mut line = Vec::new();
    loop {
        deadline
            .install_read_timeout(reader.get_ref())
            .map_err(RequestError::Io)?;
        let available = reader.fill_buf().map_err(RequestError::Io)?;
        if available.is_empty() {
            return Err(RequestError::Malformed);
        }
        let (read, complete) = match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => (index + 1, true),
            None => (available.len(), false),
        };
        let next = line.len().checked_add(read).ok_or(RequestError::TooLarge)?;
        if next > limit {
            return Err(RequestError::TooLarge);
        }
        line.try_reserve(read)
            .map_err(|_| RequestError::Io(std::io::Error::other("cannot allocate request line")))?;
        line.extend_from_slice(&available[..read]);
        reader.consume(read);
        if complete {
            return String::from_utf8(line).map_err(|_| RequestError::Malformed);
        }
    }
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

fn handle(
    mut stream: TcpStream,
    root: &ServedRoot,
    origins: &OriginPolicy,
    hosts: &HostPolicy,
    budget: &InFlightBudget,
    limits: ServerLimits,
) -> std::io::Result<()> {
    let request = match read_request(&stream, Deadline::after(limits.timeouts.request)) {
        Ok(request) => request,
        Err(RequestError::TooLarge) => {
            return respond_bytes(
                &mut stream,
                413,
                "text/plain",
                b"request too large",
                None,
                ResponseFlags::BODY,
                limits.timeouts.response,
            );
        }
        Err(RequestError::Io(error)) if is_timeout(&error) => {
            return respond_bytes(
                &mut stream,
                408,
                "text/plain",
                b"request timeout",
                None,
                ResponseFlags::BODY,
                limits.timeouts.response,
            );
        }
        Err(RequestError::Io(error)) => return Err(error),
        Err(RequestError::Malformed) => {
            return respond_bytes(
                &mut stream,
                400,
                "text/plain",
                b"bad request",
                None,
                ResponseFlags::BODY,
                limits.timeouts.response,
            );
        }
    };
    let local = stream.local_addr()?;
    if !hosts.authorize(&request.host, local) {
        return respond_bytes(
            &mut stream,
            403,
            "text/plain",
            b"host not allowed",
            None,
            ResponseFlags::new(request.method == "HEAD", false),
            limits.timeouts.response,
        );
    }
    let cors_origin = match origins.authorize(request.origin.as_deref()) {
        Ok(origin) => origin,
        Err(()) => {
            return respond_bytes(
                &mut stream,
                403,
                "text/plain",
                b"origin not allowed; restart serve-samples with --allow-origin <origin>",
                None,
                ResponseFlags::new(request.method == "HEAD", false),
                limits.timeouts.response,
            );
        }
    };
    let cors_origin = cors_origin.as_deref();

    if request.method == "OPTIONS" {
        if request
            .requested_method
            .as_deref()
            .is_some_and(|method| !matches!(method, "GET" | "HEAD"))
        {
            return respond_bytes(
                &mut stream,
                405,
                "text/plain",
                b"method not allowed",
                cors_origin,
                ResponseFlags::PREFLIGHT,
                limits.timeouts.response,
            );
        }
        return respond_bytes(
            &mut stream,
            204,
            "text/plain",
            b"",
            cors_origin,
            ResponseFlags::PREFLIGHT,
            limits.timeouts.response,
        );
    }

    let head_only = request.method == "HEAD";
    if request.method != "GET" && !head_only {
        return respond_bytes(
            &mut stream,
            405,
            "text/plain",
            b"method not allowed",
            cors_origin,
            ResponseFlags::BODY,
            limits.timeouts.response,
        );
    }

    let path = request.target.split('?').next().unwrap_or("/");
    if path == "/" {
        // A manifest is the one response that must be materialized to know
        // its Content-Length. Reserve both the scanner workspace and the wire
        // body before either allocates, then release the unused portion.
        let Some(reserved) = limits
            .scan
            .working_bytes
            .checked_add(limits.scan.manifest_bytes)
            .and_then(|bytes| u64::try_from(bytes).ok())
        else {
            return respond_bytes(
                &mut stream,
                503,
                "text/plain",
                b"server busy",
                cors_origin,
                ResponseFlags::new(head_only, false),
                limits.timeouts.response,
            );
        };
        let Some(mut manifest_permit) = budget.acquire(reserved) else {
            return respond_bytes(
                &mut stream,
                503,
                "text/plain",
                b"server busy",
                cors_origin,
                ResponseFlags::new(head_only, false),
                limits.timeouts.response,
            );
        };
        let body = match wire_manifest(root, limits.scan) {
            Ok(body) => body,
            Err(error) => {
                let status = if error.is_limit() { 413 } else { 500 };
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "sample_server": {
                            "error": "manifest unavailable",
                            "detail": error.to_string(),
                        }
                    })
                );
                return respond_bytes(
                    &mut stream,
                    status,
                    "text/plain",
                    b"sample manifest unavailable",
                    cors_origin,
                    ResponseFlags::new(head_only, false),
                    limits.timeouts.response,
                );
            }
        };
        let Ok(body_capacity) = u64::try_from(body.capacity()) else {
            return respond_bytes(
                &mut stream,
                503,
                "text/plain",
                b"server busy",
                cors_origin,
                ResponseFlags::new(head_only, false),
                limits.timeouts.response,
            );
        };
        if !manifest_permit.resize(body_capacity) {
            return respond_bytes(
                &mut stream,
                503,
                "text/plain",
                b"server busy",
                cors_origin,
                ResponseFlags::new(head_only, false),
                limits.timeouts.response,
            );
        }
        return respond_bytes(
            &mut stream,
            200,
            "application/json",
            &body,
            cors_origin,
            ResponseFlags::new(head_only, false),
            limits.timeouts.response,
        );
    }

    let Some(relative) = request_path(path) else {
        return not_found(
            &mut stream,
            head_only,
            cors_origin,
            limits.timeouts.response,
        );
    };
    let prepared = match root.prepare_sample(&relative) {
        Ok(prepared) => prepared,
        Err(OpenSampleError::NotFound) => {
            return not_found(
                &mut stream,
                head_only,
                cors_origin,
                limits.timeouts.response,
            );
        }
    };
    if prepared.len > limits.file_bytes {
        return respond_bytes(
            &mut stream,
            413,
            "text/plain",
            b"file too large",
            cors_origin,
            ResponseFlags::new(head_only, false),
            limits.timeouts.response,
        );
    }
    if head_only {
        return respond_headers(
            &mut stream,
            200,
            prepared.content_type,
            prepared.len,
            cors_origin,
            false,
            Deadline::after(limits.timeouts.response),
        );
    }

    // Charge the size learned from the pinned target before reading it. Linux
    // reopens its O_PATH anchor beneath the root; fallback platforms retain
    // the already-confined readable handle without another pathname lookup.
    let Some(mut permit) = budget.acquire(prepared.len) else {
        return respond_bytes(
            &mut stream,
            503,
            "text/plain",
            b"server busy",
            cors_origin,
            ResponseFlags::BODY,
            limits.timeouts.response,
        );
    };
    let opened = match prepared.open_read(root) {
        Ok(opened) => opened,
        Err(OpenSampleError::NotFound) => {
            return not_found(&mut stream, false, cors_origin, limits.timeouts.response);
        }
    };
    if opened.len > limits.file_bytes {
        return respond_bytes(
            &mut stream,
            413,
            "text/plain",
            b"file too large",
            cors_origin,
            ResponseFlags::BODY,
            limits.timeouts.response,
        );
    }
    if !permit.resize(opened.len) {
        return respond_bytes(
            &mut stream,
            503,
            "text/plain",
            b"server busy",
            cors_origin,
            ResponseFlags::BODY,
            limits.timeouts.response,
        );
    }

    let response_deadline = Deadline::after(limits.timeouts.response);
    respond_headers(
        &mut stream,
        200,
        opened.content_type,
        opened.len,
        cors_origin,
        false,
        response_deadline,
    )?;
    stream_file(&mut stream, opened.file, opened.len, response_deadline)
}

fn not_found(
    stream: &mut TcpStream,
    head_only: bool,
    cors_origin: Option<&str>,
    response_timeout: Duration,
) -> std::io::Result<()> {
    respond_bytes(
        stream,
        404,
        "text/plain",
        b"not found",
        cors_origin,
        ResponseFlags::new(head_only, false),
        response_timeout,
    )
}

fn stream_file(
    stream: &mut TcpStream,
    mut file: File,
    len: u64,
    deadline: Deadline,
) -> std::io::Result<()> {
    let mut writer = DeadlineWriter::new(stream, deadline);
    copy_exact_in_chunks(&mut file, &mut writer, len)?;
    writer.flush()
}

fn copy_exact_in_chunks(
    reader: &mut impl Read,
    writer: &mut impl Write,
    len: u64,
) -> std::io::Result<()> {
    let mut remaining = len;
    let mut chunk = [0u8; FILE_CHUNK_BYTES];
    while remaining != 0 {
        let wanted = remaining.min(chunk.len() as u64) as usize;
        let read = loop {
            match reader.read(&mut chunk[..wanted]) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "sample changed while it was being served",
            ));
        }
        writer.write_all(&chunk[..read])?;
        remaining -= read as u64;
    }
    Ok(())
}

fn wire_manifest(root: &ServedRoot, limits: SampleScanLimits) -> Result<Vec<u8>, ManifestError> {
    let banks = server_banks(root, limits).map_err(ManifestError::Scan)?;
    let mut body = BoundedBody::new(limits.manifest_bytes).map_err(ManifestError::Write)?;
    body.write_all(b"{").map_err(ManifestError::Write)?;
    for (bank_index, (bank, paths)) in banks.iter().enumerate() {
        if bank_index != 0 {
            body.write_all(b",").map_err(ManifestError::Write)?;
        }
        write_json_string(&mut body, None, bank).map_err(ManifestError::Write)?;
        body.write_all(b":[").map_err(ManifestError::Write)?;
        for (path_index, path) in paths.iter().enumerate() {
            if path_index != 0 {
                body.write_all(b",").map_err(ManifestError::Write)?;
            }
            write_json_string(&mut body, Some('/'), path).map_err(ManifestError::Write)?;
        }
        body.write_all(b"]").map_err(ManifestError::Write)?;
    }
    body.write_all(b"}").map_err(ManifestError::Write)?;
    Ok(body.into_inner())
}

fn server_banks(
    root: &ServedRoot,
    limits: SampleScanLimits,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, SampleFolderScanError> {
    let mut banks = crate::samples::scan_sample_folder_with_limits(&root.path, limits)?;
    // The scanner and the file route deliberately share one final check. In
    // particular, a symlink shape unsupported by the platform's confined-open
    // primitive must never be advertised as a playable sample.
    banks.retain(|_, paths| {
        paths.retain(|path| root.prepare_sample(Path::new(path)).is_ok());
        !paths.is_empty()
    });
    if banks.is_empty() {
        return Err(SampleFolderScanError::Empty(format!(
            "local samples: no servable .wav/.mp3/.ogg files under {}",
            root.path.display()
        )));
    }
    Ok(banks)
}

#[derive(Debug)]
enum ManifestError {
    Scan(SampleFolderScanError),
    Write(std::io::Error),
}

impl ManifestError {
    fn is_limit(&self) -> bool {
        match self {
            Self::Scan(error) => error.is_limit(),
            Self::Write(error) => error.kind() == std::io::ErrorKind::FileTooLarge,
        }
    }
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scan(error) => error.fmt(formatter),
            Self::Write(error) => write!(formatter, "cannot serialize sample manifest: {error}"),
        }
    }
}

struct BoundedBody {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBody {
    fn new(limit: usize) -> std::io::Result<Self> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(limit)
            .map_err(|_| std::io::Error::other("cannot allocate sample manifest"))?;
        Ok(Self { bytes, limit })
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedBody {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(next) = self.bytes.len().checked_add(bytes.len()) else {
            return Err(std::io::Error::from(std::io::ErrorKind::FileTooLarge));
        };
        if next > self.limit {
            return Err(std::io::Error::from(std::io::ErrorKind::FileTooLarge));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| std::io::Error::other("cannot allocate sample manifest"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_json_string(
    writer: &mut impl Write,
    prefix: Option<char>,
    value: &str,
) -> std::io::Result<()> {
    writer.write_all(b"\"")?;
    for character in prefix.into_iter().chain(value.chars()) {
        match character {
            '"' => writer.write_all(b"\\\"")?,
            '\\' => writer.write_all(b"\\\\")?,
            '\u{0008}' => writer.write_all(b"\\b")?,
            '\t' => writer.write_all(b"\\t")?,
            '\n' => writer.write_all(b"\\n")?,
            '\u{000c}' => writer.write_all(b"\\f")?,
            '\r' => writer.write_all(b"\\r")?,
            '\u{0000}'..='\u{001f}' => {
                let code = character as u32;
                let escaped = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    hex_digit((code >> 4) as u8),
                    hex_digit(code as u8),
                ];
                writer.write_all(&escaped)?;
            }
            _ => {
                let mut encoded = [0u8; 4];
                writer.write_all(character.encode_utf8(&mut encoded).as_bytes())?;
            }
        }
    }
    writer.write_all(b"\"")
}

fn hex_digit(nibble: u8) -> u8 {
    match nibble & 0x0f {
        value @ 0..=9 => b'0' + value,
        value => b'a' + value - 10,
    }
}

fn request_path(target: &str) -> Option<PathBuf> {
    let decoded = percent_decode(target.trim_start_matches('/'));
    let relative = PathBuf::from(decoded);
    relative_sample_path_allowed(&relative).then_some(relative)
}

fn relative_sample_path_allowed(relative: &Path) -> bool {
    let mut saw_component = false;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return false;
        };
        let component = component.to_string_lossy();
        if component.starts_with('.') || cfg!(windows) && component.contains(':') {
            return false;
        }
        saw_component = true;
    }
    saw_component
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn audio_content_type(path: &Path) -> Option<&'static str> {
    Some(match sample_audio_kind(path)? {
        SampleAudioKind::Wav => "audio/wav",
        SampleAudioKind::Mp3 => "audio/mpeg",
        SampleAudioKind::Ogg => "audio/ogg",
    })
}

fn final_target(root: &Path, path: &Path) -> Option<(&'static str, PathBuf)> {
    let relative = path.strip_prefix(root).ok()?;
    if !relative_sample_path_allowed(relative) {
        return None;
    }
    Some((audio_content_type(relative)?, path.to_path_buf()))
}

pub(crate) struct ServedRoot {
    path: PathBuf,
    #[cfg(unix)]
    directory: File,
    #[cfg(windows)]
    _directory: File,
}

impl std::fmt::Debug for ServedRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServedRoot")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ServedRoot {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for ServedRoot {}

impl ServedRoot {
    pub(crate) fn open(root: &Path) -> Result<Self, String> {
        Self::open_with(root, |_| {})
    }

    fn open_with(root: &Path, after_validation: impl FnOnce(&Path)) -> Result<Self, String> {
        let path = root
            .canonicalize()
            .map_err(|error| format!("cannot read {}: {error}", root.display()))?;
        let expected = path
            .metadata()
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if !expected.is_dir() {
            return Err(format!("cannot read {}: not a directory", path.display()));
        }
        after_validation(&path);
        #[cfg(unix)]
        let directory = {
            let directory = open_unix_directory(&path)
                .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
            let opened = directory
                .metadata()
                .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
            if !opened.is_dir() || FileIdentity::of(&opened) != FileIdentity::of(&expected) {
                return Err(format!(
                    "cannot open {}: sample folder changed while it was being opened",
                    path.display()
                ));
            }
            directory
        };
        #[cfg(windows)]
        let (path, directory) = {
            let directory = open_windows_directory(&path)
                .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
            if !directory
                .metadata()
                .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
                .is_dir()
            {
                return Err(format!("cannot read {}: not a directory", path.display()));
            }
            let final_path = windows_final_path(&directory)
                .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
            (final_path, directory)
        };
        Ok(Self {
            path,
            #[cfg(unix)]
            directory,
            #[cfg(windows)]
            _directory: directory,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn open_score_sample_with(
        &self,
        absolute: &Path,
        before_open: impl FnOnce(),
    ) -> Result<(File, u64), String> {
        // Both sides in one prefix form before the component-wise check: on
        // Windows this root's final path is verbatim while `Url::to_file_path`
        // never answers verbatim. The check itself stays component-wise - a
        // string comparison would admit a sibling sharing the root's text
        // prefix - and the relative path still has to be plain components.
        let root = with_verbatim_prefix(&self.path);
        let absolute = with_verbatim_prefix(absolute);
        let relative = absolute
            .strip_prefix(root.as_ref())
            .ok()
            .filter(|relative| relative_sample_path_allowed(relative))
            .ok_or_else(|| "local sample is outside the permitted root".to_owned())?;
        before_open();
        let opened = self
            .prepare_sample(relative)
            .and_then(|prepared| prepared.open_read(self))
            .map_err(|_| "local sample is unavailable beneath the permitted root".to_owned())?;
        Ok((opened.file, opened.len))
    }

    fn prepare_sample(&self, relative: &Path) -> Result<PreparedSample, OpenSampleError> {
        #[cfg(target_os = "linux")]
        match self.prepare_linux(relative) {
            Ok(prepared) => return Ok(prepared),
            Err(LinuxPrepareError::Refused(error)) => return Err(error),
            Err(LinuxPrepareError::Unsupported) => {}
        }
        #[cfg(unix)]
        return self.prepare_unix(relative);
        #[cfg(windows)]
        return self.prepare_windows(relative);
        #[cfg(not(any(unix, windows)))]
        Err(OpenSampleError::NotFound)
    }

    #[cfg(target_os = "linux")]
    fn prepare_linux(&self, relative: &Path) -> Result<PreparedSample, LinuxPrepareError> {
        let anchor =
            match openat2_beneath(&self.directory, relative, libc::O_PATH | libc::O_CLOEXEC) {
                Ok(file) => file,
                Err(error) if error.raw_os_error() == Some(libc::ENOSYS) => {
                    return Err(LinuxPrepareError::Unsupported);
                }
                Err(_) => return Err(LinuxPrepareError::Refused(OpenSampleError::NotFound)),
            };
        let metadata = anchor
            .metadata()
            .map_err(|_| LinuxPrepareError::Refused(OpenSampleError::NotFound))?;
        if !metadata.is_file() {
            return Err(LinuxPrepareError::Refused(OpenSampleError::NotFound));
        }
        let opened_path = path_for_open_file(&anchor)
            .ok_or(LinuxPrepareError::Refused(OpenSampleError::NotFound))?;
        let (content_type, _) = final_target(&self.path, &opened_path)
            .ok_or(LinuxPrepareError::Refused(OpenSampleError::NotFound))?;
        Ok(PreparedSample {
            relative: relative.to_path_buf(),
            len: metadata.len(),
            content_type,
            backing: PreparedBacking::Linux {
                anchor,
                identity: FileIdentity::of(&metadata),
            },
        })
    }

    #[cfg(unix)]
    fn prepare_unix(&self, relative: &Path) -> Result<PreparedSample, OpenSampleError> {
        // openat is the portable Unix fallback for kernels without openat2.
        // Each component is opened beneath the pinned parent with O_NOFOLLOW;
        // symlinks are refused when the platform cannot confine them atomically.
        let (content_type, _) =
            final_target(&self.path, &self.path.join(relative)).ok_or(OpenSampleError::NotFound)?;
        let file = openat_no_symlinks(
            &self.directory,
            relative,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
        .map_err(|_| OpenSampleError::NotFound)?;
        let metadata = file.metadata().map_err(|_| OpenSampleError::NotFound)?;
        if !metadata.is_file() {
            return Err(OpenSampleError::NotFound);
        }
        Ok(PreparedSample {
            #[cfg(target_os = "linux")]
            relative: relative.to_path_buf(),
            len: metadata.len(),
            content_type,
            backing: PreparedBacking::Opened { file },
        })
    }

    #[cfg(windows)]
    fn prepare_windows(&self, relative: &Path) -> Result<PreparedSample, OpenSampleError> {
        let file =
            open_windows_file(&self.path.join(relative)).map_err(|_| OpenSampleError::NotFound)?;
        let metadata = file.metadata().map_err(|_| OpenSampleError::NotFound)?;
        if !metadata.is_file() {
            return Err(OpenSampleError::NotFound);
        }
        let opened_path = windows_final_path(&file).map_err(|_| OpenSampleError::NotFound)?;
        let (content_type, _) =
            final_target(&self.path, &opened_path).ok_or(OpenSampleError::NotFound)?;
        Ok(PreparedSample {
            len: metadata.len(),
            content_type,
            backing: PreparedBacking::Opened { file },
        })
    }
}

struct PreparedSample {
    #[cfg(target_os = "linux")]
    relative: PathBuf,
    len: u64,
    content_type: &'static str,
    backing: PreparedBacking,
}

enum PreparedBacking {
    #[cfg(target_os = "linux")]
    Linux {
        anchor: File,
        identity: FileIdentity,
    },
    Opened {
        file: File,
    },
}

struct OpenedSample {
    file: File,
    len: u64,
    content_type: &'static str,
}

impl PreparedSample {
    fn open_read(self, _root: &ServedRoot) -> Result<OpenedSample, OpenSampleError> {
        match self.backing {
            #[cfg(target_os = "linux")]
            PreparedBacking::Linux { anchor, identity } => {
                #[cfg(test)]
                READABLE_OPENS.fetch_add(1, Ordering::Relaxed);
                let file = openat2_beneath(
                    &_root.directory,
                    &self.relative,
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )
                .map_err(|_| OpenSampleError::NotFound)?;
                let metadata = file.metadata().map_err(|_| OpenSampleError::NotFound)?;
                if !metadata.is_file() || FileIdentity::of(&metadata) != identity {
                    return Err(OpenSampleError::NotFound);
                }
                let opened_path = path_for_open_file(&file).ok_or(OpenSampleError::NotFound)?;
                let (content_type, _) =
                    final_target(&_root.path, &opened_path).ok_or(OpenSampleError::NotFound)?;
                drop(anchor);
                Ok(OpenedSample {
                    file,
                    len: metadata.len(),
                    content_type,
                })
            }
            PreparedBacking::Opened { file } => {
                let metadata = file.metadata().map_err(|_| OpenSampleError::NotFound)?;
                if !metadata.is_file() {
                    return Err(OpenSampleError::NotFound);
                }
                Ok(OpenedSample {
                    file,
                    len: metadata.len(),
                    content_type: self.content_type,
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum OpenSampleError {
    NotFound,
}

#[cfg(target_os = "linux")]
enum LinuxPrepareError {
    Unsupported,
    Refused(OpenSampleError),
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl FileIdentity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[cfg(unix)]
fn open_unix_directory(path: &Path) -> std::io::Result<File> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // O_DIRECTORY and O_NONBLOCK make a replacement FIFO or regular file fail
    // at the open itself, before any potentially blocking read-side open.
    // SAFETY: `path` is NUL-terminated and these flags do not create a file, so
    // no mode argument is required.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY
                | libc::O_CLOEXEC
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a nonnegative open result is a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn openat_no_symlinks(directory: &File, relative: &Path, flags: i32) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let mut parent = None::<File>;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        };
        let name = std::ffi::CString::new(component.as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let final_component = components.peek().is_none();
        let open_flags = if final_component {
            flags | libc::O_NOFOLLOW
        } else {
            libc::O_RDONLY
                | libc::O_CLOEXEC
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
        };
        let parent_fd = parent
            .as_ref()
            .map_or_else(|| directory.as_raw_fd(), AsRawFd::as_raw_fd);
        // SAFETY: the parent descriptor is live, `name` is NUL-terminated, and
        // no mode argument is required because the flags never create a file.
        let descriptor = unsafe { libc::openat(parent_fd, name.as_ptr(), open_flags) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a nonnegative openat result is a newly owned descriptor.
        let opened = unsafe { File::from_raw_fd(descriptor) };
        if final_component {
            return Ok(opened);
        }
        parent = Some(opened);
    }
    Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
}

#[cfg(windows)]
fn open_windows_directory(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    // Keeping this handle open without FILE_SHARE_DELETE prevents the root
    // directory from being renamed out from under the final-handle check.
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(windows)]
fn open_windows_file(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
}

#[cfg(windows)]
fn windows_final_path(file: &File) -> std::io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    const MAX_FINAL_PATH_UNITS: u32 = 32_768;
    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    // SAFETY: `handle` is live and a null zero-length buffer asks Windows for
    // the required UTF-16 capacity without writing through the pointer.
    let required = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            std::ptr::null_mut(),
            0,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if required == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if required > MAX_FINAL_PATH_UNITS {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    let mut units = Vec::new();
    units
        .try_reserve_exact(required as usize)
        .map_err(|_| std::io::Error::other("cannot allocate final sample path"))?;
    units.resize(required as usize, 0u16);
    // SAFETY: the buffer contains `required` writable UTF-16 elements and the
    // handle stays live throughout the call.
    let written = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            units.as_mut_ptr(),
            required,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if written == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if written >= required {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    units.truncate(written as usize);
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&units)))
}

/// The verbatim spelling of `path`: `C:\x` becomes `\\?\C:\x` and
/// `\\server\share\x` becomes `\\?\UNC\server\share\x`. Anything else is
/// itself.
///
/// `ServedRoot` holds a verbatim path from `GetFinalPathNameByHandleW`, but
/// a `file:` URL round trip returns an ordinary drive or UNC prefix.
/// `Path::strip_prefix` distinguishes these prefix kinds, so normalize them
/// before comparing components. This only changes spelling; the relative
/// components and the opened file's final path still require validation.
#[cfg(windows)]
fn with_verbatim_prefix(path: &Path) -> std::borrow::Cow<'_, Path> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::Prefix;

    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return std::borrow::Cow::Borrowed(path);
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let converted = match prefix.kind() {
        Prefix::Disk(_) => {
            let mut out: Vec<u16> = r"\\?\".encode_utf16().collect();
            out.extend_from_slice(&wide);
            out
        }
        Prefix::UNC(_, _) => {
            // The verbatim spelling of `\\server\share` drops the leading
            // pair of separators: `\\?\UNC\server\share`.
            let mut out: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
            out.extend_from_slice(&wide[2..]);
            out
        }
        _ => return std::borrow::Cow::Borrowed(path),
    };
    std::borrow::Cow::Owned(PathBuf::from(OsString::from_wide(&converted)))
}

/// The identity where no verbatim prefix exists, so the confinement check
/// normalizes both of its sides unconditionally.
#[cfg(not(windows))]
fn with_verbatim_prefix(path: &Path) -> std::borrow::Cow<'_, Path> {
    std::borrow::Cow::Borrowed(path)
}

#[cfg(target_os = "linux")]
fn openat2_beneath(directory: &File, relative: &Path, flags: i32) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_BENEATH: u64 = 0x08;

    let path = std::ffi::CString::new(relative.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let how = OpenHow {
        flags: flags as u64,
        mode: 0,
        // Relative symlinks may name another sample inside the root, matching
        // the scanner's existing policy. Absolute escapes and procfs magic
        // links are refused by the kernel during resolution.
        resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: `path` is NUL-terminated, `how` has the kernel's open_how
    // layout, and both remain alive for the duration of the syscall.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a nonnegative openat2 result is a newly owned descriptor. File
    // takes that one ownership and closes it exactly once.
    Ok(unsafe { File::from_raw_fd(descriptor as i32) })
}

#[cfg(target_os = "linux")]
fn path_for_open_file(file: &File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

struct InFlightBudget {
    used: AtomicU64,
    limit: u64,
}

impl InFlightBudget {
    const fn new(limit: u64) -> Self {
        Self {
            used: AtomicU64::new(0),
            limit,
        }
    }

    fn acquire(&self, bytes: u64) -> Option<InFlightPermit<'_>> {
        self.add(bytes).then(|| InFlightPermit {
            budget: self,
            bytes,
        })
    }

    fn add(&self, bytes: u64) -> bool {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let Some(next) = used.checked_add(bytes) else {
                return false;
            };
            if next > self.limit {
                return false;
            }
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(current) => used = current,
            }
        }
    }
}

struct InFlightPermit<'a> {
    budget: &'a InFlightBudget,
    bytes: u64,
}

impl InFlightPermit<'_> {
    fn resize(&mut self, bytes: u64) -> bool {
        if bytes > self.bytes {
            if !self.budget.add(bytes - self.bytes) {
                return false;
            }
        } else {
            self.budget
                .used
                .fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        }
        self.bytes = bytes;
        true
    }
}

impl Drop for InFlightPermit<'_> {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

struct HandlerSlots {
    used: AtomicUsize,
    limit: usize,
}

impl HandlerSlots {
    const fn new(limit: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            limit,
        }
    }

    fn acquire(&self) -> Option<HandlerPermit<'_>> {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            if used >= self.limit {
                return None;
            }
            match self.used.compare_exchange_weak(
                used,
                used + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(HandlerPermit { slots: self }),
                Err(current) => used = current,
            }
        }
    }
}

struct HandlerPermit<'a> {
    slots: &'a HandlerSlots,
}

impl Drop for HandlerPermit<'_> {
    fn drop(&mut self) {
        self.slots.used.fetch_sub(1, Ordering::AcqRel);
    }
}

struct DeadlineWriter<'a> {
    stream: &'a mut TcpStream,
    deadline: Deadline,
}

impl<'a> DeadlineWriter<'a> {
    fn new(stream: &'a mut TcpStream, deadline: Deadline) -> Self {
        Self { stream, deadline }
    }
}

impl Write for DeadlineWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.deadline.install_write_timeout(self.stream)?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.deadline.install_write_timeout(self.stream)?;
        self.stream.flush()
    }
}

#[derive(Clone, Copy)]
struct ResponseFlags {
    head_only: bool,
    preflight: bool,
}

impl ResponseFlags {
    const BODY: Self = Self::new(false, false);
    const PREFLIGHT: Self = Self::new(false, true);

    const fn new(head_only: bool, preflight: bool) -> Self {
        Self {
            head_only,
            preflight,
        }
    }
}

fn respond_bytes(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    cors_origin: Option<&str>,
    flags: ResponseFlags,
    response_timeout: Duration,
) -> std::io::Result<()> {
    let deadline = Deadline::after(response_timeout);
    respond_headers(
        stream,
        status,
        content_type,
        body.len() as u64,
        cors_origin,
        flags.preflight,
        deadline,
    )?;
    let mut writer = DeadlineWriter::new(stream, deadline);
    if !flags.head_only {
        writer.write_all(body)?;
    }
    writer.flush()
}

fn respond_headers(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    content_length: u64,
    cors_origin: Option<&str>,
    preflight: bool,
    deadline: Deadline,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        408 => "Request Timeout",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let mut writer = DeadlineWriter::new(stream, deadline);
    write!(
        writer,
        "HTTP/1.1 {status} {reason}\r\n\
         Vary: Origin\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {content_length}\r\n\
         Connection: close\r\n"
    )?;
    if let Some(origin) = cors_origin {
        write!(writer, "Access-Control-Allow-Origin: {origin}\r\n")?;
    }
    if preflight {
        write!(writer, "Access-Control-Allow-Methods: GET, HEAD\r\n")?;
        if cors_origin.is_some() {
            // Chromium's private-network preflight covers a public HTTPS page
            // fetching this loopback server. The origin grant above is still
            // the authorization decision; this only lets that granted request
            // cross the browser's address-space boundary.
            write!(writer, "Access-Control-Allow-Private-Network: true\r\n")?;
        }
    }
    write!(writer, "\r\n")
}

#[cfg(test)]
static READABLE_OPENS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let serial = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rustel-sample-server-{}-{name}-{serial}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("fixture directory");
            Self { path }
        }

        fn write(&self, path: &str, bytes: &[u8]) {
            let path = self.path.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("fixture parents");
            }
            std::fs::write(path, bytes).expect("fixture file");
        }

        #[cfg(unix)]
        fn set_len(&self, path: &str, len: u64) {
            let path = self.path.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("fixture parents");
            }
            File::create(path)
                .and_then(|file| file.set_len(len))
                .expect("fixture length");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn exchange(
        root: &Path,
        origins: &OriginPolicy,
        budget: &InFlightBudget,
        limits: SampleScanLimits,
        request: &str,
    ) -> Vec<u8> {
        exchange_with_timeouts(
            root,
            origins,
            budget,
            limits,
            request,
            ConnectionTimeouts::DEFAULT,
        )
    }

    fn exchange_with_timeouts(
        root: &Path,
        origins: &OriginPolicy,
        budget: &InFlightBudget,
        limits: SampleScanLimits,
        request: &str,
        timeouts: ConnectionTimeouts,
    ) -> Vec<u8> {
        exchange_with_options(
            root,
            origins,
            budget,
            limits,
            request,
            MAX_FILE_BYTES,
            timeouts,
        )
    }

    fn exchange_with_options(
        root: &Path,
        origins: &OriginPolicy,
        budget: &InFlightBudget,
        limits: SampleScanLimits,
        request: &str,
        max_file_bytes: u64,
        timeouts: ConnectionTimeouts,
    ) -> Vec<u8> {
        let root = Arc::new(ServedRoot::open(root).expect("served root"));
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener");
        let address = listener.local_addr().expect("test address");
        let hosts = HostPolicy::new("127.0.0.1", address.port()).expect("test hosts");
        let request = with_default_host(request, address);
        let root_for_server = Arc::clone(&root);
        let origins = origins.clone();
        std::thread::scope(|scope| {
            let server = scope.spawn(move || {
                let (stream, _) = listener.accept().expect("test accept");
                handle(
                    stream,
                    &root_for_server,
                    &origins,
                    &hosts,
                    budget,
                    ServerLimits {
                        scan: limits,
                        file_bytes: max_file_bytes,
                        timeouts,
                    },
                )
                .ok();
            });
            let mut client = TcpStream::connect(address).expect("test connect");
            client.write_all(request.as_bytes()).expect("test request");
            client
                .shutdown(std::net::Shutdown::Write)
                .expect("finish request");
            let mut response = Vec::new();
            client.read_to_end(&mut response).expect("test response");
            server.join().expect("test server");
            response
        })
    }

    fn with_default_host(request: &str, address: std::net::SocketAddr) -> String {
        let request = request.replace("{PORT}", &address.port().to_string());
        let has_host = request.lines().skip(1).any(|line| {
            line.split_once(':')
                .is_some_and(|(name, _)| name.eq_ignore_ascii_case("host"))
        });
        if has_host {
            return request;
        }
        let Some((request_line, rest)) = request.split_once("\r\n") else {
            return request;
        };
        format!("{request_line}\r\nHost: {address}\r\n{rest}")
    }

    fn default_policy() -> OriginPolicy {
        OriginPolicy::new(&[]).expect("default origin")
    }

    fn manifest_budget() -> InFlightBudget {
        InFlightBudget::new(((MAX_SAMPLE_SCAN_WORK_BYTES + MAX_SAMPLE_MANIFEST_BYTES) * 2) as u64)
    }

    fn status(response: &[u8]) -> u16 {
        let line = response.split(|byte| *byte == b'\n').next().unwrap_or(&[]);
        std::str::from_utf8(line)
            .expect("response status")
            .split_whitespace()
            .nth(1)
            .expect("status code")
            .parse()
            .expect("numeric status")
    }

    fn body(response: &[u8]) -> &[u8] {
        response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| &response[index + 4..])
            .unwrap_or(&[])
    }

    fn contains(response: &[u8], needle: &[u8]) -> bool {
        response
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn origin_grants_are_exact_and_custom_values_replace_the_default() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("cors");
        fixture.write("tone.wav", b"RIFF");
        let budget = manifest_budget();
        let allowed = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /tone.wav HTTP/1.1\r\nOrigin: https://strudel.cc\r\n\r\n",
        );
        assert_eq!(status(&allowed), 200);
        assert!(contains(
            &allowed,
            b"Access-Control-Allow-Origin: https://strudel.cc"
        ));
        assert!(contains(&allowed, b"Cache-Control: no-store"));
        assert!(contains(&allowed, b"X-Content-Type-Options: nosniff"));

        let denied = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /tone.wav HTTP/1.1\r\nOrigin: https://strudel.cc.evil.example\r\n\r\n",
        );
        assert_eq!(status(&denied), 403);
        assert!(contains(&denied, b"--allow-origin <origin>"));
        assert!(!contains(&denied, b"RIFF"));
        assert!(!contains(&denied, b"Access-Control-Allow-Origin"));
        let malformed = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /tone.wav HTTP/1.1\r\nOrigin : https://example.test\r\n\r\n",
        );
        assert_eq!(status(&malformed), 400);
        assert!(!contains(&malformed, b"RIFF"));

        let custom =
            OriginPolicy::new(&["http://localhost:3000".to_owned()]).expect("custom origin");
        let replaced = exchange(
            &fixture.path,
            &custom,
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /tone.wav HTTP/1.1\r\nOrigin: https://strudel.cc\r\n\r\n",
        );
        assert_eq!(status(&replaced), 403);
        let custom_allowed = exchange(
            &fixture.path,
            &custom,
            &budget,
            SampleScanLimits::DEFAULT,
            "OPTIONS /tone.wav HTTP/1.1\r\nOrigin: http://localhost:3000\r\nAccess-Control-Request-Method: GET\r\n\r\n",
        );
        assert_eq!(status(&custom_allowed), 204);
        assert!(body(&custom_allowed).is_empty());
        assert!(contains(
            &custom_allowed,
            b"Access-Control-Allow-Origin: http://localhost:3000"
        ));

        let originless = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /tone.wav HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&originless), 200);
        assert!(!contains(&originless, b"Access-Control-Allow-Origin"));
    }

    #[test]
    fn origin_configuration_is_structural() {
        for invalid in [
            "file:///tmp/samples",
            "https://strudel.cc/editor",
            "https://user@localhost",
            "https://strudel.cc?q=1",
            "null",
        ] {
            assert!(
                OriginPolicy::new(&[invalid.to_owned()]).is_err(),
                "accepted {invalid}"
            );
        }
        let policy =
            OriginPolicy::new(&["HTTPS://STRUDEL.CC:443/".to_owned()]).expect("normalized origin");
        assert!(policy.allowed.contains("https://strudel.cc"));
    }

    #[test]
    fn host_authority_blocks_dns_rebinding_even_for_an_allowed_origin() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("host-authority");
        fixture.write("tone.wav", b"RIFF");
        let budget = manifest_budget();
        for authority in ["attacker.example:{PORT}", "localhost.evil.example:{PORT}"] {
            let response = exchange(
                &fixture.path,
                &default_policy(),
                &budget,
                SampleScanLimits::DEFAULT,
                &format!(
                    "GET /tone.wav HTTP/1.1\r\nHost: {authority}\r\nOrigin: https://strudel.cc\r\n\r\n"
                ),
            );
            assert_eq!(status(&response), 403, "accepted {authority}");
            assert!(!contains(&response, b"RIFF"));
            assert!(!contains(&response, b"Access-Control-Allow-Origin"));
        }

        let localhost = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /tone.wav HTTP/1.1\r\nHost: localhost:{PORT}\r\nOrigin: https://strudel.cc\r\n\r\n",
        );
        assert_eq!(status(&localhost), 200);
        assert_eq!(body(&localhost), b"RIFF");

        let policy = HostPolicy::new("studio.local", 5432).expect("configured host");
        let local: std::net::SocketAddr = "192.0.2.10:5432".parse().expect("local address");
        assert!(policy.authorize("studio.local:5432", local));
        assert!(!policy.authorize("studio.local.evil:5432", local));
    }

    #[test]
    fn only_visible_audio_targets_are_served() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("types");
        fixture.write("tone.wav", b"RIFF");
        fixture.write("Cargo.toml", b"private project data");
        fixture.write(".hidden.wav", b"hidden file");
        fixture.write(".private/secret.wav", b"hidden directory");
        let budget = manifest_budget();
        for target in ["/Cargo.toml", "/.hidden.wav", "/.private/secret.wav"] {
            let response = exchange(
                &fixture.path,
                &default_policy(),
                &budget,
                SampleScanLimits::DEFAULT,
                &format!("GET {target} HTTP/1.1\r\n\r\n"),
            );
            assert_eq!(status(&response), 404, "served {target}");
        }
        let audio = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /tone.wav HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&audio), 200);
        assert_eq!(body(&audio), b"RIFF");
        let manifest = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&manifest), 200);
        assert!(contains(body(&manifest), b"tone.wav"));
        assert!(!contains(body(&manifest), b"Cargo.toml"));
        assert!(!contains(body(&manifest), b"hidden"));
    }

    #[test]
    fn traversal_is_refused_by_the_http_route() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("traversal-root");
        fixture.write("inside.wav", b"inside");
        let outside = Fixture::new("traversal-outside");
        outside.write("secret.wav", b"outside secret");
        let outside_name = outside
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("outside name");
        let budget = manifest_budget();
        for target in [
            format!("/../{outside_name}/secret.wav"),
            format!("/%2e%2e/{outside_name}/secret.wav"),
            format!("/inside/../../{outside_name}/secret.wav"),
            "/file/../../password.txt".to_owned(),
            "/file/%2e%2e/%2e%2e/password.txt".to_owned(),
            "/./inside.wav".to_owned(),
        ] {
            let response = exchange(
                &fixture.path,
                &default_policy(),
                &budget,
                SampleScanLimits::DEFAULT,
                &format!("GET {target} HTTP/1.1\r\n\r\n"),
            );
            assert_eq!(status(&response), 404, "accepted {target}");
            assert!(!contains(&response, b"outside secret"));
        }
    }

    #[test]
    fn request_line_and_header_limits_are_enforced_on_the_socket() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("request-limits");
        fixture.write("tone.wav", b"RIFF");
        let budget = manifest_budget();

        let long_target = "a".repeat(MAX_REQUEST_LINE_BYTES);
        let response = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            &format!("GET /{long_target} HTTP/1.1\r\n\r\n"),
        );
        assert_eq!(status(&response), 413);

        let long_value = "a".repeat(MAX_HEADER_LINE_BYTES);
        let response = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            &format!("GET /tone.wav HTTP/1.1\r\nX-Long: {long_value}\r\n\r\n"),
        );
        assert_eq!(status(&response), 413);

        let mut aggregate = String::from("GET /tone.wav HTTP/1.1\r\n");
        for index in 0..9 {
            aggregate.push_str(&format!("X-{index}: {}\r\n", "a".repeat(4 * 1024)));
        }
        aggregate.push_str("\r\n");
        let response = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            &aggregate,
        );
        assert_eq!(status(&response), 413);

        let response = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /tone.wav HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\nHost: 127.0.0.1:{PORT}\r\n\r\n",
        );
        assert_eq!(status(&response), 400);
    }

    #[test]
    fn request_deadline_is_absolute_across_partial_headers() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("request-deadline");
        fixture.write("tone.wav", b"RIFF");
        let root = Arc::new(ServedRoot::open(&fixture.path).expect("served root"));
        let origins = default_policy();
        let budget = InFlightBudget::new(MAX_FILE_BYTES);
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener");
        let address = listener.local_addr().expect("test address");
        let hosts = HostPolicy::new("127.0.0.1", address.port()).expect("test hosts");
        let timeouts = ConnectionTimeouts {
            request: Duration::from_millis(40),
            response: Duration::from_secs(1),
        };

        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                let (stream, _) = listener.accept().expect("test accept");
                handle(
                    stream,
                    &root,
                    &origins,
                    &hosts,
                    &budget,
                    ServerLimits {
                        timeouts,
                        ..ServerLimits::DEFAULT
                    },
                )
                .expect("timeout response");
            });
            let mut client = TcpStream::connect(address).expect("test connect");
            write!(
                client,
                "GET /tone.wav HTTP/1.1\r\nHost: {address}\r\nX-Slow: "
            )
            .expect("partial request");
            std::thread::sleep(Duration::from_millis(100));
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("client timeout");
            let mut response = Vec::new();
            client.read_to_end(&mut response).expect("timeout response");
            assert_eq!(status(&response), 408);
            server.join().expect("test server");
        });
    }

    #[cfg(unix)]
    #[test]
    fn symlink_policy_is_applied_to_the_opened_final_target() {
        use std::os::unix::fs::symlink;

        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("symlinks");
        fixture.write("real.wav", b"RIFF");
        fixture.write("notes.txt", b"not audio");
        fixture.write(".private/secret.wav", b"hidden");
        let outside = Fixture::new("outside");
        outside.write("outside.wav", b"outside");
        symlink("real.wav", fixture.path.join("inside.wav")).expect("inside symlink");
        symlink("notes.txt", fixture.path.join("disguise.wav")).expect("type symlink");
        symlink(
            ".private/secret.wav",
            fixture.path.join("hidden-target.wav"),
        )
        .expect("hidden symlink");
        symlink(
            outside.path.join("outside.wav"),
            fixture.path.join("escape.wav"),
        )
        .expect("escape symlink");
        let budget = manifest_budget();

        let inside = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /inside.wav HTTP/1.1\r\n\r\n",
        );
        let inside_is_supported = status(&inside) == 200;
        assert!(
            inside_is_supported || status(&inside) == 404,
            "unexpected inside-symlink response: {}",
            status(&inside)
        );
        for target in ["/disguise.wav", "/hidden-target.wav", "/escape.wav"] {
            let response = exchange(
                &fixture.path,
                &default_policy(),
                &budget,
                SampleScanLimits::DEFAULT,
                &format!("GET {target} HTTP/1.1\r\n\r\n"),
            );
            assert_eq!(status(&response), 404, "served {target}");
        }
        let manifest = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(
            contains(body(&manifest), b"inside.wav"),
            inside_is_supported,
            "the manifest and file route disagreed about confined symlink support"
        );
        for hidden in [
            b"disguise.wav".as_slice(),
            b"hidden-target.wav",
            b"escape.wav",
        ] {
            assert!(!contains(body(&manifest), hidden));
        }
    }

    #[cfg(windows)]
    #[test]
    fn alternate_data_stream_paths_are_refused_on_the_socket() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("alternate-data-stream");
        fixture.write("tone.wav", b"RIFF");
        std::fs::write(fixture.path.join("tone.wav:private.wav"), b"private stream")
            .expect("alternate data stream");
        let budget = manifest_budget();

        for target in [
            "/tone.wav:private.wav",
            "/tone.wav%3aprivate.wav",
            "/tone.wav%3Aprivate.wav",
        ] {
            let response = exchange(
                &fixture.path,
                &default_policy(),
                &budget,
                SampleScanLimits::DEFAULT,
                &format!("GET {target} HTTP/1.1\r\n\r\n"),
            );
            assert_eq!(status(&response), 404, "served {target}");
            assert!(!contains(&response, b"private stream"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_file_url_round_trip_stays_inside_the_served_root() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("url-round-trip");
        fixture.write("kit/kick.wav", b"RIFF inside");
        let root = ServedRoot::open(&fixture.path).expect("served root");
        // The root holds the final-path verbatim spelling, `\\?\C:\...`.
        let root_text = root.path().as_os_str().to_string_lossy();
        assert!(
            root_text.starts_with(r"\\?\"),
            "the served root is not in verbatim form: {root_text}"
        );
        // A `file:` URL round trip loses the verbatim prefix: `to_file_path`
        // never answers verbatim. The result must still name a path inside
        // the root.
        let canonical = fixture
            .path
            .join("kit")
            .join("kick.wav")
            .canonicalize()
            .expect("canonical sample");
        let url = url::Url::from_file_path(&canonical).expect("sample file URL");
        let path = url.to_file_path().expect("round-tripped sample path");
        assert_ne!(
            path, canonical,
            "the round trip no longer loses the verbatim prefix, so this regression has moved"
        );
        let (mut file, len) = root
            .open_score_sample_with(&path, || {})
            .expect("a registered sample must load through its own URL round trip");
        assert_eq!(len, "RIFF inside".len() as u64);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).expect("read sample");
        assert_eq!(bytes, b"RIFF inside");
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_outside_the_served_root_are_still_refused() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("url-confinement");
        fixture.write("kit/kick.wav", b"RIFF inside");
        let outside = fixture.path.with_extension("outside.wav");
        // Dot-free, so it shares the root's text prefix without tripping the
        // leading-dot rule: only a component-wise comparison refuses it.
        let sibling = PathBuf::from(format!("{}-evil", fixture.path.display()));
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&sibling);
        std::fs::write(&outside, b"RIFF secret").expect("outside file");
        std::fs::create_dir_all(&sibling).expect("sibling directory");
        std::fs::write(sibling.join("kick.wav"), b"RIFF sibling").expect("sibling file");
        let root = ServedRoot::open(&fixture.path).expect("served root");

        // A plain file beside the root, through the same URL round trip.
        let url = url::Url::from_file_path(outside.canonicalize().expect("canonical outside"))
            .expect("outside file URL");
        let path = url.to_file_path().expect("outside path");
        let error = root
            .open_score_sample_with(&path, || {})
            .expect_err("a file beside the root must be refused");
        assert!(error.contains("outside the permitted root"), "{error}");

        // A sibling sharing the root's TEXT prefix: what a string comparison
        // would serve, the component-wise comparison must refuse.
        let url = url::Url::from_file_path(
            sibling
                .join("kick.wav")
                .canonicalize()
                .expect("canonical sibling"),
        )
        .expect("sibling file URL");
        let path = url.to_file_path().expect("sibling path");
        let error = root
            .open_score_sample_with(&path, || {})
            .expect_err("a sibling sharing the root's text prefix must be refused");
        assert!(error.contains("outside the permitted root"), "{error}");

        // `..` traversal must be refused in both prefix spellings: from the
        // root's own verbatim form and from a plain non-verbatim path.
        let outside_name = outside.file_name().expect("outside name");
        for traversal in [
            root.path()
                .join("kit")
                .join("..")
                .join("..")
                .join(outside_name),
            fixture
                .path
                .join("kit")
                .join("..")
                .join("..")
                .join(outside_name),
        ] {
            let error = root
                .open_score_sample_with(&traversal, || {})
                .expect_err("a traversal path must be refused");
            assert!(error.contains("outside the permitted root"), "{error}");
        }

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&sibling);
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_escaping_the_root_is_refused_at_the_opened_final_path() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("url-junction");
        std::fs::create_dir(fixture.path.join("kit")).expect("kit directory");
        let target = Fixture::new("url-junction-target");
        target.write("secret.wav", b"RIFF secret");
        let link = fixture.path.join("kit").join("escape");
        // A junction needs no privilege, unlike a symlink, so this runs on any
        // Windows host.
        let link_text = link.display().to_string();
        let target_text = target.path.display().to_string();
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &link_text, &target_text])
            .output()
            .expect("run mklink");
        assert!(
            output.status.success(),
            "create a junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let root = ServedRoot::open(&fixture.path).expect("served root");
        // The URL uses the root's normalized spelling without resolving the
        // junction. The opened file's final path must still be refused.
        let url =
            url::Url::from_file_path(root.path().join("kit").join("escape").join("secret.wav"))
                .expect("junction URL");
        let path = url.to_file_path().expect("junction path");
        let error = root
            .open_score_sample_with(&path, || {})
            .expect_err("a junction escaping the root must be refused");
        assert!(
            error.contains("unavailable beneath the permitted root"),
            "{error}"
        );
    }

    #[test]
    fn head_does_not_open_a_readable_file_or_consume_the_body_budget() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("head");
        fixture.write("tone.wav", b"RIFF");
        let budget = InFlightBudget::new(0);
        READABLE_OPENS.store(0, Ordering::Relaxed);
        let response = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "HEAD /tone.wav HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&response), 200);
        assert!(body(&response).is_empty());
        assert_eq!(READABLE_OPENS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn file_size_limit_is_checked_before_body_open_or_streaming() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("file-size");
        fixture.write("exact.wav", b"1234");
        fixture.write("over.wav", b"12345");
        let budget = InFlightBudget::new(16);
        let timeouts = ConnectionTimeouts::DEFAULT;

        let exact = exchange_with_options(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /exact.wav HTTP/1.1\r\n\r\n",
            4,
            timeouts,
        );
        assert_eq!(status(&exact), 200);
        assert_eq!(body(&exact), b"1234");

        READABLE_OPENS.store(0, Ordering::Relaxed);
        for method in ["GET", "HEAD"] {
            let over = exchange_with_options(
                &fixture.path,
                &default_policy(),
                &budget,
                SampleScanLimits::DEFAULT,
                &format!("{method} /over.wav HTTP/1.1\r\n\r\n"),
                4,
                timeouts,
            );
            assert_eq!(status(&over), 413, "accepted oversized {method}");
            assert!(body(&over).is_empty() || body(&over) == b"file too large");
        }
        assert_eq!(READABLE_OPENS.load(Ordering::Relaxed), 0);
        assert_eq!(budget.used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_streamed_get_preserves_chunk_boundaries() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("stream");
        let bytes: Vec<u8> = (0..FILE_CHUNK_BYTES + 17)
            .map(|index| (index % 251) as u8)
            .collect();
        fixture.write("long.wav", &bytes);
        let budget = InFlightBudget::new(bytes.len() as u64);
        let response = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /long.wav HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&response), 200);
        assert_eq!(body(&response), bytes);
        assert_eq!(budget.used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn streamed_copy_alternates_bounded_reads_and_writes() {
        use std::cell::RefCell;

        #[derive(Debug, PartialEq, Eq)]
        enum Event {
            Read { requested: usize, returned: usize },
            Write(usize),
        }

        struct ObservedReader<'a> {
            input: std::io::Cursor<Vec<u8>>,
            events: &'a RefCell<Vec<Event>>,
        }

        impl Read for ObservedReader<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let requested = output.len();
                let returned = self.input.read(output)?;
                self.events.borrow_mut().push(Event::Read {
                    requested,
                    returned,
                });
                Ok(returned)
            }
        }

        struct ObservedWriter<'a> {
            output: Vec<u8>,
            events: &'a RefCell<Vec<Event>>,
        }

        impl Write for ObservedWriter<'_> {
            fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
                self.events.borrow_mut().push(Event::Write(input.len()));
                self.output.extend_from_slice(input);
                Ok(input.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let input: Vec<u8> = (0..FILE_CHUNK_BYTES * 2 + 17)
            .map(|index| (index % 251) as u8)
            .collect();
        let events = RefCell::new(Vec::new());
        let mut reader = ObservedReader {
            input: std::io::Cursor::new(input.clone()),
            events: &events,
        };
        let mut writer = ObservedWriter {
            output: Vec::new(),
            events: &events,
        };

        copy_exact_in_chunks(&mut reader, &mut writer, input.len() as u64).expect("streamed copy");

        assert_eq!(writer.output, input);
        assert_eq!(
            events.into_inner(),
            vec![
                Event::Read {
                    requested: FILE_CHUNK_BYTES,
                    returned: FILE_CHUNK_BYTES,
                },
                Event::Write(FILE_CHUNK_BYTES),
                Event::Read {
                    requested: FILE_CHUNK_BYTES,
                    returned: FILE_CHUNK_BYTES,
                },
                Event::Write(FILE_CHUNK_BYTES),
                Event::Read {
                    requested: 17,
                    returned: 17,
                },
                Event::Write(17),
            ]
        );
    }

    #[test]
    fn streamed_copy_retries_an_interrupted_read() {
        struct InterruptOnce {
            interrupted: bool,
            input: std::io::Cursor<Vec<u8>>,
        }

        impl Read for InterruptOnce {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                self.input.read(output)
            }
        }

        let expected = b"complete after interruption".to_vec();
        let mut reader = InterruptOnce {
            interrupted: false,
            input: std::io::Cursor::new(expected.clone()),
        };
        let mut output = Vec::new();
        copy_exact_in_chunks(&mut reader, &mut output, expected.len() as u64)
            .expect("interrupted read must retry");
        assert_eq!(output, expected);
    }

    #[cfg(unix)]
    #[test]
    fn response_deadline_releases_a_stalled_stream_and_its_budget() {
        use std::os::fd::AsRawFd;
        use std::sync::mpsc;

        fn set_socket_buffer(stream: &TcpStream, option: libc::c_int, bytes: libc::c_int) {
            // SAFETY: the socket descriptor and pointer to the integer option
            // stay valid for the duration of setsockopt.
            let result = unsafe {
                libc::setsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    std::ptr::from_ref(&bytes).cast(),
                    std::mem::size_of_val(&bytes) as libc::socklen_t,
                )
            };
            assert_eq!(
                result,
                0,
                "set socket buffer: {}",
                std::io::Error::last_os_error()
            );
        }

        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("response-deadline");
        let file_len = 16 * 1024 * 1024;
        fixture.set_len("long.wav", file_len);
        let root = Arc::new(ServedRoot::open(&fixture.path).expect("served root"));
        let origins = default_policy();
        let budget = Arc::new(InFlightBudget::new(file_len));
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener");
        let address = listener.local_addr().expect("test address");
        let hosts = HostPolicy::new("127.0.0.1", address.port()).expect("test hosts");
        let timeouts = ConnectionTimeouts {
            request: Duration::from_secs(1),
            response: Duration::from_millis(40),
        };
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let root_for_server = Arc::clone(&root);
        let budget_for_server = Arc::clone(&budget);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("test accept");
            set_socket_buffer(&stream, libc::SO_SNDBUF, 4 * 1024);
            let started = Instant::now();
            let result = handle(
                stream,
                &root_for_server,
                &origins,
                &hosts,
                &budget_for_server,
                ServerLimits {
                    scan: SampleScanLimits::DEFAULT,
                    file_bytes: file_len,
                    timeouts,
                },
            );
            finished_tx
                .send((result.map_err(|error| error.kind()), started.elapsed()))
                .expect("send result");
        });

        let mut client = TcpStream::connect(address).expect("test connect");
        set_socket_buffer(&client, libc::SO_RCVBUF, 4 * 1024);
        write!(client, "GET /long.wav HTTP/1.1\r\nHost: {address}\r\n\r\n").expect("test request");
        client
            .shutdown(std::net::Shutdown::Write)
            .expect("finish request");

        let (result, elapsed) = finished_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("stalled response held its handler");
        assert!(
            matches!(
                result,
                Err(std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
            ),
            "unexpected stalled response result: {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "deadline took {elapsed:?}"
        );
        drop(client);
        server.join().expect("test server");
        assert_eq!(budget.used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn the_shared_body_budget_allows_its_exact_limit_and_refuses_contention() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("budget");
        fixture.write("one.wav", b"x");
        let budget = InFlightBudget::new(8);

        let seven = budget.acquire(7).expect("seven-byte peer");
        let exact = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /one.wav HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&exact), 200);
        drop(seven);

        let all = budget.acquire(8).expect("full peer");
        let refused = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET /one.wav HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&refused), 503);
        drop(all);
        assert_eq!(budget.used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn manifest_entry_and_byte_limits_refuse_before_serialization() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("manifest-limits");
        fixture.write("bank/one.wav", b"1");
        fixture.write("bank/two.wav", b"2");
        let budget = manifest_budget();

        let one_examined = SampleScanLimits {
            examined_entries: 1,
            manifest_entries: 16,
            manifest_bytes: MAX_SAMPLE_MANIFEST_BYTES,
            ..SampleScanLimits::DEFAULT
        };
        let examined = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            one_examined,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&examined), 413);

        let one_entry = SampleScanLimits {
            examined_entries: 16,
            manifest_entries: 1,
            manifest_bytes: MAX_SAMPLE_MANIFEST_BYTES,
            ..SampleScanLimits::DEFAULT
        };
        let entries = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            one_entry,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&entries), 413);

        let exact_entries = SampleScanLimits {
            examined_entries: 16,
            manifest_entries: 2,
            manifest_bytes: MAX_SAMPLE_MANIFEST_BYTES,
            ..SampleScanLimits::DEFAULT
        };
        let entries = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            exact_entries,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&entries), 200);

        let tiny_body = SampleScanLimits {
            examined_entries: 16,
            manifest_entries: 16,
            manifest_bytes: 8,
            ..SampleScanLimits::DEFAULT
        };
        let bytes = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            tiny_body,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&bytes), 413);

        let root = ServedRoot::open(&fixture.path).expect("served root");
        let baseline = wire_manifest(&root, SampleScanLimits::DEFAULT).expect("baseline manifest");
        let exact_bytes = SampleScanLimits {
            examined_entries: 16,
            manifest_entries: 16,
            manifest_bytes: baseline.len(),
            ..SampleScanLimits::DEFAULT
        };
        let bytes = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            exact_bytes,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&bytes), 200);
        assert_eq!(body(&bytes), baseline);
        let under_bytes = SampleScanLimits {
            manifest_bytes: baseline.len() - 1,
            ..exact_bytes
        };
        let bytes = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            under_bytes,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&bytes), 413);
    }

    #[test]
    fn manifest_budget_includes_scanner_workspace_before_the_scan() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("manifest-work-budget");
        fixture.write("tone.wav", b"RIFF");
        let limits = SampleScanLimits {
            examined_entries: 1,
            manifest_entries: 1,
            manifest_bytes: 128,
            working_bytes: 2_048,
        };
        let required = (limits.manifest_bytes + limits.working_bytes) as u64;

        let short = InFlightBudget::new(required - 1);
        let refused = exchange(
            &fixture.path,
            &default_policy(),
            &short,
            limits,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&refused), 503);
        assert_eq!(short.used.load(Ordering::Acquire), 0);

        let exact = InFlightBudget::new(required);
        let accepted = exchange(
            &fixture.path,
            &default_policy(),
            &exact,
            limits,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&accepted), 200);
        assert_eq!(exact.used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn public_scanner_canonicalizes_its_root() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("noncanonical-scan-root");
        fixture.write("tone.wav", b"RIFF");
        std::fs::create_dir(fixture.path.join("marker")).expect("marker directory");
        let noncanonical = fixture.path.join("marker").join("..");
        let banks = crate::samples::scan_sample_folder(&noncanonical).expect("scan");
        assert_eq!(banks.values().map(Vec::len).sum::<usize>(), 1);
        assert!(banks.values().flatten().any(|path| path == "tone.wav"));
    }

    #[cfg(unix)]
    #[test]
    fn served_root_refuses_a_regular_file_swapped_in_after_validation() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("root-file-swap");
        fixture.write("tone.wav", b"RIFF");
        let moved = fixture.path.with_extension("validated-directory");
        let _ = std::fs::remove_dir_all(&moved);

        let result = ServedRoot::open_with(&fixture.path, |validated| {
            std::fs::rename(validated, &moved).expect("move validated directory");
            std::fs::write(validated, b"not a directory").expect("replacement file");
        });
        assert!(result.is_err(), "a replacement file became the served root");

        std::fs::remove_file(&fixture.path).expect("remove replacement file");
        std::fs::rename(&moved, &fixture.path).expect("restore fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn served_root_refuses_a_symlink_swapped_in_after_validation() {
        use std::os::unix::fs::symlink;

        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("root-symlink-swap");
        fixture.write("tone.wav", b"RIFF");
        let moved = fixture.path.with_extension("validated-directory");
        let _ = std::fs::remove_dir_all(&moved);

        let result = ServedRoot::open_with(&fixture.path, |validated| {
            std::fs::rename(validated, &moved).expect("move validated directory");
            symlink(&moved, validated).expect("replacement symlink");
        });
        assert!(
            result.is_err(),
            "a replacement symlink became the served root"
        );

        std::fs::remove_file(&fixture.path).expect("remove replacement symlink");
        std::fs::rename(&moved, &fixture.path).expect("restore fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn served_root_refuses_a_fifo_swapped_in_after_validation() {
        use std::os::unix::ffi::OsStrExt;

        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("root-fifo-swap");
        fixture.write("tone.wav", b"RIFF");
        let moved = fixture.path.with_extension("validated-directory");
        let _ = std::fs::remove_dir_all(&moved);

        let started = Instant::now();
        let result = ServedRoot::open_with(&fixture.path, |validated| {
            std::fs::rename(validated, &moved).expect("move validated directory");
            let path = std::ffi::CString::new(validated.as_os_str().as_bytes())
                .expect("fixture path without NUL");
            // SAFETY: `path` is NUL-terminated and names a new fixture FIFO.
            let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
            assert_eq!(
                result,
                0,
                "create replacement FIFO: {}",
                std::io::Error::last_os_error()
            );
        });
        assert!(result.is_err(), "a replacement FIFO became the served root");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "opening the replacement FIFO blocked"
        );

        std::fs::remove_file(&fixture.path).expect("remove replacement FIFO");
        std::fs::rename(&moved, &fixture.path).expect("restore fixture directory");
    }

    #[test]
    fn internal_scanner_paths_are_not_sent_to_remote_clients() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("private-errors");
        let budget = manifest_budget();
        let response = exchange(
            &fixture.path,
            &default_policy(),
            &budget,
            SampleScanLimits::DEFAULT,
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(status(&response), 500);
        assert!(!String::from_utf8_lossy(&response).contains(&fixture.path.display().to_string()));
        assert_eq!(body(&response), b"sample manifest unavailable");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_swap_between_metadata_and_read_open_is_refused() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("swap");
        fixture.write("swap.wav", b"first");
        fixture.write("replacement.wav", b"second");
        let root = ServedRoot::open(&fixture.path).expect("served root");
        let prepared = root
            .prepare_sample(Path::new("swap.wav"))
            .expect("prepared sample");
        std::fs::rename(fixture.path.join("swap.wav"), fixture.path.join("old.wav"))
            .expect("move original");
        std::fs::rename(
            fixture.path.join("replacement.wav"),
            fixture.path.join("swap.wav"),
        )
        .expect("install replacement");
        assert!(matches!(
            prepared.open_read(&root),
            Err(OpenSampleError::NotFound)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn portable_unix_fallback_is_handle_relative_and_retains_the_opened_file() {
        use std::os::unix::fs::symlink;

        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("unix-fallback");
        fixture.write("kit/tone.wav", b"first");
        fixture.write("kit/replacement.wav", b"second");
        symlink("tone.wav", fixture.path.join("kit/link.wav")).expect("file symlink");
        symlink("kit", fixture.path.join("linked-kit")).expect("directory symlink");
        let root = ServedRoot::open(&fixture.path).expect("served root");

        assert!(root.prepare_unix(Path::new("kit/link.wav")).is_err());
        assert!(root.prepare_unix(Path::new("linked-kit/tone.wav")).is_err());

        let prepared = root
            .prepare_unix(Path::new("kit/tone.wav"))
            .expect("direct file");
        std::fs::rename(
            fixture.path.join("kit/tone.wav"),
            fixture.path.join("kit/original.wav"),
        )
        .expect("move original");
        std::fs::rename(
            fixture.path.join("kit/replacement.wav"),
            fixture.path.join("kit/tone.wav"),
        )
        .expect("install replacement");
        let mut opened = prepared.open_read(&root).expect("retained handle").file;
        let mut bytes = Vec::new();
        opened
            .read_to_end(&mut bytes)
            .expect("read retained handle");
        assert_eq!(bytes, b"first");
    }

    #[test]
    fn the_manifest_has_sampler_compatible_leading_slashes() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let fixture = Fixture::new("manifest-wire");
        fixture.write("kit/kick.wav", b"RIFF");
        fixture.write("loose.wav", b"RIFF");
        let root = ServedRoot::open(&fixture.path).expect("served root");
        let bytes = wire_manifest(&root, SampleScanLimits::DEFAULT).expect("manifest");
        let banks: std::collections::BTreeMap<String, Vec<String>> =
            serde_json::from_slice(&bytes).expect("manifest JSON");
        for paths in banks.values() {
            for path in paths {
                assert!(path.starts_with('/'));
                assert!(!path.starts_with("//"));
            }
        }
        assert_eq!(banks["kit"], vec!["/kit/kick.wav".to_owned()]);
    }

    #[test]
    fn percent_escapes_decode_so_a_space_in_a_name_resolves() {
        assert_eq!(percent_decode("my%20kit/01.wav"), "my kit/01.wav");
        assert_eq!(percent_decode("plain.wav"), "plain.wav");
        assert_eq!(percent_decode("100%.wav"), "100%.wav");
    }

    #[test]
    fn advertised_authority_uses_the_bound_port_and_brackets_ipv6() {
        assert_eq!(
            advertised_url("127.0.0.1", 41_337),
            "http://127.0.0.1:41337"
        );
        assert_eq!(advertised_url("::1", 41_337), "http://[::1]:41337");
    }

    #[test]
    fn bounded_manifest_strings_match_json_escaping() {
        for value in ["plain.wav", "quote\".wav", "slash\\.wav", "line\ncafé.wav"] {
            let expected = serde_json::to_vec(&format!("/{value}")).expect("JSON string");
            let mut actual = BoundedBody::new(expected.len()).expect("bounded body");
            write_json_string(&mut actual, Some('/'), value).expect("bounded string");
            assert_eq!(actual.into_inner(), expected);
        }
    }

    #[test]
    fn handler_slots_release_on_every_exit() {
        let slots = HandlerSlots::new(2);
        let first = slots.acquire().expect("first slot");
        let second = slots.acquire().expect("second slot");
        assert!(slots.acquire().is_none());
        drop(first);
        assert!(slots.acquire().is_some());
        drop(second);
    }
}
