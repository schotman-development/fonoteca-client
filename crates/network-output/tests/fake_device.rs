//! The session against a stand-in for a Cast device: a TLS server on this
//! machine that speaks the protocol's framing and answers as a receiver
//! does.

use std::net::{Ipv4Addr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use network_output::{Device, Media, NetworkOutput, OpenPcm, OutputState, OutputStatus, Protocol};
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
    /// Set by a test: the next ping is followed by the end-of-song broadcast,
    /// and from then on the media session is gone.
    ends_with: Option<WhenGone>,
    /// Has the broadcast gone out and the media session been dropped.
    gone: Option<WhenGone>,
}

/// What a real receiver may answer about a media session it has dropped.
#[derive(Clone, Copy)]
enum WhenGone {
    EmptyStatus,
    InvalidRequest,
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
                ends_with: None,
                gone: None,
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
                for reply in answer(
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
) -> Vec<(String, String)> {
    let request_id = field(payload, "requestId").unwrap_or_default();
    if let (HEARTBEAT, "PING", Some(when)) = (namespace, kind, player.ends_with.take()) {
        player.state = "IDLE";
        player.idle_reason = Some("FINISHED");
        let broadcast = media_status("0", player);
        player.gone = Some(when);
        return vec![broadcast, (HEARTBEAT.into(), r#"{"type":"PONG"}"#.into())];
    }
    if let (MEDIA, "GET_STATUS", Some(when)) = (namespace, kind, player.gone) {
        return vec![match when {
            WhenGone::EmptyStatus => (
                MEDIA.into(),
                format!(r#"{{"type":"MEDIA_STATUS","requestId":{request_id},"status":[]}}"#),
            ),
            WhenGone::InvalidRequest => (
                MEDIA.into(),
                format!(
                    r#"{{"type":"INVALID_REQUEST","requestId":{request_id},"reason":"INVALID_MEDIA_SESSION_ID"}}"#
                ),
            ),
        }];
    }
    answer_one(namespace, kind, payload, player, &request_id)
        .into_iter()
        .collect()
}

fn answer_one(
    namespace: &str,
    kind: &str,
    payload: &str,
    player: &mut Player,
    request_id: &str,
) -> Option<(String, String)> {
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
            player.gone = None;
            Some(media_status(request_id, player))
        }
        (MEDIA, "PAUSE") => {
            player.state = "PAUSED";
            Some(media_status(request_id, player))
        }
        (MEDIA, "PLAY") => {
            player.state = "PLAYING";
            Some(media_status(request_id, player))
        }
        (MEDIA, "SEEK") => {
            player.position = field(payload, "currentTime")
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0);
            Some(media_status(request_id, player))
        }
        (MEDIA, "STOP") => {
            player.state = "IDLE";
            player.idle_reason = Some("CANCELLED");
            Some(media_status(request_id, player))
        }
        (MEDIA, "GET_STATUS") => Some(media_status(request_id, player)),
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

/// A Cast device fetches the file itself and is never sent audio.
fn no_pcm() -> OpenPcm {
    Arc::new(|_, _| Err("A Cast device is not sent audio.".into()))
}

fn song(start: f64, paused: bool) -> Media {
    Media {
        url: "http://127.0.0.1:1/token".into(),
        path: "unused".into(),
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
    let mut session = fake.device().connect(no_pcm());
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
    let mut session = fake.device().connect(no_pcm());
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
    let mut session = fake.device().connect(no_pcm());
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
    let mut session = fake.device().connect(no_pcm());
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
    .connect(no_pcm());
    let deadline = Instant::now() + Duration::from_secs(10);
    while session.status().is_ok() {
        assert!(
            Instant::now() < deadline,
            "the session never said the device was not there"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A real receiver announces the end of a song once, unasked, and may then
/// drop the media session; the queue must still go on to the next song.
fn a_finished_song_is_seen_once_and_the_next_one_loads(when_gone: WhenGone) {
    let fake = FakeDevice::start();
    let mut session = fake.device().connect(no_pcm());
    session.load(song(0.0, false));
    wait_for(session.as_ref(), |s| {
        s.loads == 1 && s.state == OutputState::Playing
    });

    fake.player.lock().unwrap().ends_with = Some(when_gone);
    wait_for(session.as_ref(), |s| {
        s.loads == 1 && s.state == OutputState::Finished
    });
    // Nothing the player asks for after this may undo or repeat the end.
    std::thread::sleep(Duration::from_millis(2500));
    let status = session.status().unwrap();
    assert_eq!((status.loads, status.state), (1, OutputState::Finished));

    session.load(song(0.0, false));
    let status = wait_for(session.as_ref(), |s| s.loads == 2);
    assert_ne!(status.state, OutputState::Finished, "{status:?}");
    wait_for(session.as_ref(), |s| s.state == OutputState::Playing);
}

#[test]
fn the_end_of_a_song_broadcast_before_the_session_is_dropped_is_not_lost() {
    a_finished_song_is_seen_once_and_the_next_one_loads(WhenGone::EmptyStatus);
}

#[test]
fn a_dropped_session_that_answers_invalid_request_does_not_end_the_session() {
    a_finished_song_is_seen_once_and_the_next_one_loads(WhenGone::InvalidRequest);
}

#[test]
fn a_session_dropped_without_a_broadcast_after_playing_counts_as_the_end() {
    for when_gone in [WhenGone::EmptyStatus, WhenGone::InvalidRequest] {
        let fake = FakeDevice::start();
        let mut session = fake.device().connect(no_pcm());
        session.load(song(0.0, false));
        wait_for(session.as_ref(), |s| s.state == OutputState::Playing);
        fake.player.lock().unwrap().gone = Some(when_gone);
        wait_for(session.as_ref(), |s| s.state == OutputState::Finished);
    }
}

#[test]
fn a_song_stopped_by_someone_else_or_paused_is_not_finished_when_its_session_goes() {
    for (state, idle_reason, wanted) in [
        (
            "IDLE",
            Some("CANCELLED"),
            OutputState::Failed("Something else took over the device.".into()),
        ),
        ("PAUSED", None, OutputState::Paused),
    ] {
        let fake = FakeDevice::start();
        let mut session = fake.device().connect(no_pcm());
        session.load(song(0.0, false));
        wait_for(session.as_ref(), |s| s.state == OutputState::Playing);
        {
            let mut player = fake.player.lock().unwrap();
            player.state = state;
            player.idle_reason = idle_reason;
        }
        wait_for(session.as_ref(), |s| s.state == wanted);
        fake.player.lock().unwrap().gone = Some(WhenGone::EmptyStatus);
        std::thread::sleep(Duration::from_millis(2500));
        assert_ne!(session.status().unwrap().state, OutputState::Finished);
    }
}
