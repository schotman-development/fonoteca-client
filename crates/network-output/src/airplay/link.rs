//! The connection to one receiver: the RTSP conversation over TCP that sets
//! the stream up, and the UDP sockets the audio, its clock and its
//! retransmits go over.

use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::alac::{self, FRAMES_PER_PACKET, SAMPLES_PER_PACKET};
use super::rtp::{self, Backlog, LATENCY_FRAMES, Ntp, SAMPLE_RATE, Stream};
use super::rtsp::{Malformed, Ports, Reply, Requests};
use super::{Command, Heard};
use crate::device::Device;
use crate::output::{OpenPcm, OutputState};

/// How long the receiver is given to answer a request.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest the thread waits for a command when nothing is due, which is
/// also how soon it answers the receiver's timing requests.
const TICK: Duration = Duration::from_millis(10);
const SYNC_EVERY: Duration = Duration::from_secs(1);
/// A receiver drops a connection that is quiet for too long.
const KEEP_ALIVE_EVERY: Duration = Duration::from_secs(15);
/// The volume a session starts at, in dB: a receiver keeps what the sender
/// before it left, which may be the loudest.
const START_VOLUME: f32 = -15.0;

#[derive(Debug)]
pub(super) enum Error {
    Io(io::Error),
    Refused { request: &'static str, status: u16 },
    Malformed(Malformed),
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(problem) => write!(f, "The device stopped answering: {problem}."),
            Error::Refused { request, status } => {
                write!(f, "The device refused {request} with status {status}.")
            }
            Error::Malformed(problem) => problem.fmt(f),
            Error::Closed => f.write_str("The device closed the connection."),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(problem: io::Error) -> Self {
        Error::Io(problem)
    }
}

impl From<Malformed> for Error {
    fn from(problem: Malformed) -> Self {
        Error::Malformed(problem)
    }
}

/// What is sent to the receiver now.
enum Mode {
    /// Nothing was loaded yet.
    Waiting,
    Paused {
        at: f64,
    },
    Streaming(Run),
    Finished {
        at: f64,
    },
    /// The track could not be decoded.
    Failed(String),
}

/// A track on its way to the receiver from `base` seconds into it.
struct Run {
    pcm: Box<dyn Read + Send>,
    base: f64,
    started: Instant,
    packets: u64,
    last_sync: Option<Instant>,
    /// The track has no more to send; the receiver plays what it holds.
    all_sent: bool,
}

impl Run {
    fn sent_seconds(&self) -> f64 {
        (self.packets * FRAMES_PER_PACKET as u64) as f64 / f64::from(SAMPLE_RATE)
    }

    /// When the next packet is due: they are sent as fast as they play.
    fn due(&self) -> Instant {
        self.started + Duration::from_secs_f64(self.sent_seconds())
    }

    fn latency() -> f64 {
        f64::from(LATENCY_FRAMES) / f64::from(SAMPLE_RATE)
    }

    /// Where the receiver is in the track: what it was sent, less what it
    /// still holds before it plays it.
    fn heard(&self) -> f64 {
        self.base + (self.sent_seconds() - Self::latency()).max(0.0)
    }

    fn has_played_out(&self, now: Instant) -> bool {
        self.all_sent && now >= self.due() + Duration::from_secs_f64(Self::latency())
    }
}

pub(super) struct Link {
    rtsp: TcpStream,
    requests: Requests,
    receiver: Ipv4Addr,
    ports: Ports,
    audio: UdpSocket,
    control: UdpSocket,
    timing: UdpSocket,
    stream: Stream,
    backlog: Backlog,
    open_pcm: OpenPcm,
    track: Option<PathBuf>,
    loads: u64,
    mode: Mode,
    last_contact: Instant,
}

impl Link {
    pub fn open(device: &Device, open_pcm: OpenPcm) -> Result<Link, Error> {
        let mut rtsp = TcpStream::connect_timeout(
            &SocketAddr::from((device.address, device.port)),
            IO_TIMEOUT,
        )?;
        rtsp.set_read_timeout(Some(IO_TIMEOUT))?;
        rtsp.set_write_timeout(Some(IO_TIMEOUT))?;
        let SocketAddr::V4(local) = rtsp.local_addr()? else {
            unreachable!("connected to an IPv4 address");
        };
        let local = *local.ip();
        let audio = UdpSocket::bind((local, 0))?;
        let control = UdpSocket::bind((local, 0))?;
        let timing = UdpSocket::bind((local, 0))?;
        control.set_nonblocking(true)?;
        timing.set_nonblocking(true)?;

        let mut requests = Requests::new(local, device.address, random() as u32);
        let stream = Stream::new(random() as u32, random() as u16, random() as u32);
        exchange(&mut rtsp, &requests.options(), "OPTIONS")?;
        exchange(&mut rtsp, &requests.announce(), "ANNOUNCE")?;
        let setup = requests.setup(control.local_addr()?.port(), timing.local_addr()?.port());
        let reply = exchange(&mut rtsp, &setup, "SETUP")?;
        requests.join(&reply);
        let ports = reply.ports()?;
        let record = requests.record(stream.sequence(), stream.timestamp());
        exchange(&mut rtsp, &record, "RECORD")?;
        exchange(&mut rtsp, &requests.volume(START_VOLUME), "SET_PARAMETER")?;
        Ok(Link {
            rtsp,
            requests,
            receiver: device.address,
            ports,
            audio,
            control,
            timing,
            stream,
            backlog: Backlog::default(),
            open_pcm,
            track: None,
            loads: 0,
            mode: Mode::Waiting,
            last_contact: Instant::now(),
        })
    }

    pub fn serve(
        mut self,
        commands: &mpsc::Receiver<Command>,
        heard: &Mutex<Heard>,
    ) -> Result<(), Error> {
        loop {
            match commands.recv_timeout(self.wait(Instant::now())) {
                Ok(Command::Leave) | Err(RecvTimeoutError::Disconnected) => return self.leave(),
                Ok(command) => self.handle(command)?,
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.answer_receiver()?;
            self.send_due(Instant::now())?;
            self.keep_alive()?;
            self.publish(heard);
        }
    }

    fn wait(&self, now: Instant) -> Duration {
        match &self.mode {
            Mode::Streaming(run) if !run.all_sent => {
                run.due().saturating_duration_since(now).min(TICK)
            }
            _ => TICK,
        }
    }

    fn handle(&mut self, command: Command) -> Result<(), Error> {
        match command {
            Command::Load(media) => {
                self.stop_streaming()?;
                self.track = Some(media.path);
                self.loads += 1;
                if media.paused {
                    self.mode = Mode::Paused { at: media.start };
                } else {
                    self.begin(media.start);
                }
            }
            Command::Pause(true) => {
                if let Mode::Streaming(run) = &self.mode {
                    let at = run.heard();
                    self.stop_streaming()?;
                    self.mode = Mode::Paused { at };
                }
            }
            Command::Pause(false) => {
                if let Mode::Paused { at } = self.mode {
                    self.begin(at);
                }
            }
            Command::Seek(seconds) => match self.mode {
                Mode::Streaming(_) => {
                    self.stop_streaming()?;
                    self.begin(seconds);
                }
                Mode::Paused { .. } => self.mode = Mode::Paused { at: seconds },
                _ => {}
            },
            Command::Stop => {
                self.stop_streaming()?;
                self.track = None;
                self.mode = Mode::Paused { at: 0.0 };
            }
            Command::Leave => unreachable!("Leave ends the loop before it is handled"),
        }
        Ok(())
    }

    /// Has the receiver drop what it holds, if anything was sent.
    fn stop_streaming(&mut self) -> Result<(), Error> {
        if !matches!(self.mode, Mode::Streaming(_)) {
            return Ok(());
        }
        let flush = self
            .requests
            .flush(self.stream.sequence(), self.stream.timestamp());
        exchange(&mut self.rtsp, &flush, "FLUSH")?;
        self.last_contact = Instant::now();
        self.backlog.clear();
        self.stream.restart();
        self.mode = Mode::Waiting;
        Ok(())
    }

    /// Decodes the track from `at` seconds and has it sent.
    fn begin(&mut self, at: f64) {
        let Some(path) = &self.track else { return };
        self.mode = match (self.open_pcm)(path, at) {
            Ok(pcm) => Mode::Streaming(Run {
                pcm,
                base: at,
                started: Instant::now(),
                packets: 0,
                last_sync: None,
                all_sent: false,
            }),
            Err(why) => Mode::Failed(why),
        };
    }

    /// Sends the packets that are due, and the sync packet that ties their
    /// timestamps to the clock, once a second.
    fn send_due(&mut self, now: Instant) -> Result<(), Error> {
        let Mode::Streaming(run) = &mut self.mode else {
            return Ok(());
        };
        let receiver = self.receiver;
        while !run.all_sent && now >= run.due() {
            let due_for_sync = run
                .last_sync
                .is_none_or(|sent| now.duration_since(sent) >= SYNC_EVERY);
            if due_for_sync {
                let sync =
                    rtp::sync_packet(self.stream.is_first(), ntp_now(), self.stream.timestamp());
                self.control
                    .send_to(&sync, SocketAddr::from((receiver, self.ports.control)))?;
                run.last_sync = Some(now);
            }
            let samples = match read_samples(&mut run.pcm) {
                Ok(Some(samples)) => samples,
                Ok(None) => {
                    run.all_sent = true;
                    break;
                }
                Err(problem) => {
                    self.mode = Mode::Failed(format!("The track couldn't be decoded: {problem}"));
                    return Ok(());
                }
            };
            let sequence = self.stream.sequence();
            let packet = self.stream.packet(&alac::escape_frame(&samples));
            self.audio
                .send_to(&packet, SocketAddr::from((receiver, self.ports.audio)))?;
            self.backlog.keep(sequence, packet);
            run.packets += 1;
        }
        if run.has_played_out(now) {
            let at = run.base + run.sent_seconds();
            self.mode = Mode::Finished { at };
        }
        Ok(())
    }

    /// Answers what the receiver asks over the timing and control ports.
    fn answer_receiver(&self) -> Result<(), Error> {
        for socket in [&self.timing, &self.control] {
            let mut datagram = [0; 64];
            loop {
                let (length, from) = match socket.recv_from(&mut datagram) {
                    Ok(received) => received,
                    Err(problem) if problem.kind() == io::ErrorKind::WouldBlock => break,
                    Err(problem) => return Err(problem.into()),
                };
                let received = ntp_now();
                // Anyone on the network can send to these ports.
                if from.ip() != IpAddr::V4(self.receiver) {
                    continue;
                }
                match rtp::parse_request(&datagram[..length]) {
                    Some(rtp::Request::Timing { sent }) => {
                        socket.send_to(&rtp::timing_reply(sent, received, ntp_now()), from)?;
                    }
                    Some(rtp::Request::Retransmit { first, count }) => {
                        for reply in self.backlog.replies(first, count) {
                            socket.send_to(&reply, from)?;
                        }
                    }
                    None => {}
                }
            }
        }
        Ok(())
    }

    fn keep_alive(&mut self) -> Result<(), Error> {
        if self.last_contact.elapsed() < KEEP_ALIVE_EVERY {
            return Ok(());
        }
        exchange(&mut self.rtsp, &self.requests.options(), "OPTIONS")?;
        self.last_contact = Instant::now();
        Ok(())
    }

    fn publish(&self, heard: &Mutex<Heard>) {
        let (state, position) = match &self.mode {
            Mode::Waiting => (OutputState::Loading, 0.0),
            Mode::Paused { at } => (OutputState::Paused, *at),
            Mode::Streaming(run) => (OutputState::Playing, run.heard()),
            Mode::Finished { at } => (OutputState::Finished, *at),
            Mode::Failed(why) => (OutputState::Failed(why.clone()), 0.0),
        };
        let mut heard = heard.lock().unwrap();
        heard.loads = self.loads;
        heard.state = state;
        heard.position = position;
    }

    fn leave(mut self) -> Result<(), Error> {
        exchange(&mut self.rtsp, &self.requests.teardown(), "TEARDOWN").map(drop)
    }
}

/// Sends `request` and reads its reply, which must be an OK.
fn exchange(rtsp: &mut TcpStream, request: &[u8], name: &'static str) -> Result<Reply, Error> {
    rtsp.write_all(request)?;
    let mut received = Vec::new();
    let mut chunk = [0; 2048];
    loop {
        if let Some((reply, _)) = Reply::parse(&received)? {
            return if reply.is_ok() {
                Ok(reply)
            } else {
                Err(Error::Refused {
                    request: name,
                    status: reply.status,
                })
            };
        }
        match rtsp.read(&mut chunk)? {
            0 => return Err(Error::Closed),
            read => received.extend_from_slice(&chunk[..read]),
        }
    }
}

/// The next packet of samples; the last of a track is padded with silence.
/// None once the track has run out.
fn read_samples(pcm: &mut dyn Read) -> io::Result<Option<[i16; SAMPLES_PER_PACKET]>> {
    let mut bytes = [0; SAMPLES_PER_PACKET * 2];
    let mut filled = 0;
    while filled < bytes.len() {
        match pcm.read(&mut bytes[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    if filled == 0 {
        return Ok(None);
    }
    let mut samples = [0; SAMPLES_PER_PACKET];
    for (sample, pair) in samples.iter_mut().zip(bytes.as_chunks::<2>().0) {
        *sample = i16::from_ne_bytes(*pair);
    }
    Ok(Some(samples))
}

fn ntp_now() -> Ntp {
    Ntp::from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default(),
    )
}

fn random() -> u64 {
    RandomState::new().hash_one(SystemTime::now())
}
