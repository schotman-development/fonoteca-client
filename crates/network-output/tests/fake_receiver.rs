//! The AirPlay session against a stand-in for a receiver: an RTSP server and
//! the three UDP ports of a RAOP receiver on this machine, that answer as one
//! does and keep what they were sent.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use network_output::{Device, Media, NetworkOutput, OpenPcm, OutputState, OutputStatus, Protocol};

const FRAMES_PER_PACKET: usize = 352;
const RATE: f64 = 44_100.0;

struct Request {
    method: String,
    head: String,
    body: String,
    /// How many audio packets had arrived when it did.
    audio_seen: usize,
}

#[derive(Default)]
struct Kept {
    requests: Vec<Request>,
    audio: Vec<Vec<u8>>,
    control: Vec<Vec<u8>>,
    timing: Vec<Vec<u8>>,
    /// Where the session listens for timing and retransmits, from SETUP.
    session_control: Option<SocketAddr>,
    session_timing: Option<SocketAddr>,
}

struct FakeReceiver {
    port: u16,
    kept: Arc<Mutex<Kept>>,
    control: Arc<UdpSocket>,
    timing: Arc<UdpSocket>,
    /// Answers every request with this status instead of 200.
    refuse: Arc<Mutex<Option<u16>>>,
}

impl FakeReceiver {
    fn start() -> FakeReceiver {
        Self::start_answering(None)
    }

    fn start_answering(refuse: Option<u16>) -> FakeReceiver {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let udp = || Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap());
        let (audio, control, timing) = (udp(), udp(), udp());
        let ports = [
            audio.local_addr(),
            control.local_addr(),
            timing.local_addr(),
        ]
        .map(|address| address.unwrap().port());
        let receiver = FakeReceiver {
            port: listener.local_addr().unwrap().port(),
            kept: Arc::default(),
            control: control.clone(),
            timing: timing.clone(),
            refuse: Arc::new(Mutex::new(refuse)),
        };
        listen(&audio, &receiver.kept, |kept, datagram| {
            kept.audio.push(datagram)
        });
        listen(&control, &receiver.kept, |kept, datagram| {
            kept.control.push(datagram)
        });
        listen(&timing, &receiver.kept, |kept, datagram| {
            kept.timing.push(datagram)
        });
        let (kept, refuse) = (receiver.kept.clone(), receiver.refuse.clone());
        std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            converse(tcp, &kept, &refuse, ports);
        });
        receiver
    }

    fn device(&self) -> Device {
        Device {
            protocol: Protocol::AirPlay,
            id: "A1B2C3D4E5F6".into(),
            name: "Fake".into(),
            model: None,
            address: Ipv4Addr::LOCALHOST,
            port: self.port,
        }
    }

    fn requests(&self) -> Vec<String> {
        let kept = self.kept.lock().unwrap();
        kept.requests.iter().map(|r| r.method.clone()).collect()
    }

    fn audio(&self) -> Vec<Vec<u8>> {
        self.kept.lock().unwrap().audio.clone()
    }

    fn request(&self, method: &str) -> Request {
        let kept = self.kept.lock().unwrap();
        let found = kept.requests.iter().rev().find(|r| r.method == method);
        let found = found.unwrap_or_else(|| panic!("no {method} was received"));
        Request {
            method: found.method.clone(),
            head: found.head.clone(),
            body: found.body.clone(),
            audio_seen: found.audio_seen,
        }
    }
}

/// Keeps every datagram that arrives on `socket`.
fn listen(socket: &Arc<UdpSocket>, kept: &Arc<Mutex<Kept>>, keep: fn(&mut Kept, Vec<u8>)) {
    let (socket, kept) = (socket.clone(), kept.clone());
    std::thread::spawn(move || {
        let mut datagram = [0; 2048];
        while let Ok((length, _)) = socket.recv_from(&mut datagram) {
            keep(&mut kept.lock().unwrap(), datagram[..length].to_vec());
        }
    });
}

fn converse(mut tcp: TcpStream, kept: &Mutex<Kept>, refuse: &Mutex<Option<u16>>, ports: [u16; 3]) {
    let mut received = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let Some((head, body)) = take_request(&mut received) else {
            match tcp.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(read) => {
                    received.extend_from_slice(&chunk[..read]);
                    continue;
                }
            }
        };
        let method = head.split(' ').next().unwrap().to_owned();
        let mut guard = kept.lock().unwrap();
        if method == "SETUP" {
            let port = |name: &str| {
                let value = head.split(&format!("{name}=")).nth(1).unwrap();
                value
                    .split(';')
                    .next()
                    .unwrap()
                    .trim()
                    .parse::<u16>()
                    .unwrap()
            };
            guard.session_control = Some((Ipv4Addr::LOCALHOST, port("control_port")).into());
            guard.session_timing = Some((Ipv4Addr::LOCALHOST, port("timing_port")).into());
        }
        let audio_seen = guard.audio.len();
        guard.requests.push(Request {
            method: method.clone(),
            head: head.clone(),
            body,
            audio_seen,
        });
        drop(guard);
        let cseq = head
            .lines()
            .find_map(|line| line.strip_prefix("CSeq: "))
            .unwrap();
        let status = refuse.lock().unwrap().unwrap_or(200);
        let mut reply = format!("RTSP/1.0 {status} Whatever\r\nCSeq: {cseq}\r\n");
        if method == "SETUP" {
            reply.push_str(&format!(
                "Session: 1\r\nTransport: RTP/AVP/UDP;unicast;mode=record;server_port={};control_port={};timing_port={}\r\n",
                ports[0], ports[1], ports[2]
            ));
        }
        reply.push_str("\r\n");
        if tcp.write_all(reply.as_bytes()).is_err() {
            return;
        }
    }
}

/// The first whole request in `received`, taken out of it.
fn take_request(received: &mut Vec<u8>) -> Option<(String, String)> {
    let head_end = received.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8(received[..head_end].to_vec()).unwrap();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .map_or(0, |length| length.parse::<usize>().unwrap());
    let total = head_end + 4 + length;
    if received.len() < total {
        return None;
    }
    let body = String::from_utf8(received[head_end + 4..total].to_vec()).unwrap();
    received.drain(..total);
    Some((head, body))
}

/// The samples of frame `frame` of the track the stand-in decoder makes.
fn sample(frame: u64, channel: u64) -> i16 {
    ((frame * 7 + channel * 1000) % 20_000) as i16 - 10_000
}

/// A decoder of a track of `frames` frames, which starts where asked.
struct Track {
    next: u64,
    end: u64,
}

impl Read for Track {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while self.next < self.end && buf.len() - written >= 4 {
            for channel in 0..2 {
                buf[written..written + 2]
                    .copy_from_slice(&sample(self.next, channel).to_ne_bytes());
                written += 2;
            }
            self.next += 1;
        }
        Ok(written)
    }
}

fn decoder(frames: u64) -> OpenPcm {
    Arc::new(move |_, start| {
        Ok(Box::new(Track {
            next: (start * RATE) as u64,
            end: frames,
        }))
    })
}

fn media(start: f64, paused: bool) -> Media {
    Media {
        url: "http://unused/".into(),
        path: PathBuf::from("unused"),
        start,
        paused,
        content_type: "audio/flac".into(),
    }
}

fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn status_is(output: &dyn NetworkOutput, wanted: fn(&OutputStatus) -> bool) -> bool {
    output.status().is_ok_and(|status| wanted(&status))
}

fn seq(packet: &[u8]) -> u16 {
    u16::from_be_bytes([packet[2], packet[3]])
}

fn timestamp(packet: &[u8]) -> u32 {
    u32::from_be_bytes(packet[4..8].try_into().unwrap())
}

/// The samples an ALAC escape frame holds, read as a decoder reads it.
fn escaped_samples(payload: &[u8]) -> Vec<i16> {
    let bits = |at: usize, count: usize| {
        (at..at + count).fold(0u32, |value, bit| {
            (value << 1) | u32::from(payload[bit / 8] >> (7 - bit % 8) & 1)
        })
    };
    assert_eq!(bits(0, 3), 1, "a channel pair");
    assert_eq!(bits(19, 4), 1, "an escaped frame");
    (0..FRAMES_PER_PACKET * 2)
        .map(|n| bits(23 + n * 16, 16) as u16 as i16)
        .collect()
}

fn expected(first_frame: u64) -> Vec<i16> {
    (first_frame..first_frame + FRAMES_PER_PACKET as u64)
        .flat_map(|frame| [sample(frame, 0), sample(frame, 1)])
        .collect()
}

#[test]
fn the_session_sets_the_stream_up_and_sends_the_track_as_numbered_frames_that_hold_its_samples() {
    let receiver = FakeReceiver::start();
    let mut output = receiver.device().connect(decoder(RATE as u64 * 30));
    output.load(media(0.0, false));
    until("some audio", || receiver.audio().len() >= 5);

    let requests = receiver.requests();
    assert_eq!(
        requests[..5],
        ["OPTIONS", "ANNOUNCE", "SETUP", "RECORD", "SET_PARAMETER"]
    );
    assert!(
        receiver
            .request("ANNOUNCE")
            .body
            .contains("a=rtpmap:96 AppleLossless")
    );
    assert!(
        receiver
            .request("SET_PARAMETER")
            .body
            .starts_with("volume: -15.000000")
    );
    assert!(receiver.request("RECORD").head.contains("RTP-Info: seq="));

    let audio = receiver.audio();
    assert_eq!(audio[0][1], 0xE0, "marked as the first");
    for (n, packet) in audio.iter().enumerate().take(5) {
        assert_eq!(packet[0], 0x80);
        assert_eq!(seq(packet), seq(&audio[0]).wrapping_add(n as u16));
        assert_eq!(
            timestamp(packet),
            timestamp(&audio[0]).wrapping_add((n * FRAMES_PER_PACKET) as u32)
        );
        if n > 0 {
            assert_eq!(packet[1], 0x60);
        }
        assert_eq!(
            escaped_samples(&packet[12..]),
            expected((n * FRAMES_PER_PACKET) as u64)
        );
    }
    let record = receiver.request("RECORD").head;
    let started: u16 = record
        .split("seq=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        seq(&audio[0]),
        started,
        "the stream goes on from where RECORD said"
    );
    assert!(status_is(&*output, |s| s.state == OutputState::Playing
        && s.loads == 1));
}

#[test]
fn a_sync_packet_precedes_the_audio_and_ties_its_timestamp_to_the_clock() {
    let receiver = FakeReceiver::start();
    let mut output = receiver.device().connect(decoder(RATE as u64 * 30));
    output.load(media(0.0, false));
    until("a sync packet", || {
        !receiver.kept.lock().unwrap().control.is_empty()
    });
    until("audio", || !receiver.audio().is_empty());
    let sync = receiver.kept.lock().unwrap().control[0].clone();
    assert_eq!(
        &sync[..4],
        [0x90, 0xD4, 0, 7],
        "the first, with the extension bit"
    );
    let first = receiver.audio()[0].clone();
    assert_eq!(
        u32::from_be_bytes(sync[16..20].try_into().unwrap()),
        timestamp(&first)
    );
    assert_eq!(
        u32::from_be_bytes(sync[4..8].try_into().unwrap()),
        timestamp(&first).wrapping_sub(88_200)
    );
}

#[test]
fn timing_requests_and_retransmit_requests_are_answered() {
    let receiver = FakeReceiver::start();
    let mut output = receiver.device().connect(decoder(RATE as u64 * 30));
    output.load(media(0.0, false));
    until("audio", || receiver.audio().len() >= 3);
    let (control, timing) = {
        let kept = receiver.kept.lock().unwrap();
        (kept.session_control.unwrap(), kept.session_timing.unwrap())
    };

    let mut request = [0; 32];
    request[..4].copy_from_slice(&[0x80, 0xD2, 0, 7]);
    request[24..32].copy_from_slice(&0x1122_3344_5566_7788u64.to_be_bytes());
    receiver.timing.send_to(&request, timing).unwrap();
    until("a timing reply", || {
        !receiver.kept.lock().unwrap().timing.is_empty()
    });
    let reply = receiver.kept.lock().unwrap().timing[0].clone();
    assert_eq!(&reply[..2], [0x80, 0xD3]);
    assert_eq!(
        &reply[8..16],
        0x1122_3344_5566_7788u64.to_be_bytes(),
        "our send time comes back"
    );
    assert!(reply[16..24] != [0; 8] && reply[24..32] != [0; 8]);

    let wanted = receiver.audio()[1].clone();
    let mut resend = vec![0x80, 0xD5, 0, 1];
    resend.extend_from_slice(&seq(&wanted).to_be_bytes());
    resend.extend_from_slice(&1u16.to_be_bytes());
    receiver.control.send_to(&resend, control).unwrap();
    until("a retransmit", || {
        receiver
            .kept
            .lock()
            .unwrap()
            .control
            .iter()
            .any(|d| d[1] == 0xD6)
    });
    let kept = receiver.kept.lock().unwrap();
    let reply = kept.control.iter().find(|d| d[1] == 0xD6).unwrap();
    assert_eq!(
        &reply[4..],
        &wanted[..],
        "the packet asked for, as it was sent"
    );
}

#[test]
fn pausing_flushes_and_stops_the_audio_and_playing_on_starts_again_from_where_it_was_heard() {
    let receiver = FakeReceiver::start();
    let mut output = receiver.device().connect(decoder(RATE as u64 * 30));
    output.load(media(0.0, false));
    until("audio", || receiver.audio().len() >= 3);
    output.set_paused(true);
    until("a flush", || {
        receiver.requests().contains(&"FLUSH".to_owned())
    });
    let flush = receiver.request("FLUSH");
    let sent = receiver.audio().len();
    std::thread::sleep(Duration::from_millis(100));
    assert!(sent <= receiver.audio().len() && receiver.audio().len() <= sent + 1);
    let next_seq = seq(&receiver.audio()[flush.audio_seen - 1]).wrapping_add(1);
    assert!(
        flush
            .head
            .contains(&format!("RTP-Info: seq={next_seq};rtptime="))
    );
    until("paused", || {
        status_is(&*output, |s| s.state == OutputState::Paused)
    });

    let before = receiver.audio().len();
    output.set_paused(false);
    until("audio again", || receiver.audio().len() > before + 1);
    let resumed = receiver.audio()[before].clone();
    assert_eq!(resumed[1], 0xE0, "marked as the first after a flush");
    assert_eq!(seq(&resumed), next_seq);
}

#[test]
fn a_seek_flushes_and_sends_the_track_from_the_second_asked_for() {
    let receiver = FakeReceiver::start();
    let mut output = receiver.device().connect(decoder(RATE as u64 * 30));
    output.load(media(0.0, false));
    until("audio", || receiver.audio().len() >= 3);
    output.seek(10.0);
    until("a flush", || {
        receiver.requests().contains(&"FLUSH".to_owned())
    });
    let flush = receiver.request("FLUSH");
    until("audio from the seek", || {
        receiver.audio().len() > flush.audio_seen
    });
    let after = receiver.audio()[flush.audio_seen].clone();
    assert_eq!(after[1], 0xE0);
    assert_eq!(escaped_samples(&after[12..]), expected(441_000));
}

#[test]
fn a_track_loaded_paused_sends_nothing_until_played_and_its_end_is_reported_once_it_has_played_out()
{
    let receiver = FakeReceiver::start();
    let mut output = receiver
        .device()
        .connect(decoder(FRAMES_PER_PACKET as u64 * 3 + 100));
    output.load(media(0.0, true));
    until("paused", || {
        status_is(&*output, |s| s.state == OutputState::Paused && s.loads == 1)
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(receiver.audio().is_empty());

    output.set_paused(false);
    until("all of it sent", || receiver.audio().len() == 4);
    let last = receiver.audio()[3].clone();
    let samples = escaped_samples(&last[12..]);
    assert_eq!(
        samples[..200],
        expected(FRAMES_PER_PACKET as u64 * 3)[..200]
    );
    assert!(
        samples[200..].iter().all(|&s| s == 0),
        "the rest is silence"
    );
    until("the end", || {
        status_is(&*output, |s| s.state == OutputState::Finished)
    });
    let position = output.status().unwrap().position;
    assert!((position - 1_408.0 / RATE).abs() < 1e-6, "{position}");
}

#[test]
fn a_track_that_cannot_be_decoded_is_a_failed_state() {
    let receiver = FakeReceiver::start();
    let failing: OpenPcm = Arc::new(|_, _| Err("The file is broken.".into()));
    let mut output = receiver.device().connect(failing);
    output.load(media(0.0, false));
    until("a failure", || {
        status_is(&*output, |s| {
            s.state == OutputState::Failed("The file is broken.".into())
        })
    });
}

#[test]
fn a_receiver_that_refuses_is_unavailable_with_the_request_it_refused() {
    let receiver = FakeReceiver::start_answering(Some(403));
    let output = receiver.device().connect(decoder(1));
    until("unavailable", || output.status().is_err());
    let problem = output.status().unwrap_err().to_string();
    assert!(
        problem.contains("OPTIONS") && problem.contains("403"),
        "{problem}"
    );
}

#[test]
fn dropping_the_session_tears_the_stream_down() {
    let receiver = FakeReceiver::start();
    let mut output = receiver.device().connect(decoder(RATE as u64 * 30));
    output.load(media(0.0, false));
    until("audio", || receiver.audio().len() >= 2);
    drop(output);
    assert_eq!(
        receiver.requests().last().map(String::as_str),
        Some("TEARDOWN")
    );
}
