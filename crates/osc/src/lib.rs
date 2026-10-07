/*
lib.rs - OSC output over UDP
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! OSC output, aimed at SuperDirt but useful to anything that speaks OSC.
//!
//! Sends UDP datagrams directly. Each onset is sent ahead of playback in an
//! OSC bundle carrying an NTP timetag, so a receiver such as SuperCollider
//! can schedule it on its own clock. This avoids needing a local thread to
//! wait until each onset is due.
//!
//! Nothing here may end a set: an unreachable host, an oversized datagram or a
//! refused send is counted and dropped.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// SuperDirt's default listening port.
pub const DEFAULT_OSC_PORT: u16 = 57120;
pub const DEFAULT_OSC_HOST: &str = "127.0.0.1";

/// Where SuperDirt expects playable events.
pub const DIRT_PLAY: &str = "/dirt/play";

/// Seconds between the NTP epoch (1900-01-01) and the Unix epoch (1970-01-01).
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// A datagram larger than this is dropped rather than sent.
///
/// Well inside the practical UDP limit; a `/dirt/play` message is normally a
/// few hundred bytes, so anything approaching this is a runaway score rather
/// than music.
pub const MAX_DATAGRAM_BYTES: usize = 8192;
/// Host text is an IP literal or a short RFC 6761 localhost name. Bound it
/// before trimming or formatting so a score cannot copy megabytes merely to
/// have the destination refused.
pub const MAX_OSC_HOST_BYTES: usize = 255;

/// Parse an OSC host without touching DNS.
///
/// The live loop sends from the producer thread. `ToSocketAddrs` would call
/// `getaddrinfo` there and stall scheduling for a hanging name, so only IP
/// literals (and the RFC 6761 `localhost` names, mapped to `127.0.0.1`) are
/// accepted.
pub fn parse_osc_ip(host: &str) -> Result<IpAddr, String> {
    if host.len() > MAX_OSC_HOST_BYTES {
        return Err(format!(
            "OSC host exceeds the {MAX_OSC_HOST_BYTES}-byte limit"
        ));
    }
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    if host_names_loopback(host) {
        return Ok(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    Err(format!(
        "OSC host {host:?} is not an IP address; names are not resolved during playback"
    ))
}

/// Bind `host` and `port` into a destination, still without DNS.
pub fn parse_osc_destination(host: &str, port: u16) -> Result<SocketAddr, String> {
    Ok(SocketAddr::new(parse_osc_ip(host)?, port))
}

/// Loopback including IPv4-mapped `:ffff:127.0.0.1`.
pub fn ip_is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
    }
}

/// Addresses that are never a SuperDirt listener: unspecified, multicast,
/// IPv4 broadcast.
pub fn ip_is_unsendable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast(),
        IpAddr::V6(ip) => {
            let mapped = ip.to_ipv4_mapped();
            ip.is_unspecified()
                || ip.is_multicast()
                || mapped.is_some_and(|mapped| {
                    mapped.is_unspecified() || mapped.is_multicast() || mapped.is_broadcast()
                })
        }
    }
}

fn host_names_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // RFC 6761 section 6.3 also reserves `<name>.localhost`. Split instead of
    // byte-slicing: a score-controlled string may contain multi-byte UTF-8 and
    // a misaligned index would panic the producer thread.
    match host.rsplit_once('.') {
        Some((prefix, suffix)) => {
            !prefix.is_empty() && !prefix.ends_with('.') && suffix.eq_ignore_ascii_case("localhost")
        }
        None => false,
    }
}

/// One OSC argument.
#[derive(Clone, Debug, PartialEq)]
pub enum OscValue {
    Int(i32),
    Float(f32),
    Str(String),
}

impl OscValue {
    fn tag(&self) -> u8 {
        match self {
            Self::Int(_) => b'i',
            Self::Float(_) => b'f',
            Self::Str(_) => b's',
        }
    }
}

/// OSC pads every string and blob to a four-byte boundary, and a string is
/// always followed by at least one nul.
fn push_padded_str(out: &mut Vec<u8>, text: &str) {
    // A nul inside an OSC string would terminate it early on the receiver and
    // desynchronise every argument after it, so drop them here.
    out.extend(text.bytes().filter(|byte| *byte != 0));
    out.push(0);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn padded_str_len(text: &str) -> Option<usize> {
    let bytes = text.bytes().filter(|byte| *byte != 0).count();
    bytes.checked_add(1)?.checked_add(3).map(|len| len & !3)
}

fn message_encoded_len(address: &str, args: &[OscValue]) -> Option<usize> {
    let mut len = padded_str_len(address)?;
    len = len.checked_add((args.len().checked_add(2)?.checked_add(3)?) & !3)?;
    for arg in args {
        len = len.checked_add(match arg {
            OscValue::Int(_) | OscValue::Float(_) => 4,
            OscValue::Str(value) => padded_str_len(value)?,
        })?;
    }
    Some(len)
}

/// Exact encoded size of one `/dirt/play` bundle, without allocating it.
pub fn dirt_bundle_encoded_len(args: &[OscValue]) -> Option<usize> {
    message_encoded_len(DIRT_PLAY, args)?.checked_add(20)
}

fn push_message(out: &mut Vec<u8>, address: &str, args: &[OscValue]) {
    push_padded_str(out, address);

    let mut tags = String::with_capacity(args.len() + 1);
    tags.push(',');
    for arg in args {
        tags.push(char::from(arg.tag()));
    }
    push_padded_str(out, &tags);

    for arg in args {
        match arg {
            OscValue::Int(value) => out.extend(value.to_be_bytes()),
            OscValue::Float(value) => out.extend(value.to_be_bytes()),
            OscValue::Str(value) => push_padded_str(out, value),
        }
    }
}

/// Encode one OSC message: address, type tag string, then the arguments.
pub fn encode_message(address: &str, args: &[OscValue]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + args.len() * 8);
    push_message(&mut out, address, args);
    out
}

/// A 64-bit NTP timetag: seconds since 1900 in the high half, fraction in the
/// low half.
pub fn ntp_timetag(when: SystemTime) -> u64 {
    let since_epoch = when
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0));
    let seconds = since_epoch.as_secs().saturating_add(NTP_UNIX_OFFSET);
    // The fraction is in units of 1/2^32 of a second.
    let fraction = (since_epoch.subsec_nanos() as u64 * (1u64 << 32)) / 1_000_000_000;
    (seconds << 32) | (fraction & 0xffff_ffff)
}

/// One message read back out of a datagram.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedMessage {
    pub address: String,
    pub args: Vec<OscValue>,
}

/// A bundle read back out of a datagram: its timetag and its messages.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedBundle {
    pub timetag: u64,
    pub messages: Vec<DecodedMessage>,
}

impl DecodedBundle {
    /// The timetag as seconds since the Unix epoch.
    pub fn unix_seconds(&self) -> f64 {
        let seconds = (self.timetag >> 32) as f64 - NTP_UNIX_OFFSET as f64;
        seconds + (self.timetag & 0xffff_ffff) as f64 / 4_294_967_296.0
    }

    /// The pairs a `/dirt/play` message carries, name then value.
    pub fn dirt_pairs(&self) -> Vec<(String, OscValue)> {
        self.messages
            .iter()
            .filter(|message| message.address == "/dirt/play")
            .flat_map(|message| {
                message.args.chunks(2).filter_map(|pair| match pair {
                    [OscValue::Str(name), value] => Some((name.clone(), value.clone())),
                    _ => None,
                })
            })
            .collect()
    }
}

/// Read a bundle back: the reverse of [`encode_bundle`] over
/// [`encode_message`], for a listener that wants to know what was sent -
/// a test, a probe. Only the types this crate writes (`s`, `f`, `i`) are
/// read; a datagram that is not a bundle, or holds anything else, is
/// `None`.
pub fn decode_bundle(datagram: &[u8]) -> Option<DecodedBundle> {
    let rest = datagram.strip_prefix(b"#bundle\0")?;
    let timetag = u64::from_be_bytes(rest.get(..8)?.try_into().ok()?);
    let mut rest = &rest[8..];
    let mut messages = Vec::new();
    while !rest.is_empty() {
        let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        let element = rest.get(4..4 + len)?;
        messages.push(decode_message(element)?);
        rest = &rest[4 + len..];
    }
    Some(DecodedBundle { timetag, messages })
}

fn decode_message(bytes: &[u8]) -> Option<DecodedMessage> {
    let (address, rest) = decode_string(bytes)?;
    let (tags, mut rest) = decode_string(rest)?;
    let mut args = Vec::new();
    for tag in tags.strip_prefix(',')?.chars() {
        match tag {
            's' => {
                let (text, after) = decode_string(rest)?;
                args.push(OscValue::Str(text));
                rest = after;
            }
            'f' => {
                args.push(OscValue::Float(f32::from_be_bytes(
                    rest.get(..4)?.try_into().ok()?,
                )));
                rest = &rest[4..];
            }
            'i' => {
                args.push(OscValue::Int(i32::from_be_bytes(
                    rest.get(..4)?.try_into().ok()?,
                )));
                rest = &rest[4..];
            }
            _ => return None,
        }
    }
    Some(DecodedMessage { address, args })
}

/// A NUL-terminated string padded to four bytes, and what follows it.
fn decode_string(bytes: &[u8]) -> Option<(String, &[u8])> {
    let end = bytes.iter().position(|byte| *byte == 0)?;
    let text = std::str::from_utf8(&bytes[..end]).ok()?.to_owned();
    let padded = (end + 4) & !3;
    Some((text, bytes.get(padded..)?))
}

/// Wrap one encoded message in a bundle carrying an NTP `timetag`.
/// A receiver that supports timetags can hold the message until it is due.
pub fn encode_bundle(timetag: u64, message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + message.len() + 4);
    push_padded_str(&mut out, "#bundle");
    out.extend(timetag.to_be_bytes());
    out.extend((message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    out
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OscReport {
    pub sent: u64,
    /// Datagrams the socket refused - usually nothing listening yet.
    pub send_errors: u64,
    /// Messages dropped for exceeding [`MAX_DATAGRAM_BYTES`].
    pub dropped_oversize: u64,
}

/// One bound UDP socket, reused for every destination.
pub struct OscSender {
    socket_v4: UdpSocket,
    socket_v6: Option<UdpSocket>,
    sent: AtomicU64,
    send_errors: AtomicU64,
    dropped_oversize: AtomicU64,
}

impl OscSender {
    /// Bind an ephemeral local port for sending.
    ///
    /// `0.0.0.0:0` rather than a fixed port: nothing replies to us, and
    /// claiming a known port would collide with another instance.
    pub fn new() -> Result<Self, String> {
        let socket_v4 = UdpSocket::bind("0.0.0.0:0")
            .map_err(|error| format!("could not open a UDP socket for OSC: {error}"))?;
        // A send must never block the scheduler, even briefly.
        socket_v4
            .set_nonblocking(true)
            .map_err(|error| format!("could not set the OSC socket non-blocking: {error}"))?;
        // A separate IPv6 socket makes the documented `::1` route real while
        // preserving startup on hosts where IPv6 has been disabled entirely.
        let socket_v6 = UdpSocket::bind("[::]:0").ok().and_then(|socket| {
            socket.set_nonblocking(true).ok()?;
            Some(socket)
        });
        Ok(Self {
            socket_v4,
            socket_v6,
            sent: AtomicU64::new(0),
            send_errors: AtomicU64::new(0),
            dropped_oversize: AtomicU64::new(0),
        })
    }

    /// Send one already-encoded datagram.
    ///
    /// Best effort by design: OSC is UDP, and a dropped packet is a missed
    /// note, not a reason to stop playing. `destination` is a resolved
    /// [`SocketAddr`] so this path never blocks on DNS.
    pub fn send_raw(&self, destination: SocketAddr, datagram: &[u8]) -> bool {
        if datagram.len() > MAX_DATAGRAM_BYTES {
            self.dropped_oversize.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let socket = if destination.is_ipv4() {
            Some(&self.socket_v4)
        } else {
            self.socket_v6.as_ref()
        };
        let Some(socket) = socket else {
            self.send_errors.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        match socket.send_to(datagram, destination) {
            Ok(_) => {
                self.sent.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(_) => {
                self.send_errors.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Encode `args` as a `/dirt/play` message, bundle it for `when`, and send.
    pub fn send_dirt(&self, destination: SocketAddr, when: SystemTime, args: &[OscValue]) -> bool {
        let Some(encoded_len) = dirt_bundle_encoded_len(args) else {
            self.dropped_oversize.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        if encoded_len > MAX_DATAGRAM_BYTES {
            self.dropped_oversize.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        // Encode directly into the final bundle. The old path allocated the
        // message and then copied it into a second buffer before checking the
        // wire limit.
        let message_len = encoded_len - 20;
        let mut bundle = Vec::with_capacity(encoded_len);
        push_padded_str(&mut bundle, "#bundle");
        bundle.extend(ntp_timetag(when).to_be_bytes());
        bundle.extend((message_len as u32).to_be_bytes());
        push_message(&mut bundle, DIRT_PLAY, args);
        debug_assert_eq!(bundle.len(), encoded_len);
        self.send_raw(destination, &bundle)
    }

    pub fn report(&self) -> OscReport {
        OscReport {
            sent: self.sent.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            dropped_oversize: self.dropped_oversize.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_nul_terminated_and_padded_to_four_bytes() {
        let mut out = Vec::new();
        push_padded_str(&mut out, "abc");
        assert_eq!(out, b"abc\0", "3 chars + 1 nul is already aligned");

        let mut out = Vec::new();
        push_padded_str(&mut out, "abcd");
        assert_eq!(out, b"abcd\0\0\0\0", "4 chars needs a full pad word");

        let mut out = Vec::new();
        push_padded_str(&mut out, "");
        assert_eq!(out, b"\0\0\0\0");
    }

    /// A nul inside a string would terminate it early on the receiver and
    /// desynchronise every argument after it.
    #[test]
    fn embedded_nuls_are_stripped_rather_than_desynchronising_the_message() {
        let mut out = Vec::new();
        push_padded_str(&mut out, "a\0b");
        assert_eq!(out, b"ab\0\0");
    }

    #[test]
    fn a_message_is_address_then_typetags_then_args() {
        let encoded = encode_message(
            "/dirt/play",
            &[OscValue::Str("s".into()), OscValue::Float(1.0)],
        );
        assert!(encoded.starts_with(b"/dirt/play\0\0"), "{encoded:?}");
        // ",sf" plus its nul pads to four bytes.
        let tags_at = 12;
        assert_eq!(&encoded[tags_at..tags_at + 4], b",sf\0");
        // The float is big-endian IEEE-754.
        assert_eq!(&encoded[encoded.len() - 4..], &1.0f32.to_be_bytes());
        assert_eq!(encoded.len() % 4, 0, "every OSC packet is 4-byte aligned");
    }

    #[test]
    fn a_bundle_carries_the_hash_bundle_tag_a_timetag_and_a_sized_element() {
        let message = encode_message("/x", &[]);
        let bundle = encode_bundle(0x0000_0001_8000_0000, &message);
        assert!(bundle.starts_with(b"#bundle\0"));
        assert_eq!(&bundle[8..16], &0x0000_0001_8000_0000u64.to_be_bytes());
        assert_eq!(&bundle[16..20], &(message.len() as u32).to_be_bytes());
        assert_eq!(&bundle[20..], &message[..]);
    }

    #[test]
    fn the_timetag_counts_from_1900_not_1970() {
        // The Unix epoch itself is exactly the NTP offset, with no fraction.
        let tag = ntp_timetag(UNIX_EPOCH);
        assert_eq!(tag >> 32, NTP_UNIX_OFFSET);
        assert_eq!(tag & 0xffff_ffff, 0);

        // Half a second in sets the top bit of the fraction.
        let tag = ntp_timetag(UNIX_EPOCH + Duration::from_millis(500));
        assert_eq!(tag >> 32, NTP_UNIX_OFFSET);
        assert_eq!(tag & 0xffff_ffff, 1u64 << 31);
    }

    /// A time before 1970 cannot be represented and must not panic on the
    /// live path.
    #[test]
    fn a_time_before_the_epoch_does_not_panic() {
        let before = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(ntp_timetag(before) >> 32, NTP_UNIX_OFFSET);
    }

    fn loopback_superdirt() -> SocketAddr {
        parse_osc_destination(DEFAULT_OSC_HOST, DEFAULT_OSC_PORT).expect("loopback")
    }

    #[test]
    fn an_oversized_datagram_is_dropped_rather_than_sent() {
        let sender = OscSender::new().expect("bind");
        assert!(!sender.send_raw(loopback_superdirt(), &vec![0u8; MAX_DATAGRAM_BYTES + 1],));
        assert_eq!(sender.report().dropped_oversize, 1);
        assert_eq!(sender.report().sent, 0);
    }

    #[test]
    fn dirt_size_is_preflighted_before_allocating_the_packet() {
        for args in [
            vec![],
            vec![OscValue::Int(1), OscValue::Float(2.0)],
            vec![OscValue::Str(String::new())],
            vec![OscValue::Str("a\0bc\0d".into())],
            vec![OscValue::Str("s".into()), OscValue::Str("bd".into())],
        ] {
            let message = encode_message(DIRT_PLAY, &args);
            let bundle = encode_bundle(0, &message);
            assert_eq!(dirt_bundle_encoded_len(&args), Some(bundle.len()));
        }

        let sender = OscSender::new().expect("bind");
        let exact = [OscValue::Str("x".repeat(MAX_DATAGRAM_BYTES - 37))];
        assert_eq!(dirt_bundle_encoded_len(&exact), Some(MAX_DATAGRAM_BYTES));
        assert!(sender.send_dirt(loopback_superdirt(), SystemTime::now(), &exact));

        let one_byte_over = [OscValue::Str("x".repeat(MAX_DATAGRAM_BYTES - 36))];
        assert!(
            dirt_bundle_encoded_len(&one_byte_over).is_some_and(|len| len > MAX_DATAGRAM_BYTES)
        );
        assert!(!sender.send_dirt(loopback_superdirt(), SystemTime::now(), &one_byte_over));
        let huge = [OscValue::Str("x".repeat(MAX_DATAGRAM_BYTES * 2))];
        assert!(!sender.send_dirt(loopback_superdirt(), SystemTime::now(), &huge));
        assert_eq!(sender.report().dropped_oversize, 2);
        assert_eq!(sender.report().sent, 1);
    }

    #[test]
    fn the_sender_uses_an_ipv6_socket_when_the_host_supports_one() {
        let receiver = match UdpSocket::bind("[::1]:0") {
            Ok(receiver) => receiver,
            Err(_) => return,
        };
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("timeout");
        let sender = OscSender::new().expect("bind");
        assert!(
            sender.socket_v6.is_some(),
            "IPv6 loopback is available but the sender did not bind IPv6"
        );
        let destination = receiver.local_addr().expect("IPv6 destination");
        assert!(sender.send_raw(destination, b"x"));
        let mut received = [0u8; 1];
        assert_eq!(receiver.recv(&mut received).expect("receive"), 1);
        assert_eq!(received, *b"x");
        assert_eq!(sender.report().sent, 1);
    }

    #[test]
    fn a_hostname_is_refused_without_calling_dns() {
        let error = parse_osc_destination("no-such-host.invalid", DEFAULT_OSC_PORT)
            .expect_err("a name must not reach getaddrinfo");
        assert!(error.contains("not an IP address"), "{error}");
        assert!(parse_osc_ip("localhost").expect("localhost").is_loopback());
        assert_eq!(
            parse_osc_ip("127.0.0.1").expect("loopback v4"),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        assert!(parse_osc_ip("::1").expect("loopback v6").is_loopback());
        assert!(ip_is_loopback(
            parse_osc_ip("::ffff:127.0.0.1").expect("mapped loopback")
        ));
        assert!(ip_is_unsendable("0.0.0.0".parse().unwrap()));
        assert!(ip_is_unsendable("255.255.255.255".parse().unwrap()));
        assert!(ip_is_unsendable("224.0.0.1".parse().unwrap()));
        let oversized = "x".repeat(MAX_OSC_HOST_BYTES + 1);
        let error = parse_osc_ip(&oversized).expect_err("oversized host");
        assert!(error.contains("byte limit"), "{error}");
    }

    #[test]
    fn a_multibyte_localhost_name_is_refused_without_panicking() {
        // The old implementation byte-sliced at `len - 10`, which panics when
        // that index falls inside a multi-byte character. Score text controls
        // this string, so a panic here would end the live set.
        for hostile in ["üüüülocalhost", "スsuperdirt", "locaLHOSTü", "localhostü"] {
            let error =
                parse_osc_ip(hostile).expect_err("multi-byte names must be refused, not panicked");
            assert!(error.contains("not an IP address"), "{error}");
        }
        for name in ["foo.localhost", "superdirt.LOCALHOST", "ス.localhost"] {
            assert!(
                parse_osc_ip(name)
                    .expect("RFC 6761 subdomains are loopback")
                    .is_loopback(),
                "{name}"
            );
        }
        assert!(parse_osc_ip(".localhost").is_err());
        assert!(parse_osc_ip("foo..localhost").is_err());
        assert!(parse_osc_ip("localhost.").is_err());
        assert!(parse_osc_ip("notlocalhost").is_err());
    }

    /// A local UDP listener checks the encoded OSC bytes and bundle timestamp
    /// through the real send path. This requires no external device.
    #[test]
    fn a_dirt_message_arrives_on_the_wire_intact() {
        let listener = UdpSocket::bind("127.0.0.1:0").expect("bind listener");
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let destination = listener.local_addr().expect("addr");

        let sender = OscSender::new().expect("bind sender");
        let when = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        sender.send_dirt(
            destination,
            when,
            &[
                OscValue::Str("s".into()),
                OscValue::Str("bd".into()),
                OscValue::Str("speed".into()),
                OscValue::Float(1.5),
                OscValue::Str("orbit".into()),
                OscValue::Int(2),
            ],
        );

        let mut buffer = [0u8; 2048];
        let (len, _) = listener.recv_from(&mut buffer).expect("datagram arrived");
        let packet = &buffer[..len];

        assert!(packet.starts_with(b"#bundle\0"), "not a bundle: {packet:?}");
        let timetag = u64::from_be_bytes(packet[8..16].try_into().unwrap());
        assert_eq!(
            timetag >> 32,
            1_700_000_000 + NTP_UNIX_OFFSET,
            "timetag was not the requested instant in NTP seconds"
        );

        let element_len = u32::from_be_bytes(packet[16..20].try_into().unwrap()) as usize;
        let message = &packet[20..20 + element_len];
        assert!(message.starts_with(b"/dirt/play\0"), "{message:?}");

        // The type tags describe exactly what we passed, in order.
        let tags_start = 12;
        assert_eq!(&message[tags_start..tags_start + 8], b",sssfsi\0");

        // And the values survive: 1.5 and 2 are recoverable big-endian.
        assert!(
            message
                .windows(4)
                .any(|window| window == 1.5f32.to_be_bytes()),
            "the float argument did not survive"
        );
        assert!(
            message
                .windows(4)
                .any(|window| window == 2i32.to_be_bytes()),
            "the int argument did not survive"
        );

        // Read back whole, it is what was sent, with the moment intact.
        let bundle = decode_bundle(packet).expect("a bundle this crate wrote reads back");
        assert!((bundle.unix_seconds() - 1_700_000_000.0).abs() < 1e-3);
        assert_eq!(
            bundle.dirt_pairs(),
            vec![
                ("s".to_owned(), OscValue::Str("bd".into())),
                ("speed".to_owned(), OscValue::Float(1.5)),
                ("orbit".to_owned(), OscValue::Int(2)),
            ]
        );
        assert_eq!(
            decode_bundle(b"/dirt/play\0\0,\0\0\0"),
            None,
            "a bare message is not a bundle"
        );
        assert_eq!(
            decode_bundle(&packet[..packet.len() - 1]),
            None,
            "a cut datagram"
        );
    }
}
