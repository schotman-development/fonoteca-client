//! Google's Cast protocol: how a device announces itself, and playing to it
//! over the connection rust_cast makes. rust_cast
//! reads a connection with no way to give up, and a read holds the
//! connection against writes, so one thread does all of it in turn: it takes
//! the player's commands, and asks how the device is every second, which
//! also answers the device's pings. What it last heard is what the player
//! gets; a device that falls silent shows as one that stopped answering
//! while the thread waits on.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rust_cast::CastDevice;
use rust_cast::ChannelMessage;
use rust_cast::channels::connection::ConnectionResponse;
use rust_cast::channels::heartbeat::HeartbeatResponse;
use rust_cast::channels::media::{
    IdleReason, LoadOptions, Media as CastMedia, PlayerState, StatusEntry, StreamType,
};
use rust_cast::channels::receiver::CastDeviceApp;
use rust_cast::errors::Error;

use crate::device::{Announcement, Device, Protocol};
use crate::output::{Media, NetworkOutput, OutputState, OutputStatus, Unavailable};

const RECEIVER: &str = "receiver-0";
/// How often the device is asked how it is.
const POLL: Duration = Duration::from_secs(1);
/// A device that has not answered for this long has gone; it pings every
/// five seconds.
const SILENT_FOR: Duration = Duration::from_secs(15);
/// How long dropping a session waits for the device to be told to pause.
const PAUSE_WAIT: Duration = Duration::from_secs(2);

enum Command {
    Load(Media),
    Pause(bool),
    Seek(f64),
    Stop,
    /// Pauses and lets the device go.
    Leave,
}

/// What the thread last heard, for [`CastSession::status`].
struct Heard {
    started: Instant,
    loads: u64,
    latest: Option<(Instant, OutputState, f64)>,
    failure: Option<String>,
}

impl Heard {
    fn record(&mut self, entry: Option<&StatusEntry>, loaded: bool) {
        self.loads += u64::from(loaded);
        let (state, position) = entry.map_or((OutputState::Loading, 0.0), |entry| {
            (
                state_of(entry),
                f64::from(entry.current_time.unwrap_or_default()),
            )
        });
        self.latest = Some((Instant::now(), state, position));
    }
}

/// Plays on a Cast device that this session connects to in the background.
pub struct CastSession {
    commands: mpsc::Sender<Command>,
    heard: Arc<Mutex<Heard>>,
    /// Closes when the thread has ended.
    ended: mpsc::Receiver<()>,
}

impl CastSession {
    pub fn connect(device: &Device) -> Self {
        let (commands, received) = mpsc::channel();
        let (ending, ended) = mpsc::channel();
        let heard = Arc::new(Mutex::new(Heard {
            started: Instant::now(),
            loads: 0,
            latest: None,
            failure: None,
        }));
        let (device, shared) = (device.clone(), heard.clone());
        std::thread::Builder::new()
            .name("cast-session".into())
            .spawn(move || {
                let outcome = Link::open(&device).and_then(|link| link.serve(&received, &shared));
                if let Err(problem) = outcome {
                    shared.lock().unwrap().failure = Some(problem.to_string());
                }
                drop(ending);
            })
            .expect("a thread for the Cast session");
        CastSession {
            commands,
            heard,
            ended,
        }
    }

    fn send(&self, command: Command) {
        // The thread is gone when the device is: `status` says so.
        let _ = self.commands.send(command);
    }
}

impl NetworkOutput for CastSession {
    fn load(&mut self, media: Media) {
        self.send(Command::Load(media));
    }

    fn set_paused(&mut self, paused: bool) {
        self.send(Command::Pause(paused));
    }

    fn seek(&mut self, seconds: f64) {
        self.send(Command::Seek(seconds));
    }

    fn stop(&mut self) {
        self.send(Command::Stop);
    }

    fn status(&self) -> Result<OutputStatus, Unavailable> {
        let heard = self.heard.lock().unwrap();
        if let Some(problem) = &heard.failure {
            return Err(Unavailable(problem.clone()));
        }
        let loads = heard.loads;
        match &heard.latest {
            Some((at, state, position)) if at.elapsed() <= SILENT_FOR => {
                let played = if *state == OutputState::Playing {
                    at.elapsed().as_secs_f64()
                } else {
                    0.0
                };
                Ok(OutputStatus {
                    loads,
                    state: state.clone(),
                    position: position + played,
                })
            }
            None if heard.started.elapsed() <= SILENT_FOR => Ok(OutputStatus {
                loads,
                state: OutputState::Loading,
                position: 0.0,
            }),
            _ => Err(Unavailable("The device stopped answering.".into())),
        }
    }
}

impl Drop for CastSession {
    /// Leaving a device playing when the app ends would play on with nobody
    /// to stop it.
    fn drop(&mut self) {
        if self.commands.send(Command::Leave).is_err() {
            return;
        }
        if let Err(RecvTimeoutError::Timeout) = self.ended.recv_timeout(PAUSE_WAIT) {
            eprintln!("A Cast device didn't answer to being paused.");
        }
    }
}

/// The connection to the device and the player on it.
struct Link {
    device: CastDevice<'static>,
    transport_id: String,
    session_id: String,
    media_session: Option<i32>,
}

impl Link {
    fn open(device: &Device) -> Result<Link, Error> {
        // A Cast device's certificate is signed by Google for the device
        // alone, and names nothing a client could check it against.
        let cast =
            CastDevice::connect_without_host_verification(device.address.to_string(), device.port)?;
        cast.connection.connect(RECEIVER)?;
        let app = cast
            .receiver
            .launch_app(&CastDeviceApp::DefaultMediaReceiver)?;
        cast.connection.connect(app.transport_id.clone())?;
        Ok(Link {
            device: cast,
            transport_id: app.transport_id,
            session_id: app.session_id,
            media_session: None,
        })
    }

    fn serve(
        mut self,
        commands: &mpsc::Receiver<Command>,
        heard: &Mutex<Heard>,
    ) -> Result<(), Error> {
        loop {
            let (entry, loaded) = match commands.recv_timeout(POLL) {
                Ok(Command::Leave) | Err(RecvTimeoutError::Disconnected) => return self.pause(),
                Ok(Command::Load(media)) => (self.load(&media)?, true),
                Ok(Command::Pause(true)) => {
                    (self.control(|d, to, id| d.media.pause(to, id))?, false)
                }
                Ok(Command::Pause(false)) => {
                    (self.control(|d, to, id| d.media.play(to, id))?, false)
                }
                Ok(Command::Seek(seconds)) => (
                    self.control(|d, to, id| d.media.seek(to, id, Some(seconds as f32), None))?,
                    false,
                ),
                Ok(Command::Stop) => (self.control(|d, to, id| d.media.stop(to, id))?, false),
                Err(RecvTimeoutError::Timeout) => (self.ask()?, false),
            };
            heard.lock().unwrap().record(entry.as_ref(), loaded);
        }
    }

    fn load(&mut self, media: &Media) -> Result<Option<StatusEntry>, Error> {
        let status = self.device.media.load_with_opts(
            self.transport_id.clone(),
            self.session_id.clone(),
            &cast_media(media),
            LoadOptions {
                current_time: media.start,
                autoplay: !media.paused,
            },
        )?;
        let entry = status.entries.into_iter().next();
        self.media_session = entry.as_ref().map(|entry| entry.media_session_id);
        Ok(entry)
    }

    /// Has the device change what it plays; nothing when nothing is loaded.
    fn control(
        &self,
        change: impl FnOnce(&CastDevice<'static>, String, i32) -> Result<StatusEntry, Error>,
    ) -> Result<Option<StatusEntry>, Error> {
        let Some(session) = self.media_session else {
            return Ok(None);
        };
        change(&self.device, self.transport_id.clone(), session).map(Some)
    }

    /// Answers the device's pings, and asks how the player is.
    fn ask(&mut self) -> Result<Option<StatusEntry>, Error> {
        self.exchange_pings()?;
        let Some(session) = self.media_session else {
            return Ok(None);
        };
        let status = self
            .device
            .media
            .get_status(self.transport_id.clone(), Some(session))?;
        Ok(status.entries.into_iter().next())
    }

    /// Sends a ping and reads until its pong, answering the device's own
    /// pings on the way: the device hangs up on a sender that doesn't.
    fn exchange_pings(&self) -> Result<(), Error> {
        self.device.heartbeat.ping()?;
        loop {
            match self.device.receive()? {
                ChannelMessage::Heartbeat(HeartbeatResponse::Ping) => {
                    self.device.heartbeat.pong()?
                }
                ChannelMessage::Heartbeat(HeartbeatResponse::Pong) => return Ok(()),
                ChannelMessage::Connection(ConnectionResponse::Close) => {
                    return Err(Error::Internal("The device closed the connection.".into()));
                }
                _ => {}
            }
        }
    }

    fn pause(&self) -> Result<(), Error> {
        if let Some(session) = self.media_session {
            self.device
                .media
                .pause(self.transport_id.clone(), session)?;
        }
        Ok(())
    }
}

fn cast_media(media: &Media) -> CastMedia {
    CastMedia {
        content_id: media.url.clone(),
        stream_type: StreamType::Buffered,
        content_type: media.content_type.clone(),
        metadata: None,
        duration: None,
    }
}

fn state_of(entry: &StatusEntry) -> OutputState {
    match (entry.player_state, entry.idle_reason) {
        (PlayerState::Playing | PlayerState::Buffering, _) => OutputState::Playing,
        (PlayerState::Paused, _) => OutputState::Paused,
        (PlayerState::Idle, Some(IdleReason::Finished)) => OutputState::Finished,
        (PlayerState::Idle, Some(IdleReason::Error)) => {
            OutputState::Failed("The device couldn't play the file.".into())
        }
        (PlayerState::Idle, Some(IdleReason::Cancelled | IdleReason::Interrupted)) => {
            OutputState::Failed("Something else took over the device.".into())
        }
        (PlayerState::Idle, None) => OutputState::Loading,
    }
}

/// A Cast device is known by the id in its text record and named by its
/// friendly name; without an id or an IPv4 address nothing connects to it.
pub(crate) fn device(announcement: &Announcement) -> Option<Device> {
    let text = |key: &str| (announcement.text)(key).filter(|value| !value.is_empty());
    Some(Device {
        protocol: Protocol::Cast,
        id: text("id")?,
        name: text("fn").unwrap_or_else(|| announcement.instance()),
        model: text("md"),
        address: announcement.ipv4()?,
        port: announcement.port,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn device_from(text: &[(&str, &str)], addresses: Vec<IpAddr>) -> Option<Device> {
        let text: HashMap<String, String> = text
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let lookup = |key: &str| text.get(key).cloned();
        device(&Announcement {
            fullname: "Chromecast-abc._googlecast._tcp.local.",
            port: 8009,
            addresses,
            text: &lookup,
        })
    }

    fn lan() -> Vec<IpAddr> {
        vec![
            IpAddr::V6("fe80::1".parse().unwrap()),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
        ]
    }

    #[test]
    fn a_device_is_known_by_the_id_in_its_text_and_named_by_its_friendly_name() {
        let found = device_from(
            &[
                ("id", "9f8e"),
                ("fn", "Kitchen"),
                ("md", "Chromecast Audio"),
            ],
            lan(),
        );
        assert_eq!(
            found,
            Some(Device {
                protocol: Protocol::Cast,
                id: "9f8e".into(),
                name: "Kitchen".into(),
                model: Some("Chromecast Audio".into()),
                address: Ipv4Addr::new(192, 168, 1, 20),
                port: 8009,
            })
        );
    }

    #[test]
    fn a_device_without_a_friendly_name_is_named_by_its_instance() {
        let found = device_from(&[("id", "9f8e")], lan()).unwrap();
        assert_eq!(found.name, "Chromecast-abc");
        assert_eq!(found.model, None);
    }

    #[test]
    fn an_announcement_without_an_id_or_an_ipv4_address_is_no_device() {
        assert_eq!(device_from(&[("fn", "Kitchen")], lan()), None);
        let only_v6 = vec![IpAddr::V6("fe80::1".parse().unwrap())];
        assert_eq!(device_from(&[("id", "9f8e")], only_v6), None);
    }
}
