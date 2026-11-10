//! The Link wire formats: discovery messages on the multicast group and
//! the ping and pong a session is measured with. Every integer is
//! big-endian and every value rides in a length-framed entry.

/// The multicast group every participant listens on.
pub const GROUP: [u8; 4] = [224, 76, 78, 75];
/// The port discovery, ping and pong all share.
pub const PORT: u16 = 20808;
/// The largest message any participant accepts.
pub const MAX_MESSAGE: usize = 512;

/// `_asdp_v` and one protocol version byte.
const DISCOVERY_HEADER: [u8; 8] = [b'_', b'a', b's', b'd', b'p', b'_', b'v', 1];
/// `_link_v` and one protocol version byte.
const STATE_HEADER: [u8; 8] = [b'_', b'l', b'i', b'n', b'k', b'_', b'v', 1];

/// Entry keys, as they stand at the front of an entry.
pub const TIMELINE_KEY: u32 = u32::from_be_bytes(*b"tmln");
pub const SESSION_KEY: u32 = u32::from_be_bytes(*b"sess");
pub const START_STOP_KEY: u32 = u32::from_be_bytes(*b"stst");
pub const MEASURE_V4_KEY: u32 = u32::from_be_bytes(*b"mep4");
pub const HOST_TIME_KEY: u32 = u32::from_be_bytes(*b"__ht");
pub const GHOST_TIME_KEY: u32 = u32::from_be_bytes(*b"__gt");
pub const PREV_GHOST_TIME_KEY: u32 = u32::from_be_bytes(*b"_pgt");

/// One beat in micro-beats, one minute in microseconds.
pub const MICRO_BEATS_PER_BEAT: i64 = 1_000_000;
const MICROS_PER_MINUTE: i64 = 60_000_000;

/// The tempo and the beat grid it runs on. Times are ghost microseconds,
/// beats are micro-beats, and the pair is a bijection between them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Timeline {
    /// Microseconds a beat lasts. The tempo, as the wire carries it.
    pub micros_per_beat: i64,
    /// The beat the grid counts from.
    pub beat_origin: i64,
    /// The ghost time that beat was at.
    pub time_origin: i64,
}

impl Timeline {
    /// The beat the grid is at, in micro-beats. Every step saturates, so
    /// a grid a peer carried past the range of `i64` reads as a bound,
    /// not a panic.
    pub fn beat_at(&self, ghost: i64) -> i64 {
        let micros = ghost.saturating_sub(self.time_origin);
        self.beat_origin.saturating_add(
            micros.saturating_mul(MICRO_BEATS_PER_BEAT) / self.micros_per_beat.max(1),
        )
    }

    /// The ghost time a beat is at. Rounding keeps the inverse of
    /// `beat_at` exact on whole beats, and every step saturates as in
    /// `beat_at`.
    pub fn time_at(&self, beat: i64) -> i64 {
        let micros = beat
            .saturating_sub(self.beat_origin)
            .saturating_mul(self.micros_per_beat.max(1))
            / MICRO_BEATS_PER_BEAT;
        self.time_origin.saturating_add(micros)
    }
}

/// Microseconds a beat lasts at a tempo in beats per minute, held to the
/// range the protocol keeps.
pub fn micros_per_beat(bpm: f64) -> i64 {
    (MICROS_PER_MINUTE as f64 / bpm.clamp(MIN_BPM, MAX_BPM)).round() as i64
}

/// The tempo in beats per minute a grid runs at.
pub fn bpm(micros_per_beat: i64) -> f64 {
    MICROS_PER_MINUTE as f64 / micros_per_beat.max(1) as f64
}

/// The tempo range a participant holds.
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 999.0;

/// Whether the transport runs, and when that was decided.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StartStop {
    pub playing: bool,
    /// Where the grid was when the decision landed.
    pub beat: i64,
    /// Ghost time of the decision; the newer one wins.
    pub timestamp: i64,
}

/// What one participant said about itself, gathered from a message's
/// entries. Every part is optional: a message is a set of entries, and
/// only the ones a sender chose are there.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PeerState {
    pub node_id: [u8; 8],
    pub session_id: [u8; 8],
    pub timeline: Option<Timeline>,
    pub start_stop: Option<StartStop>,
    /// Where that participant answers a ping, as IPv4 address and port.
    pub measure_v4: Option<([u8; 4], u16)>,
}

/// A discovery message: what a participant said, and how it said it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Discovery {
    pub alive: bool,
    /// A reply to someone else's announcement, sent to them alone.
    pub response: bool,
    /// Leaving for good; the state fields are empty.
    pub bye_bye: bool,
    pub ttl: u8,
    pub state: PeerState,
}

/// A ping or a pong, with the entries either carries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StateMessage {
    pub pong: bool,
    /// `sess`: the session the replier stands in.
    pub session_id: Option<[u8; 8]>,
    /// `__gt`: the replier's ghost time as it answered.
    pub ghost_time: Option<i64>,
    /// `__ht`: echoed from the ping, the sender's host time.
    pub host_time: Option<i64>,
    /// `_pgt`: echoed from the ping, the ghost time last heard back.
    pub prev_ghost_time: Option<i64>,
}

/// Append one length-framed entry.
fn put_entry(out: &mut Vec<u8>, key: u32, body: &[u8]) {
    out.extend_from_slice(&key.to_be_bytes());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
}

fn put_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Walk the entries of a payload, handing each key and body to `visit`.
/// A frame that runs past the buffer ends the walk: trailing bytes are
/// another protocol's business, not ours.
fn entries<'a>(payload: &'a [u8], mut visit: impl FnMut(u32, &'a [u8])) {
    let mut rest = payload;
    while rest.len() >= 8 {
        let key = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
        let size = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let Some(body) = rest.get(8..8 + size) else {
            break;
        };
        visit(key, body);
        rest = &rest[8 + size..];
    }
}

fn read_i64(body: &[u8]) -> Option<i64> {
    body.try_into().ok().map(i64::from_be_bytes)
}

fn read_id(body: &[u8]) -> Option<[u8; 8]> {
    body.try_into().ok()
}

fn put_state_entries(out: &mut Vec<u8>, state: &PeerState, with_endpoints: bool) {
    if let Some(timeline) = state.timeline {
        let mut body = Vec::with_capacity(24);
        put_i64(&mut body, timeline.micros_per_beat);
        put_i64(&mut body, timeline.beat_origin);
        put_i64(&mut body, timeline.time_origin);
        put_entry(out, TIMELINE_KEY, &body);
    }
    put_entry(out, SESSION_KEY, &state.session_id);
    if let Some(start_stop) = state.start_stop {
        let mut body = Vec::with_capacity(17);
        body.push(u8::from(start_stop.playing));
        put_i64(&mut body, start_stop.beat);
        put_i64(&mut body, start_stop.timestamp);
        put_entry(out, START_STOP_KEY, &body);
    }
    if with_endpoints && let Some((address, port)) = state.measure_v4 {
        let mut body = Vec::with_capacity(6);
        body.extend_from_slice(&address);
        body.extend_from_slice(&port.to_be_bytes());
        put_entry(out, MEASURE_V4_KEY, &body);
    }
}

/// Encode a discovery message. Returns the byte count, which fits or the
/// message is not sent: a payload past the protocol's maximum is a bug in
/// the sender, not something to truncate on the wire.
pub fn encode_discovery(message: &Discovery) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(96);
    out.extend_from_slice(&DISCOVERY_HEADER);
    let kind = if message.bye_bye {
        3
    } else if message.response {
        2
    } else {
        1
    };
    out.push(kind);
    out.push(message.ttl);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&message.state.node_id);
    if !message.bye_bye {
        put_state_entries(&mut out, &message.state, true);
    }
    (out.len() <= MAX_MESSAGE).then_some(out)
}

/// Encode a ping or pong. A pong repeats the ping's own payload after its
/// entries, which is how the measurer gets its times echoed back.
pub fn encode_state(message: &StateMessage, echoed_ping_payload: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(48 + echoed_ping_payload.len());
    out.extend_from_slice(&STATE_HEADER);
    out.push(u8::from(message.pong) + 1);
    if let Some(session_id) = message.session_id {
        put_entry(&mut out, SESSION_KEY, &session_id);
    }
    if let Some(ghost_time) = message.ghost_time {
        let mut body = Vec::with_capacity(8);
        put_i64(&mut body, ghost_time);
        put_entry(&mut out, GHOST_TIME_KEY, &body);
    }
    if let Some(host_time) = message.host_time {
        let mut body = Vec::with_capacity(8);
        put_i64(&mut body, host_time);
        put_entry(&mut out, HOST_TIME_KEY, &body);
    }
    if let Some(prev_ghost_time) = message.prev_ghost_time {
        let mut body = Vec::with_capacity(8);
        put_i64(&mut body, prev_ghost_time);
        put_entry(&mut out, PREV_GHOST_TIME_KEY, &body);
    }
    out.extend_from_slice(echoed_ping_payload);
    (out.len() <= MAX_MESSAGE).then_some(out)
}

/// Decode a datagram by its leading header. Discovery messages carry a
/// full peer state; state messages carry only what a ping or pong needs.
pub fn decode<'a>(datagram: &'a [u8]) -> Option<DiscoveryOrState<'a>> {
    let (header, rest) = datagram.split_first_chunk::<8>()?;
    if *header == DISCOVERY_HEADER {
        let (kind, rest) = rest.split_first_chunk::<1>()?;
        let (ttl, rest) = rest.split_first_chunk::<1>()?;
        let (group, rest) = rest.split_first_chunk::<2>()?;
        let (node_id, payload) = rest.split_first_chunk::<8>()?;
        if *group != [0, 0] {
            return None;
        }
        let mut state = PeerState {
            node_id: *node_id,
            ..PeerState::default()
        };
        entries(payload, |key, body| match key {
            TIMELINE_KEY if body.len() == 24 => {
                state.timeline = Some(Timeline {
                    micros_per_beat: read_i64(&body[0..8]).unwrap_or_default(),
                    beat_origin: read_i64(&body[8..16]).unwrap_or_default(),
                    time_origin: read_i64(&body[16..24]).unwrap_or_default(),
                });
            }
            SESSION_KEY if body.len() == 8 => {
                state.session_id = read_id(body).unwrap_or_default();
            }
            START_STOP_KEY if body.len() == 17 => {
                state.start_stop = Some(StartStop {
                    playing: body[0] != 0,
                    beat: read_i64(&body[1..9]).unwrap_or_default(),
                    timestamp: read_i64(&body[9..17]).unwrap_or_default(),
                });
            }
            MEASURE_V4_KEY if body.len() == 6 => {
                let port = u16::from_be_bytes([body[4], body[5]]);
                state.measure_v4 = Some(([body[0], body[1], body[2], body[3]], port));
            }
            _ => {}
        });
        return Some(DiscoveryOrState::Discovery(Discovery {
            alive: *kind == [1],
            response: *kind == [2],
            bye_bye: *kind == [3],
            ttl: ttl[0],
            state,
        }));
    }
    if *header == STATE_HEADER {
        let (kind, payload) = rest.split_first_chunk::<1>()?;
        let mut message = StateMessage::default();
        entries(payload, |key, body| match key {
            SESSION_KEY if body.len() == 8 => {
                message.session_id = read_id(body);
            }
            GHOST_TIME_KEY if body.len() == 8 => {
                message.ghost_time = read_i64(body);
            }
            HOST_TIME_KEY if body.len() == 8 => {
                message.host_time = read_i64(body);
            }
            PREV_GHOST_TIME_KEY if body.len() == 8 => {
                message.prev_ghost_time = read_i64(body);
            }
            _ => {}
        });
        return Some(DiscoveryOrState::State {
            pong: *kind == [2],
            message,
            payload,
        });
    }
    None
}

/// A decoded datagram, already sorted by protocol.
#[derive(Clone, Copy, Debug)]
pub enum DiscoveryOrState<'a> {
    Discovery(Discovery),
    State {
        pong: bool,
        message: StateMessage,
        /// The payload after the header, echoed into a pong verbatim.
        payload: &'a [u8],
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline() -> Timeline {
        Timeline {
            micros_per_beat: 500_000,
            beat_origin: 12_345_678,
            time_origin: 987_654_321,
        }
    }

    fn state() -> PeerState {
        PeerState {
            node_id: [1, 2, 3, 4, 5, 6, 7, 8],
            session_id: [8, 7, 6, 5, 4, 3, 2, 1],
            timeline: Some(timeline()),
            start_stop: Some(StartStop {
                playing: true,
                beat: 4_000_000,
                timestamp: 1_000_000_000,
            }),
            measure_v4: Some(([192, 168, 1, 42], PORT)),
        }
    }

    #[test]
    fn a_discovery_message_survives_the_wire() {
        let bytes = encode_discovery(&Discovery {
            alive: true,
            response: false,
            bye_bye: false,
            ttl: 1,
            state: state(),
        })
        .unwrap();
        assert!(bytes.len() <= MAX_MESSAGE);
        assert_eq!(&bytes[..8], &DISCOVERY_HEADER);
        let DiscoveryOrState::Discovery(back) = decode(&bytes).unwrap() else {
            panic!("not a discovery message");
        };
        assert!(back.alive && !back.response && !back.bye_bye);
        assert_eq!(back.ttl, 1);
        assert_eq!(back.state, state());
    }

    #[test]
    fn a_bye_bye_carries_no_state() {
        let bytes = encode_discovery(&Discovery {
            alive: false,
            response: false,
            bye_bye: true,
            ttl: 0,
            state: state(),
        })
        .unwrap();
        let DiscoveryOrState::Discovery(back) = decode(&bytes).unwrap() else {
            panic!("not a discovery message");
        };
        assert!(back.bye_bye);
        assert_eq!(back.state.timeline, None);
        assert_eq!(back.state.session_id, [0; 8]);
    }

    #[test]
    fn a_response_decodes_as_one() {
        let bytes = encode_discovery(&Discovery {
            alive: false,
            response: true,
            bye_bye: false,
            ttl: 1,
            state: state(),
        })
        .unwrap();
        let DiscoveryOrState::Discovery(back) = decode(&bytes).unwrap() else {
            panic!("not a discovery message");
        };
        assert!(back.response && !back.alive);
    }

    #[test]
    fn a_pong_carries_the_ping_payload_back_verbatim() {
        let mut ping_payload = Vec::new();
        let mut body = Vec::new();
        put_i64(&mut body, 111_111_111);
        put_entry(&mut ping_payload, HOST_TIME_KEY, &body);
        let mut body = Vec::new();
        put_i64(&mut body, 222_222_222);
        put_entry(&mut ping_payload, PREV_GHOST_TIME_KEY, &body);

        let bytes = encode_state(
            &StateMessage {
                pong: true,
                session_id: Some([9; 8]),
                ghost_time: Some(333_333_333),
                host_time: None,
                prev_ghost_time: None,
            },
            &ping_payload,
        )
        .unwrap();
        assert!(bytes.ends_with(&ping_payload));
        let DiscoveryOrState::State {
            pong,
            message,
            payload,
        } = decode(&bytes).unwrap()
        else {
            panic!("not a state message");
        };
        assert!(pong);
        assert_eq!(message.session_id, Some([9; 8]));
        assert_eq!(message.ghost_time, Some(333_333_333));
        assert_eq!(message.host_time, Some(111_111_111));
        assert_eq!(message.prev_ghost_time, Some(222_222_222));
        // The payload carries the pong's own entries and then the ping's,
        // which is what a pong echoes back verbatim.
        assert!(payload.ends_with(&ping_payload));
    }

    #[test]
    fn a_foreign_datagram_is_left_alone() {
        assert!(decode(&[]).is_none());
        assert!(decode(b"_asdp_v1").is_none());
        assert!(decode(b"not a link datagram at all").is_none());
        // The right magic with a version byte that is not ours.
        assert!(decode(b"_link_v\x02\x01").is_none());
    }

    #[test]
    fn a_truncated_frame_ends_the_walk_rather_than_panicking() {
        let mut bytes = encode_discovery(&Discovery {
            alive: true,
            response: false,
            bye_bye: false,
            ttl: 1,
            state: state(),
        })
        .unwrap();
        // Keep the header and one entry header, cut the body short.
        bytes.truncate(30);
        let DiscoveryOrState::Discovery(back) = decode(&bytes).unwrap() else {
            panic!("not a discovery message");
        };
        assert_eq!(back.state.timeline, None);
        assert_eq!(back.state.session_id, [0; 8]);
    }

    #[test]
    fn the_grid_round_trips_time_and_beats() {
        let timeline = timeline();
        let beat = timeline.beat_at(1_000_000_000);
        assert_eq!(timeline.time_at(beat), 1_000_000_000);
        // Half a beat out and back, to within a microsecond.
        let near = timeline.time_at(beat) + timeline.micros_per_beat / 2;
        let beat_near = timeline.beat_at(near);
        assert!((beat_near - beat).abs() <= MICRO_BEATS_PER_BEAT);
    }

    #[test]
    fn a_peer_grid_past_the_range_of_i64_reads_as_a_bound() {
        // Origins a malformed peer can carry: both conversions would
        // overflow a plain subtraction or multiplication.
        let far_past = Timeline {
            micros_per_beat: 1,
            beat_origin: 0,
            time_origin: i64::MIN,
        };
        assert_eq!(far_past.beat_at(0), i64::MAX);
        let far_future = Timeline {
            micros_per_beat: 1_000_000,
            beat_origin: i64::MIN,
            time_origin: i64::MAX,
        };
        assert_eq!(far_future.time_at(0), i64::MAX);
    }
}
