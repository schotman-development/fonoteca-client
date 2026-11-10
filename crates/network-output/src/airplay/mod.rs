//! AirPlay 1, as an AirPort Express, shairport-sync and the speakers that
//! carry it receive audio: unencrypted, as Apple Lossless frames that hold
//! the samples as they are. A receiver can't be told to fetch a file, so the
//! track is decoded here and sent, in real time, over RTP. One thread does
//! all of it in turn: it takes the player's commands, sends the audio that is
//! due, and answers the receiver's timing and retransmit requests.

mod alac;
mod link;
mod record;
mod rtp;
mod rtsp;

pub(crate) use record::{Record, merge, record};

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::device::Device;
use crate::output::{Media, NetworkOutput, OpenPcm, OutputState, OutputStatus, Unavailable};
use link::Link;

/// How long dropping a session waits for the receiver to be told to stop.
const TEARDOWN_WAIT: Duration = Duration::from_secs(2);

enum Command {
    Load(Media),
    Pause(bool),
    Seek(f64),
    Stop,
    /// Lets the receiver go.
    Leave,
}

/// What the thread last published, for [`AirPlaySession::status`].
struct Heard {
    loads: u64,
    state: OutputState,
    position: f64,
    failure: Option<String>,
}

/// Plays on an AirPlay receiver that this session connects to in the
/// background.
pub struct AirPlaySession {
    commands: mpsc::Sender<Command>,
    heard: Arc<Mutex<Heard>>,
    /// Closes when the thread has ended.
    ended: mpsc::Receiver<()>,
}

impl AirPlaySession {
    pub fn connect(device: &Device, pcm: OpenPcm) -> Self {
        let (commands, received) = mpsc::channel();
        let (ending, ended) = mpsc::channel();
        let heard = Arc::new(Mutex::new(Heard {
            loads: 0,
            state: OutputState::Loading,
            position: 0.0,
            failure: None,
        }));
        let (device, shared) = (device.clone(), heard.clone());
        std::thread::Builder::new()
            .name("airplay-session".into())
            .spawn(move || {
                let outcome =
                    Link::open(&device, pcm).and_then(|link| link.serve(&received, &shared));
                if let Err(problem) = outcome {
                    shared.lock().unwrap().failure = Some(problem.to_string());
                }
                drop(ending);
            })
            .expect("a thread for the AirPlay session");
        AirPlaySession {
            commands,
            heard,
            ended,
        }
    }

    fn send(&self, command: Command) {
        // The thread is gone when the receiver is: `status` says so.
        let _ = self.commands.send(command);
    }
}

impl NetworkOutput for AirPlaySession {
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
        match &heard.failure {
            Some(problem) => Err(Unavailable(problem.clone())),
            None => Ok(OutputStatus {
                loads: heard.loads,
                state: heard.state.clone(),
                position: heard.position,
            }),
        }
    }
}

impl Drop for AirPlaySession {
    /// Leaving a receiver streaming when the app ends would leave it playing
    /// what it had buffered, and busy for any other sender.
    fn drop(&mut self) {
        if self.commands.send(Command::Leave).is_err() {
            return;
        }
        if let Err(RecvTimeoutError::Timeout) = self.ended.recv_timeout(TEARDOWN_WAIT) {
            eprintln!("An AirPlay receiver didn't answer to being told to stop.");
        }
    }
}
