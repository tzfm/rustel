//! Ableton Link sync: one session on the local network, its tempo and
//! its transport, shared with whatever else speaks the protocol.
//!
//! The participant is an actor thread behind a cheap handle. It keeps
//! one UDP socket on the multicast group every participant listens on,
//! joined on every interface the machine has, loopback included, says
//! it is alive once a second, and answers everyone else's
//! announcements. Tempo is the session's own beat grid, carried in every
//! announcement; the grid a participant changes outranks the one it
//! heard, so a tempo someone sets reaches everyone. Transport is a
//! playing flag with a timestamp, and the newer decision wins. When
//! another session appears, its founder is measured with pings until
//! the two session clocks are known well enough to say which one to
//! stand in, and a participant that changes session takes the grid and
//! the clock of the one it joins.
//!
//! Shared by the live command and the studio, because both steer the
//! same scheduler with what the session says.

mod core;
mod wire;

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often a participant says it is alive.
const ALIVE_INTERVAL: Duration = Duration::from_secs(1);
/// A peer not heard from for this long has gone.
const PEER_LIFETIME: Duration = Duration::from_secs(5);
/// The least time between two responses, so an arrival burst cannot
/// flood the network with replies.
const RESPONSE_LIMIT: Duration = Duration::from_millis(50);
/// How long a measurement waits for a pong before it sends its ping
/// again. The ping after a pong goes out at once; this is only for loss.
const RETRY_INTERVAL: Duration = Duration::from_millis(100);
/// Pings gone unanswered before a target is given up on.
const MEASURE_RETRIES: u32 = 5;
/// Samples enough for a session clock to be decided on.
const MEASURE_SAMPLES: usize = 24;
/// A foreign session measured and not joined waits this long before it
/// is measured again.
const REMEASURE_AFTER: Duration = Duration::from_secs(30);
/// How long a target that never answered waits before it is asked
/// again. Loss is common and cheap to retry, unlike a decline.
const SILENT_RETRY_AFTER: Duration = Duration::from_secs(1);
/// How long a pacer may sleep between wake-ups. Every deadline in the
/// actor is far longer than this, so no deadline waits for another.
const WAKE: Duration = Duration::from_millis(100);

/// What the session says, as the poller reads it. `cycle` is where the
/// four-beat grid stands at the moment of the read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinkStatus {
    /// Other participants heard lately.
    pub peers: usize,
    /// The session's tempo in beats per minute.
    pub bpm: f64,
    /// Whether the session's transport runs.
    pub playing: bool,
    /// Cycles of four beats on the session grid, right now.
    pub cycle: f64,
}

/// The part of the status the actor writes and every reader computes
/// the rest from.
#[derive(Clone, Copy, Debug, Default)]
struct Shared {
    peers: usize,
    timeline: wire::Timeline,
    playing: bool,
    host_offset: i64,
}

enum Command {
    Tempo(f64),
    Playing(bool),
    Shutdown,
}

/// A handle on the actor thread. Dropping it tells the session goodbye.
pub struct Link {
    shared: Arc<Mutex<Shared>>,
    commands: Sender<Command>,
}

impl Link {
    /// Join the network's session, starting one of our own at `bpm` with
    /// the grid already standing at `cycle`. Fails only where the socket
    /// could not be had at all.
    pub fn start(bpm: f64, cycle: f64) -> io::Result<Self> {
        let socket = bind_socket()?;
        // Pings and pongs run on a port of this participant's own, not
        // the one every participant shares: two of them on one machine
        // would otherwise have their unicast shared out from under them.
        let measure_socket = UdpSocket::bind(("0.0.0.0", 0))?;
        let measure_v4 = local_address().map(|address| {
            let port = measure_socket
                .local_addr()
                .map(|address| address.port())
                .unwrap_or(0);
            (address, port)
        });
        let (commands, inbox) = channel();
        let node_id = random_id();
        let session = core::Session::found(node_id, bpm, cycle, host_micros(Instant::now()));
        let shared = Arc::new(Mutex::new(Shared {
            peers: 0,
            timeline: session.timeline,
            playing: session.start_stop.playing,
            host_offset: session.host_offset,
        }));
        let actor_shared = Arc::clone(&shared);
        let started = thread::Builder::new()
            .name("rustel-link".into())
            .spawn(move || {
                Actor::new(
                    socket,
                    measure_socket,
                    inbox,
                    actor_shared,
                    node_id,
                    measure_v4,
                    session,
                )
                .run();
            })
            .map_err(|error| {
                let _ = error;
                io::Error::other("the Link thread could not start")
            })?;
        let _ = started;
        Ok(Self { shared, commands })
    }

    /// What the session says now.
    pub fn status(&self) -> LinkStatus {
        let shared = *self
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ghost = host_micros(Instant::now()) + shared.host_offset;
        LinkStatus {
            peers: shared.peers,
            bpm: wire::bpm(shared.timeline.micros_per_beat),
            playing: shared.playing,
            cycle: core::cycle_of(&shared.timeline, ghost),
        }
    }

    /// Change the session's tempo from here.
    pub fn set_tempo(&self, bpm: f64) {
        let _ = self.commands.send(Command::Tempo(bpm));
    }

    /// Tell the session this machine started or stopped.
    pub fn set_playing(&self, playing: bool) {
        let _ = self.commands.send(Command::Playing(playing));
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

/// One other participant, as its announcements left it.
struct Peer {
    addr: SocketAddr,
    session_id: [u8; 8],
    timeline: Option<wire::Timeline>,
    start_stop: Option<wire::StartStop>,
    measure_v4: Option<([u8; 4], u16)>,
    last_seen: Instant,
}

impl Peer {
    /// Where this peer answers a ping: the endpoint it advertised, else
    /// the address its announcement came from.
    fn measure_endpoint(&self) -> SocketAddr {
        self.measure_v4
            .map(|(octets, port)| SocketAddrV4::new(Ipv4Addr::from(octets), port).into())
            .unwrap_or(self.addr)
    }

    fn is_founder_of(&self, session_id: [u8; 8]) -> bool {
        self.session_id == session_id
    }
}

/// The measurement under way: pings to one peer, and the clock samples
/// its pongs gave back.
struct Measure {
    peer: [u8; 8],
    session_id: [u8; 8],
    endpoint: SocketAddr,
    samples: Vec<i64>,
    /// Pings sent since the last pong came back.
    retries: u32,
    /// When an unanswered ping is sent again.
    next_ping: Instant,
}

struct Actor {
    socket: UdpSocket,
    measure_socket: UdpSocket,
    /// Every interface the session lives on: loopback for the other
    /// participants on this machine, and the network's route for the
    /// rest of the session.
    interfaces: Vec<Ipv4Addr>,
    group: SocketAddr,
    inbox: Receiver<Command>,
    shared: Arc<Mutex<Shared>>,
    node_id: [u8; 8],
    measure_v4: Option<([u8; 4], u16)>,
    session: core::Session,
    peers: HashMap<[u8; 8], Peer>,
    /// A foreign session measured lately and not joined.
    measured_elsewhere: HashMap<[u8; 8], Instant>,
    /// A session whose target stayed quiet, and when it may be asked
    /// again. Silence is lost packets or a busy wire, not a decision,
    /// so it comes due far sooner than a session declined.
    silent_until: HashMap<[u8; 8], Instant>,
    measure: Option<Measure>,
    next_alive: Instant,
    last_announce: Option<Instant>,
    /// Set whenever the state changed, so the next announcement goes out
    /// at once instead of waiting for the second hand.
    announce_now: bool,
}

impl Actor {
    fn new(
        socket: UdpSocket,
        measure_socket: UdpSocket,
        inbox: Receiver<Command>,
        shared: Arc<Mutex<Shared>>,
        node_id: [u8; 8],
        measure_v4: Option<([u8; 4], u16)>,
        session: core::Session,
    ) -> Self {
        let interfaces = session_interfaces();
        for interface in &interfaces {
            socket
                .join_multicast_v4(&Ipv4Addr::from(wire::GROUP), interface)
                .ok();
        }
        socket.set_multicast_loop_v4(true).ok();
        socket.set_read_timeout(Some(Duration::from_millis(1))).ok();
        Self {
            group: SocketAddrV4::new(Ipv4Addr::from(wire::GROUP), wire::PORT).into(),
            socket,
            interfaces,
            measure_socket,
            inbox,
            shared,
            node_id,
            measure_v4,
            session,
            peers: HashMap::new(),
            measured_elsewhere: HashMap::new(),
            silent_until: HashMap::new(),
            measure: None,
            next_alive: Instant::now(),
            last_announce: None,
            announce_now: true,
        }
    }

    fn run(mut self) {
        while self.turn() {}
        self.say_bye_bye();
    }

    /// One wake-up: whatever arrived, whatever came in from the handle,
    /// and whatever is due. False when the session is over.
    fn turn(&mut self) -> bool {
        let now = Instant::now();
        let next_deadline = [
            Some(self.next_alive),
            self.measure.as_ref().map(|m| m.next_ping),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(now + ALIVE_INTERVAL);
        // Sleep only as long as the next deadline allows, minus a
        // wake-up's worth so a deadline is never slept past.
        let wait = next_deadline
            .saturating_duration_since(now)
            .min(WAKE)
            .max(Duration::from_millis(1));
        let mut buffer = [0u8; wire::MAX_MESSAGE];
        // Discovery is read without waiting long: its timing is not
        // precise, and the turn must get to the measurement socket.
        self.socket
            .set_read_timeout(Some(Duration::from_millis(1)))
            .ok();
        if let Ok((len, from)) = self.socket.recv_from(&mut buffer) {
            self.hear(&buffer[..len], from, Instant::now());
        }
        // The measurement socket holds the rest of the wait, and a pong
        // wakes it the moment it lands.
        self.measure_socket.set_read_timeout(Some(wait)).ok();
        if let Ok((len, from)) = self.measure_socket.recv_from(&mut buffer) {
            self.hear(&buffer[..len], from, Instant::now());
        }
        while let Ok(command) = self.inbox.try_recv() {
            match command {
                Command::Tempo(bpm) => {
                    self.session.initiate_tempo(bpm, self.ghost_now());
                    self.announce_now = true;
                }
                Command::Playing(playing) => {
                    self.session.set_playing(playing, self.ghost_now());
                    self.announce_now = true;
                }
                Command::Shutdown => return false,
            }
        }
        self.maintain(Instant::now());
        true
    }

    fn hear(&mut self, datagram: &[u8], from: SocketAddr, at: Instant) {
        let Some(packet) = wire::decode(datagram) else {
            return;
        };
        match packet {
            wire::DiscoveryOrState::Discovery(message) => {
                self.hear_discovery(message, from, at);
            }
            wire::DiscoveryOrState::State {
                pong: false,
                message,
                payload,
            } => self.answer_ping(message, payload, from),
            wire::DiscoveryOrState::State {
                pong: true,
                message,
                payload: _,
            } => self.hear_pong(message, from, at),
        }
    }

    fn hear_discovery(&mut self, message: wire::Discovery, from: SocketAddr, at: Instant) {
        let state = message.state;
        if state.node_id == self.node_id || from.port() == 0 {
            return;
        }
        if message.bye_bye {
            self.peers.remove(&state.node_id);
            self.publish();
            return;
        }
        let changed = self
            .peers
            .insert(
                state.node_id,
                Peer {
                    addr: from,
                    session_id: state.session_id,
                    timeline: state.timeline,
                    start_stop: state.start_stop,
                    measure_v4: state.measure_v4,
                    last_seen: at,
                },
            )
            .is_none();
        let adopted = state.session_id == self.session.id
            && (state
                .timeline
                .is_some_and(|timeline| self.session.adopt_timeline(timeline))
                || state
                    .start_stop
                    .is_some_and(|start_stop| self.session.observe_start_stop(start_stop)));
        self.announce_now |= adopted;
        // An announcement gets a reply to its author alone, so a crowded
        // network still hears from a participant that arrived last.
        if message.alive && !message.response && (changed || adopted) && self.quiet_long_enough(at)
        {
            self.send_discovery(from, true, false);
        }
        self.publish();
    }

    /// Whether enough time has passed since the last thing this
    /// participant put on the network.
    fn quiet_long_enough(&self, at: Instant) -> bool {
        self.last_announce
            .is_none_or(|sent| at.saturating_duration_since(sent) >= RESPONSE_LIMIT)
    }

    /// Answer a ping with the session, this machine's ghost now, and the
    /// ping's own times echoed back.
    fn answer_ping(&mut self, _message: wire::StateMessage, payload: &[u8], from: SocketAddr) {
        let reply = wire::encode_state(
            &wire::StateMessage {
                pong: true,
                session_id: Some(self.session.id),
                ghost_time: Some(self.ghost_now()),
                host_time: None,
                prev_ghost_time: None,
            },
            payload,
        );
        if let Some(reply) = reply {
            let _ = self.measure_socket.send_to(&reply, from);
        }
    }

    /// A pong from the target of the measurement. The times it echoes
    /// back are the pairing the samples need: the host time and ghost
    /// time of the ping that asked, and the ghost time of the pong before
    /// it. Answering a pong with the next ping at once is what keeps the
    /// pairing tight; the interval is only for a ping that went missing.
    fn hear_pong(&mut self, message: wire::StateMessage, from: SocketAddr, at: Instant) {
        let Some((target_session, peer_id, endpoint)) = self
            .measure
            .as_ref()
            .map(|measure| (measure.session_id, measure.peer, measure.endpoint))
        else {
            return;
        };
        if from != endpoint {
            return;
        }
        if message.session_id != Some(target_session) {
            // The target moved to another session: measure that instead
            // of dropping its answers on the floor.
            if message.session_id.is_some() {
                self.measure = None;
            }
            return;
        }
        let host_now = host_micros(at);
        let decided = if let Some(measure) = self.measure.as_mut() {
            if let (Some(ghost_time), Some(host_time)) = (message.ghost_time, message.host_time)
                && ghost_time != 0
                && host_time != 0
            {
                measure
                    .samples
                    .push(ghost_time - (host_now + host_time) / 2);
                if let Some(prev_ghost) = message.prev_ghost_time.filter(|time| *time != 0) {
                    measure
                        .samples
                        .push((ghost_time + prev_ghost) / 2 - host_time);
                }
                measure.samples.truncate(MEASURE_SAMPLES * 2);
            }
            measure.retries = 0;
            measure.next_ping = at + RETRY_INTERVAL;
            let ping = wire::encode_state(
                &wire::StateMessage {
                    pong: false,
                    session_id: None,
                    ghost_time: None,
                    host_time: Some(host_now),
                    prev_ghost_time: message.ghost_time,
                },
                &[],
            );
            if let Some(ping) = ping {
                let _ = self.measure_socket.send_to(&ping, measure.endpoint);
            }
            measure.samples.len() >= MEASURE_SAMPLES
        } else {
            return;
        };
        if !decided {
            return;
        }
        let Some(offset) = self
            .measure
            .as_mut()
            .and_then(|measure| core::median(&mut measure.samples))
        else {
            return;
        };
        if target_session == self.session.id {
            // The session this machine stands in, measured against its
            // founder: the offset tracks the founder's clock.
            self.session.host_offset = offset;
            if let Some(measure) = self.measure.as_mut() {
                measure.samples.clear();
            }
        } else {
            let their_session = core::Session {
                id: target_session,
                timeline: self
                    .peers
                    .get(&peer_id)
                    .and_then(|peer| peer.timeline)
                    .unwrap_or(self.session.timeline),
                start_stop: self
                    .peers
                    .get(&peer_id)
                    .and_then(|peer| peer.start_stop)
                    .unwrap_or(self.session.start_stop),
                host_offset: offset,
            };
            let join = core::should_join(
                self.session.id,
                self.session.host_offset,
                their_session.id,
                their_session.host_offset,
            );
            if join {
                self.session.join(&their_session);
                self.announce_now = true;
            } else {
                self.measured_elsewhere.insert(target_session, at);
            }
            self.measure = None;
        }
        self.publish();
    }

    /// The periodic work: announcements, peer expiry, the measurement.
    fn maintain(&mut self, now: Instant) {
        self.peers
            .retain(|_, peer| now.duration_since(peer.last_seen) < PEER_LIFETIME);
        self.maintain_measurement(now);
        if self.announce_now || (now >= self.next_alive && self.quiet_long_enough(now)) {
            self.send_discovery(self.group, false, false);
            self.next_alive = now + ALIVE_INTERVAL;
            self.announce_now = false;
        }
        self.publish();
    }

    /// Keep one peer measured at a time: the founder of the session stood
    /// in, else the founder of a session not measured lately. A ping the
    /// target never answered is sent again, and a target that stays quiet
    /// is given up on until its session comes due again.
    fn maintain_measurement(&mut self, now: Instant) {
        self.maintain_target(now);
        let Some(measure) = self.measure.as_ref() else {
            return;
        };
        if now < measure.next_ping {
            return;
        }
        let (endpoint, session_id, retries) =
            (measure.endpoint, measure.session_id, measure.retries);
        if retries >= MEASURE_RETRIES {
            self.silent_until
                .insert(session_id, now + SILENT_RETRY_AFTER);
            self.measure = None;
            return;
        }
        let host_time = host_micros(now);
        if let Some(measure) = self.measure.as_mut() {
            measure.retries = retries + 1;
            measure.next_ping = now + RETRY_INTERVAL;
        }
        let ping = wire::encode_state(
            &wire::StateMessage {
                pong: false,
                session_id: None,
                ghost_time: None,
                host_time: Some(host_time),
                prev_ghost_time: None,
            },
            &[],
        );
        if let Some(ping) = ping {
            let _ = self.measure_socket.send_to(&ping, endpoint);
        }
    }

    fn maintain_target(&mut self, now: Instant) {
        if self.measure.is_some() {
            return;
        }
        let mut ours = None;
        let mut other = None;
        for (id, peer) in &self.peers {
            if *id == self.node_id || !peer.is_founder_of(*id) {
                continue;
            }
            if peer.session_id == self.session.id {
                ours = Some((*id, peer.session_id, peer.measure_endpoint()));
                break;
            }
            let declined_lately = self
                .measured_elsewhere
                .get(&peer.session_id)
                .is_some_and(|measured| now.saturating_duration_since(*measured) < REMEASURE_AFTER);
            let silent_lately = self
                .silent_until
                .get(&peer.session_id)
                .is_some_and(|until| now < *until);
            if !declined_lately && !silent_lately {
                other = other.or(Some((*id, peer.session_id, peer.measure_endpoint())));
            }
        }
        let Some((peer, session_id, endpoint)) = ours.or(other) else {
            return;
        };
        self.silent_until.remove(&session_id);
        self.measure = Some(Measure {
            peer,
            session_id,
            endpoint,
            samples: Vec::new(),
            retries: 0,
            next_ping: now,
        });
    }

    /// One announcement: to the group when leaving it unsaid, and to one
    /// address when answering it.
    fn send_discovery(&mut self, to: SocketAddr, response: bool, bye_bye: bool) {
        self.last_announce = Some(Instant::now());
        let state = wire::PeerState {
            node_id: self.node_id,
            session_id: self.session.id,
            timeline: Some(self.session.timeline),
            start_stop: Some(self.session.start_stop),
            measure_v4: if bye_bye { None } else { self.measure_v4 },
        };
        let message = wire::Discovery {
            alive: !response && !bye_bye,
            response,
            bye_bye,
            ttl: 1,
            state,
        };
        if let Some(bytes) = wire::encode_discovery(&message) {
            if to.ip().is_multicast() {
                // The announcement goes out once per interface, so the
                // participants on this machine and on the network both
                // hear it.
                for interface in &self.interfaces {
                    set_multicast_interface(&self.socket, *interface);
                    let _ = self.socket.send_to(&bytes, to);
                }
            } else {
                let _ = self.socket.send_to(&bytes, to);
            }
        }
    }

    fn say_bye_bye(&mut self) {
        self.send_discovery(self.group, false, true);
    }

    fn ghost_now(&self) -> i64 {
        self.session.ghost_at(host_micros(Instant::now()))
    }

    fn publish(&self) {
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        shared.peers = self.peers.len();
        shared.timeline = self.session.timeline;
        shared.playing = self.session.start_stop.playing;
        shared.host_offset = self.session.host_offset;
    }
}

/// Host time in microseconds: the wall clock, read through a monotonic
/// step, so it never runs backwards between reads.
fn host_micros(at: Instant) -> i64 {
    static ANCHOR: std::sync::OnceLock<(Instant, u128)> = std::sync::OnceLock::new();
    let (start, wall) = ANCHOR.get_or_init(|| {
        (
            Instant::now(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_micros())
                .unwrap_or(0),
        )
    });
    let micros = wall + at.saturating_duration_since(*start).as_micros();
    micros.min(i64::MAX as u128) as i64
}

/// An id no other participant on this network is likely to hold.
fn random_id() -> [u8; 8] {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos() as u64)
        .unwrap_or(0);
    let mixed = nanos ^ count.rotate_left(32) ^ u64::from(std::process::id()).rotate_left(17);
    mixed.to_be_bytes()
}

/// The address this machine sends multicast from, found without sending
/// anything: a connect only asks the routing table which interface would
/// carry it.
fn local_address() -> Option<[u8; 4]> {
    let probe = UdpSocket::bind("0.0.0.0:0").ok()?;
    probe.connect("8.8.8.8:80").ok()?;
    match probe.local_addr().ok()? {
        SocketAddr::V4(address) => Some(address.ip().octets()),
        SocketAddr::V6(_) => None,
    }
}

/// Every interface the session lives on. Loopback is always there: two
/// participants on one machine find each other on it, with or without a
/// network.
fn session_interfaces() -> Vec<Ipv4Addr> {
    let mut interfaces = vec![Ipv4Addr::LOCALHOST];
    if let Some(address) = local_address().map(Ipv4Addr::from)
        && !interfaces.contains(&address)
    {
        interfaces.push(address);
    }
    interfaces
}

/// Bind the shared port, allowing the other participants on this machine
/// to keep theirs: two Link apps on one desk are two participants.
#[cfg(unix)]
fn bind_socket() -> io::Result<UdpSocket> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    for option in [libc::SO_REUSEADDR, libc::SO_REUSEPORT] {
        let one: libc::c_int = 1;
        let set = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                &one as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if set < 0 {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(error);
        }
    }
    let address = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: wire::PORT.to_be(),
        sin_addr: libc::in_addr {
            s_addr: libc::INADDR_ANY,
        },
        sin_zero: [0; 8],
        sin_len: 0,
    };
    let bound = unsafe {
        libc::bind(
            fd,
            &address as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if bound < 0 {
        let error = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error);
    }
    Ok(unsafe { UdpSocket::from_raw_fd(fd) })
}

#[cfg(windows)]
fn bind_socket() -> io::Result<UdpSocket> {
    use std::os::windows::io::FromRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, IN_ADDR, IN_ADDR_0, IPPROTO_UDP, SO_REUSEADDR, SOCK_DGRAM, SOCKADDR, SOCKADDR_IN,
        SOCKET_ERROR, SOL_SOCKET, bind, closesocket, setsockopt, socket,
    };

    // Winsock starts on demand with the first std socket; a raw socket
    // made before any std one would find it never started.
    drop(UdpSocket::bind(("0.0.0.0", 0))?);
    let handle = unsafe { socket(AF_INET as i32, SOCK_DGRAM, IPPROTO_UDP) };
    if handle == usize::MAX {
        return Err(winsock_error("socket"));
    }
    let one: u32 = 1;
    let set = unsafe {
        setsockopt(
            handle,
            SOL_SOCKET,
            SO_REUSEADDR,
            &one as *const u32 as *const u8,
            std::mem::size_of::<u32>() as i32,
        )
    };
    if set == SOCKET_ERROR {
        let error = winsock_error("setsockopt");
        unsafe { closesocket(handle) };
        return Err(error);
    }
    let address = SOCKADDR_IN {
        sin_family: AF_INET,
        sin_port: wire::PORT.to_be(),
        sin_addr: IN_ADDR {
            S_un: IN_ADDR_0 { S_addr: 0 },
        },
        sin_zero: [0; 8],
    };
    let bound = unsafe {
        bind(
            handle,
            &address as *const SOCKADDR_IN as *const SOCKADDR,
            std::mem::size_of::<SOCKADDR_IN>() as i32,
        )
    };
    if bound == SOCKET_ERROR {
        let error = winsock_error("bind");
        unsafe { closesocket(handle) };
        return Err(error);
    }
    Ok(unsafe { UdpSocket::from_raw_socket(handle as u64) })
}

/// The last Winsock failure, as an error that names the call.
#[cfg(windows)]
fn winsock_error(call: &str) -> io::Error {
    use windows_sys::Win32::Networking::WinSock::WSAGetLastError;
    let code = unsafe { WSAGetLastError() };
    io::Error::other(format!("Winsock {call} failed: {code}"))
}

#[cfg(not(any(unix, windows)))]
fn bind_socket() -> io::Result<UdpSocket> {
    UdpSocket::bind(("0.0.0.0", wire::PORT))
}

/// Name the interface multicast leaves by. Left unsaid, the first send
/// can stall for seconds while the kernel looks for a route, which a
/// session that has just begun cannot wait out.
#[cfg(unix)]
fn set_multicast_interface(socket: &UdpSocket, interface: Ipv4Addr) {
    use std::os::fd::AsRawFd;
    // IP_MULTICAST_IF: a different number on the BSDs than on Linux.
    let option = if cfg!(any(target_os = "macos", target_os = "ios")) {
        9
    } else {
        32
    };
    let address = libc::in_addr {
        s_addr: u32::from_ne_bytes(interface.octets()),
    };
    let set = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::IPPROTO_IP,
            option,
            &address as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::in_addr>() as libc::socklen_t,
        )
    };
    let _ = set;
}

#[cfg(windows)]
fn set_multicast_interface(socket: &UdpSocket, interface: Ipv4Addr) {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        IN_ADDR, IN_ADDR_0, IP_MULTICAST_IF, IPPROTO_IP, setsockopt,
    };

    let address = IN_ADDR {
        S_un: IN_ADDR_0 {
            S_addr: u32::from_ne_bytes(interface.octets()),
        },
    };
    let set = unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            IPPROTO_IP,
            IP_MULTICAST_IF,
            &address as *const IN_ADDR as *const u8,
            std::mem::size_of::<IN_ADDR>() as i32,
        )
    };
    let _ = set;
}

#[cfg(not(any(unix, windows)))]
fn set_multicast_interface(socket: &UdpSocket, interface: Ipv4Addr) {
    let _ = (socket, interface);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_clock_never_runs_backwards() {
        let first = host_micros(Instant::now());
        let second = host_micros(Instant::now());
        assert!(second >= first);
    }

    #[test]
    fn the_ids_a_process_draws_are_all_different() {
        let first = random_id();
        let second = random_id();
        assert_ne!(first, second);
    }

    #[test]
    fn the_session_speaks_on_loopback_whatever_the_network() {
        assert!(session_interfaces().contains(&Ipv4Addr::LOCALHOST));
    }

    /// Two participants on this machine, both with the port they share:
    /// the bind every participant makes.
    #[test]
    fn two_participants_can_hold_the_same_port() {
        let Ok(first) = bind_socket() else {
            return;
        };
        let second = bind_socket();
        assert!(
            second.is_ok(),
            "the second bind failed: {}",
            second.unwrap_err()
        );
        drop(first);
    }

    /// Two participants on this machine, each with a grid of their own,
    /// end up standing in one session on one grid. Skipped where the
    /// announcements never come back through the looped group; the
    /// arithmetic is covered elsewhere, this is the wire.
    #[test]
    fn two_participants_meet_on_one_grid() {
        let Ok(first) = Link::start(120.0, 0.0) else {
            eprintln!("no Link socket here; skipping");
            return;
        };
        let Ok(second) = Link::start(90.0, 7.0) else {
            eprintln!("only one Link socket here; skipping");
            return;
        };
        let discovery_deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if first.status().peers >= 1 && second.status().peers >= 1 {
                break;
            }
            if Instant::now() > discovery_deadline {
                eprintln!("no discovery here; skipping");
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        // Both measure, one session yields, and from then on both run
        // the same tempo and the same phase, whatever each started with.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let (a, b) = (first.status(), second.status());
            let same_tempo = (a.bpm - b.bpm).abs() < 0.5;
            let same_phase = (a.cycle - b.cycle).abs() < 0.01;
            if same_tempo && same_phase {
                break;
            }
            assert!(Instant::now() < deadline, "no shared grid within 20 s");
            thread::sleep(Duration::from_millis(100));
        }
    }
}
