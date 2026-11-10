//! The RTSP conversation with an AirPlay receiver, sans-io: the requests as
//! bytes to send, and what a reply says as read from the bytes received.

use std::fmt;
use std::net::Ipv4Addr;

use super::alac::FORMAT_PARAMETERS;

const USER_AGENT: &str = "Fonoteca";
/// The loudest and quietest volumes a receiver takes, in dB; below the
/// quietest is silence and a receiver is muted by [`MUTED`].
pub const LOUDEST: f32 = 0.0;
pub const QUIETEST: f32 = -30.0;
pub const MUTED: f32 = -144.0;
/// The longest reply head or body taken in, as it comes from the network.
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;

/// Builds the requests of one conversation: numbers them, and says which
/// session they belong to once the receiver gave one.
pub struct Requests {
    uri: String,
    local: Ipv4Addr,
    remote: Ipv4Addr,
    sdp_id: u32,
    cseq: u32,
    session: Option<String>,
}

impl Requests {
    /// A conversation of this device (`local`) with the receiver, under
    /// `session_id`, which names it in the address of the stream.
    pub fn new(local: Ipv4Addr, remote: Ipv4Addr, session_id: u32) -> Self {
        Requests {
            uri: format!("rtsp://{local}/{session_id}"),
            local,
            remote,
            sdp_id: session_id,
            cseq: 0,
            session: None,
        }
    }

    /// Keeps the session the reply to SETUP gave, for the requests after.
    pub fn join(&mut self, reply: &Reply) {
        if let Some(session) = reply.header("session") {
            self.session = Some(session.to_owned());
        }
    }

    pub fn options(&mut self) -> Vec<u8> {
        self.request("OPTIONS", "*", &[], None)
    }

    pub fn announce(&mut self) -> Vec<u8> {
        let sdp = self.sdp();
        let uri = self.uri.clone();
        self.request(
            "ANNOUNCE",
            &uri,
            &[],
            Some(("application/sdp", sdp.as_bytes())),
        )
    }

    pub fn setup(&mut self, control_port: u16, timing_port: u16) -> Vec<u8> {
        let transport = format!(
            "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={control_port};timing_port={timing_port}"
        );
        let uri = self.uri.clone();
        self.request("SETUP", &uri, &[("Transport", transport)], None)
    }

    pub fn record(&mut self, sequence: u16, timestamp: u32) -> Vec<u8> {
        let headers = [
            ("Range", "npt=0-".to_owned()),
            ("RTP-Info", format!("seq={sequence};rtptime={timestamp}")),
        ];
        let uri = self.uri.clone();
        self.request("RECORD", &uri, &headers, None)
    }

    /// Sets the volume in dB: [`QUIETEST`] to [`LOUDEST`], or [`MUTED`].
    /// Anything else is clamped into that range.
    pub fn volume(&mut self, decibels: f32) -> Vec<u8> {
        let decibels = if decibels == MUTED {
            MUTED
        } else {
            decibels.clamp(QUIETEST, LOUDEST)
        };
        let body = format!("volume: {decibels:.6}\r\n");
        let uri = self.uri.clone();
        self.request(
            "SET_PARAMETER",
            &uri,
            &[],
            Some(("text/parameters", body.as_bytes())),
        )
    }

    /// Has the receiver drop what it holds; what follows starts at the
    /// given packet.
    pub fn flush(&mut self, sequence: u16, timestamp: u32) -> Vec<u8> {
        let headers = [("RTP-Info", format!("seq={sequence};rtptime={timestamp}"))];
        let uri = self.uri.clone();
        self.request("FLUSH", &uri, &headers, None)
    }

    pub fn teardown(&mut self) -> Vec<u8> {
        let uri = self.uri.clone();
        self.request("TEARDOWN", &uri, &[], None)
    }

    fn sdp(&self) -> String {
        format!(
            "v=0\r\no=iTunes {id} 0 IN IP4 {local}\r\ns=iTunes\r\nc=IN IP4 {remote}\r\nt=0 0\r\n\
             m=audio 0 RTP/AVP 96\r\na=rtpmap:96 AppleLossless\r\na=fmtp:96 {FORMAT_PARAMETERS}\r\n",
            id = self.sdp_id,
            local = self.local,
            remote = self.remote,
        )
    }

    fn request(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, String)],
        body: Option<(&str, &[u8])>,
    ) -> Vec<u8> {
        self.cseq += 1;
        let mut head = format!(
            "{method} {uri} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: {USER_AGENT}\r\n",
            self.cseq
        );
        if let Some(session) = &self.session {
            head.push_str(&format!("Session: {session}\r\n"));
        }
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        let body = body.map(|(content_type, body)| {
            head.push_str(&format!(
                "Content-Type: {content_type}\r\nContent-Length: {}\r\n",
                body.len()
            ));
            body
        });
        head.push_str("\r\n");
        let mut request = head.into_bytes();
        request.extend_from_slice(body.unwrap_or_default());
        request
    }
}

/// What a receiver answered.
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    /// The headers, named in lower case.
    headers: Vec<(String, String)>,
}

/// A reply that is not RTSP, or larger than a receiver would send.
#[derive(Debug, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "The device's reply is malformed: {}.", self.0)
    }
}

impl std::error::Error for Malformed {}

impl Reply {
    /// The reply at the start of `received`, and how many of its bytes it
    /// took; None while a whole reply has not arrived yet.
    pub fn parse(received: &[u8]) -> Result<Option<(Reply, usize)>, Malformed> {
        let Some(head_end) = find(received, b"\r\n\r\n") else {
            return if received.len() > MAX_HEAD {
                Err(Malformed("the head is too long"))
            } else {
                Ok(None)
            };
        };
        let head = std::str::from_utf8(&received[..head_end])
            .map_err(|_| Malformed("the head is not text"))?;
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(status_of)
            .ok_or(Malformed("no RTSP status line"))?;
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let reply = Reply { status, headers };
        let body_length = match reply.header("content-length") {
            Some(length) => length
                .parse::<usize>()
                .ok()
                .filter(|&length| length <= MAX_BODY)
                .ok_or(Malformed("the body's length is not one to take"))?,
            None => 0,
        };
        let total = head_end + 4 + body_length;
        Ok((received.len() >= total).then_some((reply, total)))
    }

    pub fn is_ok(&self) -> bool {
        self.status == 200
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    /// The ports the receiver listens on, from SETUP's reply.
    pub fn ports(&self) -> Result<Ports, Malformed> {
        let transport = self
            .header("transport")
            .ok_or(Malformed("no transport in the reply to SETUP"))?;
        let port = |name: &str| {
            transport
                .split(';')
                .filter_map(|part| part.split_once('='))
                .find(|(key, _)| key.trim() == name)
                .and_then(|(_, value)| value.trim().parse::<u16>().ok())
                .filter(|&port| port != 0)
        };
        Ok(Ports {
            audio: port("server_port").ok_or(Malformed("no audio port in the transport"))?,
            control: port("control_port").ok_or(Malformed("no control port in the transport"))?,
        })
    }
}

/// Where the receiver takes audio and sync packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    pub audio: u16,
    pub control: u16,
}

fn status_of(line: &str) -> Option<u16> {
    let mut parts = line.split_whitespace();
    parts.next()?.starts_with("RTSP/").then_some(())?;
    parts.next()?.parse().ok()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requests() -> Requests {
        Requests::new(
            Ipv4Addr::new(192, 168, 1, 5),
            Ipv4Addr::new(192, 168, 1, 20),
            1234,
        )
    }

    fn text(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn options_asks_about_the_whole_server_and_every_request_is_numbered() {
        let mut requests = requests();
        assert_eq!(
            text(requests.options()),
            "OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nUser-Agent: Fonoteca\r\n\r\n"
        );
        assert!(text(requests.teardown()).contains("CSeq: 2\r\n"));
    }

    #[test]
    fn announce_describes_unencrypted_apple_lossless_in_an_sdp_body() {
        let request = text(requests().announce());
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("ANNOUNCE rtsp://192.168.1.5/1234 RTSP/1.0\r\n"));
        assert!(head.contains("Content-Type: application/sdp\r\n"));
        assert!(head.ends_with(&format!("Content-Length: {}", body.len())));
        assert_eq!(
            body,
            "v=0\r\no=iTunes 1234 0 IN IP4 192.168.1.5\r\ns=iTunes\r\nc=IN IP4 192.168.1.20\r\nt=0 0\r\n\
             m=audio 0 RTP/AVP 96\r\na=rtpmap:96 AppleLossless\r\n\
             a=fmtp:96 352 0 16 40 10 14 2 255 0 0 44100\r\n"
        );
        assert!(!body.contains("rsaaeskey"));
    }

    #[test]
    fn setup_names_the_ports_we_listen_on_and_the_session_is_kept_for_what_follows() {
        let mut requests = requests();
        let setup = text(requests.setup(6001, 6002));
        assert!(setup.contains(
            "Transport: RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port=6001;timing_port=6002\r\n"
        ));
        let (reply, _) = Reply::parse(b"RTSP/1.0 200 OK\r\nCSeq: 3\r\nSession: DEADBEEF\r\n\r\n")
            .unwrap()
            .unwrap();
        requests.join(&reply);
        assert!(text(requests.record(10, 20)).contains("Session: DEADBEEF\r\n"));
    }

    #[test]
    fn record_and_flush_say_where_the_stream_goes_on() {
        let mut requests = requests();
        let record = text(requests.record(65_000, 4_000_000_000));
        assert!(record.starts_with("RECORD rtsp://192.168.1.5/1234 RTSP/1.0\r\n"));
        assert!(record.contains("Range: npt=0-\r\n"));
        assert!(record.contains("RTP-Info: seq=65000;rtptime=4000000000\r\n"));
        let flush = text(requests.flush(3, 9));
        assert!(flush.starts_with("FLUSH "));
        assert!(flush.contains("RTP-Info: seq=3;rtptime=9\r\n"));
    }

    #[test]
    fn volume_is_text_parameters_in_decibels_clamped_to_what_a_receiver_takes() {
        let body = |decibels| {
            let request = text(requests().volume(decibels));
            let (head, body) = request.split_once("\r\n\r\n").unwrap();
            assert!(head.contains("Content-Type: text/parameters\r\n"));
            body.to_owned()
        };
        assert_eq!(body(-12.5), "volume: -12.500000\r\n");
        assert_eq!(body(0.0), "volume: 0.000000\r\n");
        assert_eq!(body(6.0), "volume: 0.000000\r\n");
        assert_eq!(body(-80.0), "volume: -30.000000\r\n");
        assert_eq!(body(MUTED), "volume: -144.000000\r\n");
    }

    #[test]
    fn a_reply_is_read_with_its_headers_and_body_and_waits_for_the_rest() {
        let whole = b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: 4\r\nPublic: A, B\r\n\r\nbodyRTSP/1.0 200";
        let (reply, used) = Reply::parse(whole).unwrap().unwrap();
        assert!(reply.is_ok());
        assert_eq!(reply.header("public"), Some("A, B"));
        assert_eq!(&whole[used..], b"RTSP/1.0 200");
        assert_eq!(Reply::parse(&whole[..whole.len() - 14]).unwrap(), None);
        assert_eq!(Reply::parse(b"RTSP/1.0 200 OK\r\nCSe").unwrap(), None);
    }

    #[test]
    fn a_refusal_is_a_reply_that_is_not_ok() {
        let (reply, _) = Reply::parse(b"RTSP/1.0 453 Not Enough Bandwidth\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!((reply.status, reply.is_ok()), (453, false));
    }

    #[test]
    fn what_is_not_rtsp_or_too_large_is_malformed() {
        assert!(Reply::parse(b"HTTP/1.1 200 OK\r\n\r\n").is_err());
        assert!(Reply::parse(b"garbage\r\n\r\n").is_err());
        assert!(Reply::parse(b"RTSP/1.0 200 OK\r\nContent-Length: 99999999\r\n\r\n").is_err());
        assert!(Reply::parse(b"RTSP/1.0 200 OK\r\nContent-Length: x\r\n\r\n").is_err());
        assert!(Reply::parse(&vec![b'a'; MAX_HEAD + 1]).is_err());
    }

    #[test]
    fn the_ports_come_from_the_transport_and_a_reply_without_them_is_refused() {
        let reply = |transport: &str| {
            let text = format!("RTSP/1.0 200 OK\r\nTransport: {transport}\r\n\r\n");
            Reply::parse(text.as_bytes()).unwrap().unwrap().0
        };
        assert_eq!(
            reply("RTP/AVP/UDP;unicast;mode=record;server_port=6000;control_port=6001;timing_port=6002")
                .ports(),
            Ok(Ports {
                audio: 6000,
                control: 6001
            })
        );
        assert!(
            reply("RTP/AVP/UDP;unicast;server_port=6000")
                .ports()
                .is_err()
        );
        assert!(
            reply("RTP/AVP/UDP;server_port=0;control_port=1")
                .ports()
                .is_err()
        );
        let none = Reply::parse(b"RTSP/1.0 200 OK\r\n\r\n").unwrap().unwrap().0;
        assert!(none.ports().is_err());
    }
}
