//! The session against a stand-in for a Cast device: a TLS server on this
//! machine that speaks the protocol's framing and answers as a receiver
//! does.

use std::net::{Ipv4Addr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use network_output::{Device, Media, NetworkOutput, OutputState, OutputStatus, Protocol};
use rust_cast::message_manager::{CastMessage, CastMessagePayload, MessageManager};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

const HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const MEDIA: &str = "urn:x-cast:com.google.cast.media";

/// What the stand-in device keeps of the player it runs.
struct Player {
    state: &'static str,
    position: f64,
    idle_reason: Option<&'static str>,
}

struct FakeDevice {
    port: u16,
    /// The `type` of each message received, in order.
    received: Arc<Mutex<Vec<String>>>,
    hang_up: Arc<AtomicBool>,
    player: Arc<Mutex<Player>>,
}

impl FakeDevice {
    fn start() -> FakeDevice {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certified.signing_key.serialize_der(),
        ));
        let config = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![certified.cert.der().clone()], key)
                .unwrap(),
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let device = FakeDevice {
            port: listener.local_addr().unwrap().port(),
            received: Arc::default(),
            hang_up: Arc::default(),
            player: Arc::new(Mutex::new(Player {
                state: "IDLE",
                position: 0.0,
                idle_reason: None,
            })),
        };
        let (received, hang_up, player) = (
            device.received.clone(),
            device.hang_up.clone(),
            device.player.clone(),
        );
        std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let stream = StreamOwned::new(ServerConnection::new(config).unwrap(), tcp);
            let messages = MessageManager::new(stream);
            // The session ends the conversation by hanging up or by us.
            while let Ok(message) = messages.receive() {
                if hang_up.load(Ordering::SeqCst) {
                    return;
                }
                let CastMessagePayload::String(payload) = &message.payload else {
                    continue;
                };
                let kind = field(payload, "type").unwrap_or_default();
                received.lock().unwrap().push(kind.clone());
                if let Some(reply) = answer(
                    &message.namespace,
                    &kind,
                    payload,
                    &mut player.lock().unwrap(),
                ) {
                    messages
                        .send(CastMessage {
                            namespace: reply.0,
                            source: message.destination.clone(),
                            destination: message.source.clone(),
                            payload: CastMessagePayload::String(reply.1),
                        })
                        .unwrap();
                }
            }
        });
        device
    }

    fn device(&self) -> Device {
        Device {
            protocol: Protocol::Cast,
            id: "fake".into(),
            name: "Fake".into(),
            model: None,
            address: Ipv4Addr::LOCALHOST,
            port: self.port,
        }
    }

    fn received(&self) -> Vec<String> {
        self.received.lock().unwrap().clone()
    }
}

/// The value of a top-level `"name":` in a flat JSON payload, a string
/// without its quotes or a number as it is written.
fn field(payload: &str, name: &str) -> Option<String> {
    let after = payload.split_once(&format!("\"{name}\":"))?.1.trim_start();
    let value: String = match after.strip_prefix('"') {
        Some(quoted) => quoted.chars().take_while(|&c| c != '"').collect(),
        None => after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect(),
    };
    Some(value)
}

fn answer(
    namespace: &str,
    kind: &str,
    payload: &str,
    player: &mut Player,
) -> Option<(String, String)> {
    let request_id = field(payload, "requestId").unwrap_or_default();
    match (namespace, kind) {
        (HEARTBEAT, "PING") => Some((HEARTBEAT.into(), r#"{"type":"PONG"}"#.into())),
        (RECEIVER, "LAUNCH") => Some((
            RECEIVER.into(),
            format!(
                r#"{{"type":"RECEIVER_STATUS","requestId":{request_id},"status":{{"volume":{{"level":1.0,"muted":false}},"applications":[{{"appId":"CC1AD845","sessionId":"session","transportId":"transport","namespaces":[{{"name":"{MEDIA}"}}],"displayName":"Default Media Receiver","statusText":""}}]}}}}"#
            ),
        )),
        (MEDIA, "LOAD") => {
            player.state = if payload.contains(r#""autoplay":false"#) {
                "PAUSED"
            } else {
                "PLAYING"
            };
            player.position = field(payload, "currentTime")
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0);
            player.idle_reason = None;
            Some(media_status(&request_id, player))
        }
        (MEDIA, "PAUSE") => {
            player.state = "PAUSED";
            Some(media_status(&request_id, player))
        }
        (MEDIA, "PLAY") => {
            player.state = "PLAYING";
            Some(media_status(&request_id, player))
        }
        (MEDIA, "SEEK") => {
            player.position = field(payload, "currentTime")
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0);
            Some(media_status(&request_id, player))
        }
        (MEDIA, "STOP") => {
            player.state = "IDLE";
            player.idle_reason = Some("CANCELLED");
            Some(media_status(&request_id, player))
        }
        (MEDIA, "GET_STATUS") => Some(media_status(&request_id, player)),
        _ => None,
    }
}

fn media_status(request_id: &str, player: &Player) -> (String, String) {
    let idle_reason = player
        .idle_reason
        .map(|reason| format!(r#","idleReason":"{reason}""#))
        .unwrap_or_default();
    (
        MEDIA.into(),
        format!(
            r#"{{"type":"MEDIA_STATUS","requestId":{request_id},"status":[{{"mediaSessionId":7,"playbackRate":1.0,"playerState":"{}","currentTime":{},"supportedMediaCommands":15{idle_reason}}}]}}"#,
            player.state, player.position
        ),
    )
}

fn song(start: f64, paused: bool) -> Media {
    Media {
        url: "http://127.0.0.1:1/token".into(),
        content_type: "audio/flac".into(),
        start,
        paused,
    }
}

/// Polls the session until `wanted` holds for what it says.
fn wait_for(session: &dyn NetworkOutput, wanted: impl Fn(&OutputStatus) -> bool) -> OutputStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = session.status().unwrap();
        if wanted(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the session never said so; last: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_session_loads_pauses_and_seeks_on_the_device_and_reports_what_it_answers() {
    let fake = FakeDevice::start();
    let mut session = fake.device().connect();
    assert_eq!(session.status().unwrap().state, OutputState::Loading);

    session.load(song(12.0, false));
    let status = wait_for(session.as_ref(), |s| {
        s.loads == 1 && s.state == OutputState::Playing
    });
    assert!(status.position >= 12.0, "{status:?}");

    session.set_paused(true);
    wait_for(session.as_ref(), |s| s.state == OutputState::Paused);
    session.seek(90.0);
    let status = wait_for(session.as_ref(), |s| s.position >= 90.0);
    assert_eq!(status.state, OutputState::Paused);
    session.set_paused(false);
    wait_for(session.as_ref(), |s| s.state == OutputState::Playing);

    let received = fake.received();
    for expected in ["LAUNCH", "LOAD", "PAUSE", "SEEK", "PLAY"] {
        assert!(
            received.iter().any(|kind| kind == expected),
            "{expected} in {received:?}"
        );
    }
}

#[test]
fn a_song_loaded_paused_waits_for_play_and_one_the_device_finished_says_so() {
    let fake = FakeDevice::start();
    let mut session = fake.device().connect();
    session.load(song(0.0, true));
    wait_for(session.as_ref(), |s| {
        s.loads == 1 && s.state == OutputState::Paused
    });
    {
        let mut player = fake.player.lock().unwrap();
        player.state = "IDLE";
        player.idle_reason = Some("FINISHED");
    }
    wait_for(session.as_ref(), |s| s.state == OutputState::Finished);
}

#[test]
fn a_device_that_hangs_up_is_unavailable() {
    let fake = FakeDevice::start();
    let mut session = fake.device().connect();
    session.load(song(0.0, false));
    wait_for(session.as_ref(), |s| s.state == OutputState::Playing);
    fake.hang_up.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(10);
    while session.status().is_ok() {
        assert!(
            Instant::now() < deadline,
            "the session never noticed the device had gone"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn dropping_a_session_pauses_the_device() {
    let fake = FakeDevice::start();
    let mut session = fake.device().connect();
    session.load(song(0.0, false));
    wait_for(session.as_ref(), |s| s.state == OutputState::Playing);
    drop(session);
    assert_eq!(fake.player.lock().unwrap().state, "PAUSED");
}

#[test]
fn a_device_that_is_not_there_is_unavailable() {
    let unused = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let session = Device {
        protocol: Protocol::Cast,
        id: "gone".into(),
        name: "Gone".into(),
        model: None,
        address: Ipv4Addr::LOCALHOST,
        port: unused,
    }
    .connect();
    let deadline = Instant::now() + Duration::from_secs(10);
    while session.status().is_ok() {
        assert!(
            Instant::now() < deadline,
            "the session never said the device was not there"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
