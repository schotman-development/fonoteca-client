//! Plays a queue of tracks the way Roon does: each track goes to the sound
//! device in its own sample rate and bit depth where the device takes it,
//! one track runs into the next without a gap, and every step from the file
//! to the device is reported, so the app can show whether what plays is
//! bit-perfect.
//!
//! Tracks arrive as [`Download`]s the app fills from the network while they
//! play. FFmpeg decodes them and its filters do what changes the samples
//! (volume leveling, resampling for a device that can't take the file's
//! rate); cpal plays the result.

mod decode;
mod download;
mod output;

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

pub use download::{Download, Fill, new as download};
pub use output::devices;

use decode::{Decoding, Stop};
use output::{Segment, Shared};

/// The format the device is opened in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u16,
    pub sample: cpal::SampleFormat,
}

impl Format {
    fn frame_bytes(&self) -> usize {
        usize::from(self.channels) * self.sample.sample_size()
    }

    fn bytes_per_second(&self) -> f64 {
        (self.frame_bytes() as f64) * f64::from(self.rate)
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bits = match self.sample {
            cpal::SampleFormat::F32 => "32-bit float".to_owned(),
            sample => format!("{}-bit", sample.bits_per_sample()),
        };
        write!(f, "{} Hz {bits}, {} channels", self.rate, self.channels)
    }
}

/// One step of the signal path, and whether it leaves the audio as it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub text: String,
    pub lossless: bool,
}

/// Which ReplayGain value evens out the volume, if any.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Leveling {
    #[default]
    Off,
    Track,
    Album,
}

/// A track's ReplayGain values, as the server gives them: gains in dB,
/// peaks as linear amplitudes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Gain {
    pub track: Option<f64>,
    pub album: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_peak: Option<f64>,
    /// For a track without the gain asked for.
    pub fallback: Option<f64>,
}

impl Gain {
    /// The gain to apply in dB, never so much that the loudest sample
    /// clips; none when there is nothing to change.
    fn applied(&self, leveling: Leveling) -> Option<f64> {
        let (first, first_peak, second, second_peak) = match leveling {
            Leveling::Off => return None,
            Leveling::Track => (self.track, self.track_peak, self.album, self.album_peak),
            Leveling::Album => (self.album, self.album_peak, self.track, self.track_peak),
        };
        let (gain, peak) = match (first, second) {
            (Some(gain), _) => (gain, first_peak),
            (None, Some(gain)) => (gain, second_peak),
            (None, None) => (self.fallback?, None),
        };
        let gain = match peak {
            Some(peak) if peak > 0.0 => gain.min(-20.0 * peak.log10()),
            _ => gain.min(0.0),
        };
        (gain.abs() >= 0.005).then_some(gain)
    }
}

/// A track to play: the id the app fetches it by, and its gain.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: String,
    pub gain: Gain,
}

/// What is playing.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Status {
    /// The track heard, by its place in the queue.
    pub index: Option<usize>,
    /// Seconds into it.
    pub position: f64,
    pub paused: bool,
    /// The queue has played to its end, or was stopped.
    pub ended: bool,
    /// From the file to the device.
    pub path: Vec<Step>,
    pub bit_perfect: bool,
    pub problem: Option<String>,
}

/// Fetches a track: starts its download and hands back the reading side.
pub type Fetch = Box<dyn FnMut(&Track) -> Result<Download, String> + Send>;

enum Command {
    Play(Vec<Track>, usize),
    Next,
    Previous,
    Seek(f64),
    Leveling(Leveling),
    /// Opens what plays again where it is, for another device.
    Reopen,
    Stop,
    Quit,
}

pub struct Player {
    shared: Arc<Shared>,
    commands: Sender<Command>,
    /// Commands sent and not yet taken, which makes the decoder stop waiting
    /// for a download.
    pending: Arc<AtomicUsize>,
    output: Option<std::thread::JoinHandle<()>>,
}

/// How far ahead of the device the decoder goes, in seconds.
const AHEAD: f64 = 5.0;

/// Before this many seconds into a track, Previous goes to the track
/// before; after, back to its start.
const RESTART_AFTER: f64 = 3.0;

impl Player {
    /// A player on the device with this cpal id, or the system's default,
    /// that gets its tracks from `fetch`.
    pub fn new(device: Option<String>, fetch: Fetch) -> Self {
        let _ = ffmpeg_next::init();
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
        let shared = Arc::new(Shared::default());
        shared.ring.lock().unwrap().wanted = device;
        shared.ring.lock().unwrap().idle = true;
        let (commands, receiver) = channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let output = std::thread::Builder::new()
            .name("audio output".into())
            .spawn({
                let shared = shared.clone();
                move || output::run(shared)
            })
            .expect("a thread for the sound device");
        let decoder = Decoder {
            shared: shared.clone(),
            pending: pending.clone(),
            fetch,
            queue: Vec::new(),
            leveling: Leveling::Off,
            current: None,
            next: None,
        };
        std::thread::Builder::new()
            .name("audio decoder".into())
            .spawn(move || decoder.run(receiver))
            .expect("a thread for the decoder");
        Player {
            shared,
            commands,
            pending,
            output: Some(output),
        }
    }

    fn send(&self, command: Command) {
        self.pending.fetch_add(1, Relaxed);
        let _ = self.commands.send(command);
    }

    /// Plays `queue` from its `start`th track.
    pub fn play(&self, queue: Vec<Track>, start: usize) {
        {
            // Playing from now on, before the decoder gets to it.
            let mut ring = self.shared.ring.lock().unwrap();
            ring.paused = false;
            ring.idle = false;
            ring.segments.clear();
            ring.loading = Some((start, 0.0));
        }
        self.shared.changed.notify_all();
        self.send(Command::Play(queue, start));
    }

    pub fn next(&self) {
        self.send(Command::Next);
    }

    pub fn previous(&self) {
        self.send(Command::Previous);
    }

    pub fn seek(&self, seconds: f64) {
        self.send(Command::Seek(seconds));
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.ring.lock().unwrap().paused = paused;
        self.shared.changed.notify_all();
    }

    pub fn set_leveling(&self, leveling: Leveling) {
        self.send(Command::Leveling(leveling));
    }

    /// Plays on the device with this cpal id, or the system's default.
    pub fn set_device(&self, device: Option<String>) {
        {
            let mut ring = self.shared.ring.lock().unwrap();
            ring.wanted = device;
            ring.generation += 1;
            // The decoder waits for the new one's formats, not the old one's.
            ring.device = None;
            ring.problem = None;
        }
        self.shared.changed.notify_all();
        self.send(Command::Reopen);
    }

    pub fn status(&self) -> Status {
        let ring = self.shared.ring.lock().unwrap();
        let front = ring.segments.front();
        let mut path = front.map(|front| front.path.clone()).unwrap_or_default();
        // Only once the device is open in this track's format; until then it
        // still plays the last one's.
        let open = front.and_then(|front| ring.open.filter(|&open| open == front.format));
        if let (Some(device), Some(open)) = (&ring.device, open) {
            let how = if device.exclusive {
                "Exclusive"
            } else {
                "Shared"
            };
            path.push(Step {
                text: format!("{how}: {} ({open})", device.name),
                lossless: device.exclusive,
            });
        }
        let heard = ring.heard();
        Status {
            index: heard.map(|(index, _)| index),
            position: heard.map_or(0.0, |(_, position)| position),
            paused: ring.paused,
            ended: ring.idle && ring.segments.iter().all(|s| s.data.is_empty()),
            bit_perfect: open.is_some() && path.iter().all(|step| step.lossless),
            path,
            problem: ring.problem.clone(),
        }
    }
}

impl Drop for Player {
    /// Lets the device go before returning, so another player, or another
    /// app, can open it right after.
    fn drop(&mut self) {
        self.send(Command::Quit);
        self.shared.ring.lock().unwrap().quit = true;
        self.shared.changed.notify_all();
        if let Some(output) = self.output.take() {
            let _ = output.join();
        }
    }
}

/// The track being decoded, and another reader of its download, to see when
/// it has all arrived and to open it again.
struct Current {
    index: usize,
    decoding: Decoding,
    download: Download,
}

struct Decoder {
    shared: Arc<Shared>,
    pending: Arc<AtomicUsize>,
    fetch: Fetch,
    queue: Vec<Track>,
    leveling: Leveling,
    current: Option<Current>,
    /// The next track, fetched ahead once the current one has all arrived.
    next: Option<(usize, Result<Download, String>)>,
}

impl Decoder {
    fn run(mut self, commands: Receiver<Command>) {
        loop {
            let command = match self.current {
                Some(_) => commands.try_recv().ok(),
                None => Some(commands.recv().unwrap_or(Command::Quit)),
            };
            if let Some(command) = command {
                self.pending.fetch_sub(1, Relaxed);
                if !self.handle(command) {
                    return;
                }
                continue;
            }
            let mut ring = self.shared.ring.lock().unwrap();
            if ring.halt {
                ring.halt = false;
                ring.idle = true;
                drop(ring);
                self.current = None;
                self.next = None;
                continue;
            }
            if ring.buffered() > AHEAD {
                let _ = self
                    .shared
                    .changed
                    .wait_timeout(ring, Duration::from_millis(50));
                continue;
            }
            drop(ring);
            self.step();
        }
    }

    /// Carries out `command`; false to quit.
    fn handle(&mut self, command: Command) -> bool {
        let heard = self.shared.ring.lock().unwrap().heard();
        match command {
            Command::Quit => return false,
            Command::Play(queue, start) => {
                self.queue = queue;
                self.current = None;
                self.next = None;
                self.start(start, 0.0);
            }
            Command::Next => match heard {
                Some((index, _)) if index + 1 < self.queue.len() => self.start(index + 1, 0.0),
                _ => self.stop(),
            },
            Command::Previous => match heard {
                Some((index, position)) if position > RESTART_AFTER || index == 0 => {
                    self.start(index, 0.0)
                }
                Some((index, _)) => self.start(index - 1, 0.0),
                None => {}
            },
            Command::Seek(seconds) => {
                if let Some((index, _)) = heard {
                    self.start(index, seconds.max(0.0));
                }
            }
            Command::Leveling(leveling) => {
                self.leveling = leveling;
                if let Some((index, position)) = heard {
                    self.start(index, position);
                }
            }
            Command::Reopen => {
                if let Some((index, position)) = heard {
                    self.start(index, position);
                }
            }
            Command::Stop => self.stop(),
        }
        true
    }

    fn stop(&mut self) {
        self.current = None;
        self.next = None;
        let mut ring = self.shared.ring.lock().unwrap();
        ring.segments.clear();
        ring.loading = None;
        ring.idle = true;
        self.shared.changed.notify_all();
    }

    /// Plays the `index`th track from `seconds` into it, in place of
    /// whatever was playing. A track still being decoded opens again from
    /// what has been downloaded, with the leveling and device as they are
    /// now.
    fn start(&mut self, index: usize, seconds: f64) {
        {
            let mut ring = self.shared.ring.lock().unwrap();
            ring.segments.clear();
            ring.problem = None;
            ring.loading = Some((index, seconds));
        }
        let again = self
            .current
            .take()
            .filter(|current| current.index == index)
            .and_then(|current| current.download.reader().ok());
        self.open(index, seconds, again);
    }

    /// Opens the `index`th track, from `download` or else from the network,
    /// and puts it after what is playing.
    fn open(&mut self, index: usize, seconds: f64, download: Option<Download>) {
        let Some(track) = self.queue.get(index).cloned() else {
            let mut ring = self.shared.ring.lock().unwrap();
            ring.idle = true;
            ring.loading = None;
            self.shared.changed.notify_all();
            return;
        };
        let download = match (download, self.next.take()) {
            (Some(download), next) => {
                self.next = next;
                Ok(download)
            }
            (None, Some((next, download))) if next == index => download,
            _ => (self.fetch)(&track),
        };
        let opened = download.map_err(Stop::Failed).and_then(|download| {
            let spare = download
                .reader()
                .map_err(|e| Stop::Failed(format!("The track couldn't be read back: {e}")))?;
            let supported = self.supported()?;
            let pending = self.pending.clone();
            let mut decoding =
                Decoding::open(download, &track, self.leveling, &supported, move || {
                    pending.load(Relaxed) > 0
                })?;
            if seconds > 0.0 {
                decoding.seek(seconds)?;
            }
            Ok((decoding, spare))
        });
        match opened {
            Ok((decoding, download)) => {
                self.push(segment(index, &decoding, seconds));
                self.current = Some(Current {
                    index,
                    decoding,
                    download,
                });
            }
            // A command came; it says what's next.
            Err(Stop::Interrupted) => {}
            Err(Stop::Failed(problem)) => self.skip(index, problem),
        }
    }

    /// Reports why the `index`th track can't play and goes on with the next.
    fn skip(&mut self, index: usize, problem: String) {
        self.shared.ring.lock().unwrap().problem = Some(problem);
        self.current = None;
        self.open(index + 1, 0.0, None);
    }

    /// The formats the device takes, once it has been found.
    fn supported(&self) -> Result<Vec<cpal::SupportedStreamConfigRange>, Stop> {
        let mut ring = self.shared.ring.lock().unwrap();
        loop {
            if let Some(device) = &ring.device {
                return Ok(device.supported.clone());
            }
            if let Some(problem) = &ring.problem {
                return Err(Stop::Failed(problem.clone()));
            }
            if self.pending.load(Relaxed) > 0 {
                return Err(Stop::Interrupted);
            }
            ring = self
                .shared
                .changed
                .wait_timeout(ring, Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }

    fn push(&self, segment: Segment) {
        let mut ring = self.shared.ring.lock().unwrap();
        ring.idle = false;
        ring.loading = None;
        ring.segments.push_back(segment);
        self.shared.changed.notify_all();
    }

    /// Decodes a little more of the current track, and moves on to the next
    /// at its end.
    fn step(&mut self) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        let mut out = Vec::new();
        let result = current.decoding.step(&mut out);
        let index = current.index;
        let finished = !matches!(result, Ok(true) | Err(Stop::Interrupted));
        {
            let mut ring = self.shared.ring.lock().unwrap();
            if let Some(segment) = ring.segments.back_mut().filter(|s| s.track == index) {
                segment.data.extend(out);
                segment.ended |= finished;
            }
            self.shared.changed.notify_all();
        }
        match result {
            Ok(true) => {}
            Ok(false) => {
                self.current = None;
                self.open(index + 1, 0.0, None);
            }
            // A command is waiting and is handled next.
            Err(Stop::Interrupted) => current.decoding.resume(),
            Err(Stop::Failed(problem)) => self.skip(index, problem),
        }
        // The next track starts downloading once this one is in, so a gap
        // in the network doesn't become one in the music.
        let ahead = self
            .current
            .as_ref()
            .filter(|current| self.next.is_none() && current.download.complete())
            .map(|current| current.index + 1)
            .filter(|&next| next < self.queue.len());
        // Tried once: a failure shows when the track's turn comes.
        if let Some(next) = ahead {
            self.next = Some((next, (self.fetch)(&self.queue[next])));
        }
    }
}

fn segment(track: usize, decoding: &Decoding, seconds: f64) -> Segment {
    Segment {
        track,
        format: decoding.format,
        data: VecDeque::new(),
        ended: false,
        start: (seconds * f64::from(decoding.format.rate)) as u64,
        played: 0,
        path: decoding.path.clone(),
    }
}

#[cfg(test)]
mod tests;
