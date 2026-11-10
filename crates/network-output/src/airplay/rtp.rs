//! The packets of an AirPlay stream, sans-io: the audio packets sent to the
//! receiver, the sync packets that tie their timestamps to the clock, and the
//! timing and retransmit requests the receiver sends back.

use std::collections::VecDeque;
use std::time::Duration;

use super::alac::FRAMES_PER_PACKET;

/// How many frames the receiver buffers before it plays what it was sent;
/// two seconds at 44.1 kHz.
pub const LATENCY_FRAMES: u32 = 88_200;
pub const SAMPLE_RATE: u32 = 44_100;

const VERSION: u8 = 0x80;
const MARKER: u8 = 0x80;
const AUDIO: u8 = 0x60;
const SYNC: u8 = 0x54;
const TIMING_REQUEST: u8 = 0x52;
const TIMING_REPLY: u8 = 0x53;
const RETRANSMIT_REQUEST: u8 = 0x55;
const RETRANSMIT_REPLY: u8 = 0x56;
/// Set on the first sync packet after a start or a flush.
const EXTENSION: u8 = 0x10;
/// What the receiver's retransmit requests and replies number themselves.
const SEQUENCE_OF_CONTROL: u8 = 1;
const SEQUENCE_OF_SYNC_AND_TIMING: u8 = 7;
const TIMING_REQUEST_LENGTH: usize = 32;
const RETRANSMIT_REQUEST_LENGTH: usize = 8;
/// Seconds from the NTP epoch (1900) to the Unix one (1970).
const NTP_TO_UNIX: u64 = 2_208_988_800;
/// How many sent packets are kept for the receiver to ask for again.
const BACKLOG: usize = 512;

/// A time as NTP writes it: seconds since 1900 in the upper half, and the
/// fraction of a second in the lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ntp(pub u64);

impl Ntp {
    pub fn from_unix(since_epoch: Duration) -> Self {
        let seconds = since_epoch.as_secs() + NTP_TO_UNIX;
        let fraction = (u64::from(since_epoch.subsec_nanos()) << 32) / 1_000_000_000;
        Ntp((seconds << 32) | fraction)
    }

    fn bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }
}

/// Numbers the audio packets of one stream.
pub struct Stream {
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    /// The next packet is the first since a start or a flush.
    first: bool,
}

impl Stream {
    pub fn new(ssrc: u32, sequence: u16, timestamp: u32) -> Self {
        Stream {
            ssrc,
            sequence,
            timestamp,
            first: true,
        }
    }

    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    pub fn timestamp(&self) -> u32 {
        self.timestamp
    }

    /// Whether no packet was sent since the start or flush.
    pub fn is_first(&self) -> bool {
        self.first
    }

    /// Marks the next packet as the first of a new run, as after a flush.
    pub fn restart(&mut self) {
        self.first = true;
    }

    /// The packet carrying `payload`, which takes the next sequence number
    /// and timestamp.
    pub fn packet(&mut self, payload: &[u8]) -> Vec<u8> {
        let mut packet = Vec::with_capacity(12 + payload.len());
        packet.push(VERSION);
        packet.push(AUDIO | if self.first { MARKER } else { 0 });
        packet.extend_from_slice(&self.sequence.to_be_bytes());
        packet.extend_from_slice(&self.timestamp.to_be_bytes());
        packet.extend_from_slice(&self.ssrc.to_be_bytes());
        packet.extend_from_slice(payload);
        self.first = false;
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(FRAMES_PER_PACKET as u32);
        packet
    }
}

/// Tells the receiver which time `timestamp` is to be played at: the time
/// is that of the moment `now`, the receiver adds its latency.
pub fn sync_packet(first: bool, now: Ntp, timestamp: u32) -> [u8; 20] {
    let mut packet = [0; 20];
    packet[0] = VERSION | if first { EXTENSION } else { 0 };
    packet[1] = SYNC | MARKER;
    packet[3] = SEQUENCE_OF_SYNC_AND_TIMING;
    packet[4..8].copy_from_slice(&timestamp.wrapping_sub(LATENCY_FRAMES).to_be_bytes());
    packet[8..16].copy_from_slice(&now.bytes());
    packet[16..20].copy_from_slice(&timestamp.to_be_bytes());
    packet
}

/// What a receiver asks over the control or timing port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// For the sender's clock: answered by [`timing_reply`].
    Timing { sent: Ntp },
    /// For `count` packets from sequence number `first` again.
    Retransmit { first: u16, count: u16 },
}

/// None for anything else, as a datagram of any length may arrive from
/// anyone on the network.
pub fn parse_request(datagram: &[u8]) -> Option<Request> {
    let kind = datagram.get(1)? & !MARKER;
    match kind {
        TIMING_REQUEST if datagram.len() >= TIMING_REQUEST_LENGTH => {
            let sent = u64::from_be_bytes(datagram[24..32].try_into().ok()?);
            Some(Request::Timing { sent: Ntp(sent) })
        }
        RETRANSMIT_REQUEST if datagram.len() >= RETRANSMIT_REQUEST_LENGTH => {
            Some(Request::Retransmit {
                first: u16::from_be_bytes([datagram[4], datagram[5]]),
                count: u16::from_be_bytes([datagram[6], datagram[7]]),
            })
        }
        _ => None,
    }
}

/// The answer to a timing request that was sent at `sent` by the receiver's
/// clock, and arrived at `received` and is answered at `replied` by ours.
pub fn timing_reply(sent: Ntp, received: Ntp, replied: Ntp) -> [u8; TIMING_REQUEST_LENGTH] {
    let mut packet = [0; TIMING_REQUEST_LENGTH];
    packet[0] = VERSION;
    packet[1] = TIMING_REPLY | MARKER;
    packet[3] = SEQUENCE_OF_SYNC_AND_TIMING;
    packet[8..16].copy_from_slice(&sent.bytes());
    packet[16..24].copy_from_slice(&received.bytes());
    packet[24..32].copy_from_slice(&replied.bytes());
    packet
}

/// The packets sent last, for the receiver to ask for again.
#[derive(Default)]
pub struct Backlog {
    sent: VecDeque<(u16, Vec<u8>)>,
}

impl Backlog {
    pub fn keep(&mut self, sequence: u16, packet: Vec<u8>) {
        if self.sent.len() == BACKLOG {
            self.sent.pop_front();
        }
        self.sent.push_back((sequence, packet));
    }

    /// Forgets what was sent, which a flush made the receiver drop.
    pub fn clear(&mut self) {
        self.sent.clear();
    }

    /// The replies to a retransmit request for the packets it still has.
    pub fn replies(&self, first: u16, count: u16) -> Vec<Vec<u8>> {
        (0..count)
            .filter_map(|offset| {
                let wanted = first.wrapping_add(offset);
                self.sent.iter().find(|(sequence, _)| *sequence == wanted)
            })
            .map(|(_, packet)| {
                let mut reply = vec![VERSION, RETRANSMIT_REPLY | MARKER, 0, SEQUENCE_OF_CONTROL];
                reply.extend_from_slice(packet);
                reply
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_audio_packet_has_an_rtp_header_with_the_marker_on_the_first_only() {
        let mut stream = Stream::new(0xAABB_CCDD, 7, 1000);
        let first = stream.packet(&[1, 2, 3]);
        assert_eq!(
            first,
            [
                0x80, 0xE0, 0, 7, 0, 0, 0x03, 0xE8, 0xAA, 0xBB, 0xCC, 0xDD, 1, 2, 3
            ]
        );
        let second = stream.packet(&[4]);
        assert_eq!(&second[..4], [0x80, 0x60, 0, 8]);
        assert_eq!(u32::from_be_bytes(second[4..8].try_into().unwrap()), 1352);
    }

    #[test]
    fn sequence_numbers_and_timestamps_wrap_and_a_restart_marks_the_next_packet_again() {
        let mut stream = Stream::new(1, u16::MAX, u32::MAX - 100);
        stream.packet(&[]);
        assert_eq!(stream.sequence(), 0);
        assert_eq!(stream.timestamp(), (u32::MAX - 100).wrapping_add(352));
        assert!(!stream.is_first());
        stream.restart();
        assert_eq!(stream.packet(&[])[1], 0xE0);
    }

    #[test]
    fn unix_time_becomes_ntp_seconds_since_1900_and_a_binary_fraction() {
        let ntp = Ntp::from_unix(Duration::new(1, 500_000_000));
        assert_eq!(ntp.0 >> 32, 2_208_988_801);
        assert_eq!(ntp.0 & 0xFFFF_FFFF, 1 << 31);
    }

    #[test]
    fn a_sync_packet_gives_the_clock_and_the_timestamp_played_at_it_less_the_latency() {
        let now = Ntp(0x0102_0304_0506_0708);
        let packet = sync_packet(true, now, 100_000);
        assert_eq!(&packet[..4], [0x90, 0xD4, 0, 7]);
        assert_eq!(
            u32::from_be_bytes(packet[4..8].try_into().unwrap()),
            100_000 - LATENCY_FRAMES
        );
        assert_eq!(&packet[8..16], now.bytes());
        assert_eq!(
            u32::from_be_bytes(packet[16..20].try_into().unwrap()),
            100_000
        );
        assert_eq!(sync_packet(false, now, 5)[0], 0x80);
    }

    #[test]
    fn a_timing_request_is_answered_with_its_send_time_and_ours() {
        let mut request = [0; 32];
        request[1] = 0xD2;
        request[24..32].copy_from_slice(&Ntp(42).bytes());
        let Some(Request::Timing { sent }) = parse_request(&request) else {
            panic!("a timing request");
        };
        let reply = timing_reply(sent, Ntp(50), Ntp(51));
        assert_eq!(&reply[..4], [0x80, 0xD3, 0, 7]);
        assert_eq!(u64::from_be_bytes(reply[8..16].try_into().unwrap()), 42);
        assert_eq!(u64::from_be_bytes(reply[16..24].try_into().unwrap()), 50);
        assert_eq!(u64::from_be_bytes(reply[24..32].try_into().unwrap()), 51);
    }

    #[test]
    fn a_retransmit_request_is_read_and_anything_short_or_unknown_is_not() {
        assert_eq!(
            parse_request(&[0x80, 0xD5, 0, 1, 0x12, 0x34, 0, 3]),
            Some(Request::Retransmit {
                first: 0x1234,
                count: 3
            })
        );
        assert_eq!(parse_request(&[0x80, 0xD5, 0, 1, 0, 1]), None);
        assert_eq!(parse_request(&[0x80, 0xD2, 0, 7]), None);
        assert_eq!(parse_request(&[0x80, 0x60, 0, 7, 0, 0, 0, 0]), None);
        assert_eq!(parse_request(&[]), None);
    }

    #[test]
    fn a_retransmit_reply_wraps_the_packet_sent_and_only_those_still_kept() {
        let mut backlog = Backlog::default();
        for sequence in [65_534, 65_535, 0] {
            backlog.keep(sequence, vec![sequence as u8]);
        }
        let replies = backlog.replies(65_535, 3);
        assert_eq!(
            replies,
            [vec![0x80, 0xD6, 0, 1, 255], vec![0x80, 0xD6, 0, 1, 0]]
        );
        backlog.clear();
        assert!(backlog.replies(0, 1).is_empty());
    }

    #[test]
    fn the_backlog_forgets_the_oldest_packet_past_its_size() {
        let mut backlog = Backlog::default();
        for sequence in 0..=BACKLOG as u16 {
            backlog.keep(sequence, Vec::new());
        }
        assert!(backlog.replies(0, 1).is_empty());
        assert_eq!(backlog.replies(1, 1).len(), 1);
    }
}
