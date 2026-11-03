//! Plays a queue of tracks the way Roon does: each track goes to the sound
//! device in its own sample rate and bit depth where the device takes it,
//! one track runs into the next without a gap, and every step from the file
//! to the device is reported, so the app can show whether what plays is
//! bit-perfect.
//!
//! Tracks arrive as [`Download`]s the app fills from the network while they
//! play. FFmpeg decodes them and its filters do what changes the samples
//! (volume leveling, the listener's [`Dsp`], resampling for a device that
//! can't take the file's rate); cpal plays the result.

mod decode;
mod download;
mod dsp;
mod output;
mod outputs;

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

pub use cpal::SampleFormat;
pub use download::{Download, Fill, new as download};
pub use dsp::{Band, Crossfeed, Dsp, Equalizer, Filter};
pub use output::{SystemMixer, devices};
pub use outputs::OutputDevice;

use decode::{Decoding, Stop};
use output::{Route, Segment, Shared};

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

/// One step of the signal path, and what it does to the audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub text: String,
    pub quality: Quality,
}

/// What a step does to the audio, graded as Roon grades its signal path,
/// worst first; a path is as good as its worst step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    /// A lossy file.
    Low,
    /// Changed to suit the device: resampled, fewer bits, other channels,
    /// or mixed by the system.
    High,
    /// Changed because the listener asked: leveling and sound processing.
    Enhanced,
    /// Left as it was.
    Lossless,
}

/// What plays once a track ends.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// The next track, until the queue ends.
    #[default]
    Off,
    /// The next track, and the first after the last.
    All,
    /// The same track again.
    One,
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
    /// The worst step's, once the device is open in the track's format.
    pub quality: Option<Quality>,
    pub problem: Option<String>,
    /// A track that couldn't play, by its place in the queue, and why; while
    /// the one played in its place is heard.
    pub skipped: Option<(usize, String)>,
    /// Where the queue stopped short of its end, as a device that can't play
    /// or tracks that can't play up to its end leave it: the track to go on
    /// with, and how far into it.
    pub stopped: Option<(usize, f64)>,
    /// How many of the queue's changes the player has made. Until it has
    /// made all that were asked, `index` counts in the queue as it was.
    pub requeues: u64,
}

/// Fetches a track from the given seconds into it: starts its download and
/// hands back the reading side. A source that begins a track anywhere marks
/// the download [`Download::starting_at`] there; any other hands back the
/// whole file, which the player reads on to that point.
pub type Fetch = Box<dyn FnMut(&Track, f64) -> Result<Download, String> + Send>;

enum Command {
    Play(Vec<Track>, usize, f64),
    /// A changed queue, and where each track of the one before went.
    Requeue(Vec<Track>, Vec<Option<usize>>),
    Jump(usize),
    Next,
    Previous,
    Seek(f64),
    Repeat(Repeat),
    Leveling(Leveling),
    Dsp(Dsp),
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
        Self::build(device, fetch, None)
    }

    /// Like [`Player::new`], on a system whose mixer `system` can be asked
    /// about and told to pass audio on untouched.
    pub fn with_system(device: Option<String>, fetch: Fetch, system: Arc<dyn SystemMixer>) -> Self {
        Self::build(device, fetch, Some(system))
    }

    fn build(device: Option<String>, fetch: Fetch, system: Option<Arc<dyn SystemMixer>>) -> Self {
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
                let system = system.clone();
                move || output::run(shared, system)
            })
            .expect("a thread for the sound device");
        let decoder = Decoder {
            shared: shared.clone(),
            pending: pending.clone(),
            fetch,
            system,
            queue: Vec::new(),
            leveling: Leveling::Off,
            dsp: Dsp::default(),
            repeat: Repeat::Off,
            current: None,
            next: None,
            failures: 0,
            failed_from: (0, 0.0),
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

    /// Plays `queue` from `seconds` into its `start`th track.
    pub fn play(&self, queue: Vec<Track>, start: usize, seconds: f64) {
        {
            // Playing from now on, before the decoder gets to it.
            let mut ring = self.shared.ring.lock().unwrap();
            ring.paused = false;
            ring.idle = false;
            ring.segments.clear();
            ring.loading = Some((start, seconds));
        }
        self.shared.changed.notify_all();
        self.send(Command::Play(queue, start, seconds));
    }

    /// Puts `queue` in place of the one playing, where `moved` gives each
    /// track of the one before its place in `queue`, or none if it was taken
    /// out. What plays goes on, followed by what follows it in `queue`.
    pub fn requeue(&self, queue: Vec<Track>, moved: Vec<Option<usize>>) {
        self.send(Command::Requeue(queue, moved));
    }

    /// Plays the queue's `index`th track from its start.
    pub fn jump(&self, index: usize) {
        self.shared.ring.lock().unwrap().paused = false;
        self.shared.changed.notify_all();
        self.send(Command::Jump(index));
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

    pub fn set_repeat(&self, repeat: Repeat) {
        self.send(Command::Repeat(repeat));
    }

    /// Has what plays go through `dsp` from where it is.
    pub fn set_dsp(&self, dsp: Dsp) {
        self.send(Command::Dsp(dsp));
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
            path.extend(device_steps(device, ring.route, open));
        }
        let quality = open.and_then(|_| path.iter().map(|step| step.quality).min());
        let heard = ring.heard();
        Status {
            index: heard.map(|(index, _)| index),
            position: heard.map_or(0.0, |(_, position)| position),
            paused: ring.paused,
            ended: ring.idle && ring.segments.iter().all(|s| s.data.is_empty()),
            quality,
            path,
            problem: ring.problem.clone(),
            skipped: ring.shown_skip().cloned(),
            stopped: ring.stopped,
            requeues: ring.requeues,
        }
    }
}

/// The steps from the audio, as the device is opened in `open`, to the
/// sound: the device, and the system's mixer if it takes it first.
fn device_steps(device: &output::Device, route: Route, open: Format) -> Vec<Step> {
    let (how, quality) = if route == Route::BitPerfect {
        ("Bit-perfect", Quality::Lossless)
    } else if device.exclusive {
        ("Exclusive", Quality::Lossless)
    } else {
        ("Shared", Quality::High)
    };
    let mut steps = vec![Step {
        text: format!("{how}: {} ({open})", device.name),
        quality,
    }];
    if route == Route::Mixed {
        // Which rate it runs the output at isn't something Android says.
        steps.push(Step {
            text: "Android's mixer handles the stream and may change its rate".into(),
            quality: Quality::High,
        });
    }
    steps
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
    system: Option<Arc<dyn SystemMixer>>,
    queue: Vec<Track>,
    leveling: Leveling,
    dsp: Dsp,
    repeat: Repeat,
    current: Option<Current>,
    /// The next track, fetched ahead once the current one has all arrived.
    next: Option<(usize, Result<Download, String>)>,
    /// Tracks that couldn't play, one after the other, and the first of them
    /// with how far into it.
    failures: usize,
    failed_from: (usize, f64),
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
            Command::Play(queue, start, seconds) => {
                self.queue = queue;
                self.current = None;
                self.next = None;
                self.start(start, seconds);
            }
            Command::Requeue(queue, moved) => self.requeue(queue, &moved),
            Command::Jump(index) => self.start(index, 0.0),
            Command::Next => match heard.and_then(|(index, _)| self.following(index)) {
                Some(next) => self.start(next, 0.0),
                None => self.stop(),
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
            Command::Repeat(repeat) => {
                self.repeat = repeat;
                self.rechain();
            }
            Command::Leveling(leveling) => {
                self.leveling = leveling;
                if let Some((index, position)) = heard {
                    self.start(index, position);
                }
            }
            // Typing into the settings sends what changes nothing too.
            Command::Dsp(dsp) if dsp == self.dsp => {}
            Command::Dsp(dsp) => {
                self.dsp = dsp;
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

    /// The track after the `index`th, as Next goes: the queue's next, or its
    /// first after its last when all of it repeats.
    fn following(&self, index: usize) -> Option<usize> {
        if index + 1 < self.queue.len() {
            Some(index + 1)
        } else {
            (self.repeat == Repeat::All && !self.queue.is_empty()).then_some(0)
        }
    }

    /// The track that plays once the `index`th ends.
    fn after(&self, index: usize) -> Option<usize> {
        match self.repeat {
            Repeat::One => Some(index),
            _ => self.following(index),
        }
    }

    /// Takes up `queue` in place of the one before, where `moved` gives each
    /// track of that one its place in `queue`.
    fn requeue(&mut self, queue: Vec<Track>, moved: &[Option<usize>]) {
        let place = |index: usize| moved.get(index).copied().flatten();
        self.queue = queue;
        self.current = self.current.take().and_then(|mut current| {
            current.index = place(current.index)?;
            Some(current)
        });
        self.next = self
            .next
            .take()
            .and_then(|(index, download)| Some((place(index)?, download)));
        let heard_gone = {
            let mut ring = self.shared.ring.lock().unwrap();
            ring.requeues += 1;
            ring.loading = ring
                .loading
                .and_then(|(index, seconds)| Some((place(index)?, seconds)));
            ring.skipped = ring
                .skipped
                .take()
                .and_then(|(index, why)| Some((place(index)?, why)));
            ring.stopped = ring
                .stopped
                .and_then(|(index, seconds)| Some((place(index)?, seconds)));
            let heard_gone = ring
                .segments
                .front()
                .map(|front| front.track)
                .filter(|&track| place(track).is_none());
            for segment in &mut ring.segments {
                segment.track = place(segment.track).unwrap_or(usize::MAX);
            }
            heard_gone
        };
        // The track heard was taken out as the app asked; the first after it
        // that stayed plays in its place.
        if let Some(heard) = heard_gone {
            match (heard..moved.len()).find_map(place) {
                Some(index) => self.start(index, 0.0),
                None => self.stop(),
            }
            return;
        }
        self.rechain();
    }

    /// Keeps what has been decoded as far as it follows on in the queue, and
    /// goes on from there.
    fn rechain(&mut self) {
        let (last, last_ended, loading) = {
            let mut ring = self.shared.ring.lock().unwrap();
            let mut kept = 0;
            let mut last: Option<usize> = None;
            for segment in &ring.segments {
                let follows = match last {
                    None => segment.track < self.queue.len(),
                    Some(last) => self.after(last) == Some(segment.track),
                };
                if !follows {
                    break;
                }
                last = Some(segment.track);
                kept += 1;
            }
            ring.segments.truncate(kept);
            let last_ended = ring.segments.back().is_some_and(|back| back.ended);
            (last, last_ended, ring.loading)
        };
        match last {
            // The one kept last is still being decoded, so what follows it
            // comes as it ends.
            Some(_) if !last_ended => {}
            Some(last) => {
                self.current = None;
                let next = self.after(last).unwrap_or(self.queue.len());
                self.open(next, 0.0, None);
            }
            // Nothing decoded yet, as when a command broke into opening it.
            None => {
                if let Some((index, seconds)) =
                    loading.filter(|(index, _)| *index < self.queue.len())
                {
                    self.start(index, seconds);
                }
            }
        }
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
    /// what has been downloaded, with the leveling, processing and device as
    /// they are now; one whose download can't play from `seconds` is
    /// fetched again from there.
    fn start(&mut self, index: usize, seconds: f64) {
        // A track asked for counts the failures afresh.
        self.failures = 0;
        {
            let mut ring = self.shared.ring.lock().unwrap();
            ring.segments.clear();
            ring.problem = None;
            ring.skipped = None;
            ring.stopped = None;
            ring.loading = Some((index, seconds));
        }
        let again = self
            .current
            .take()
            .filter(|current| current.index == index && plays_from(&current.download, seconds))
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
        // Without a device nothing plays, so nothing is fetched; the
        // problem it found says why.
        let supported = match self.supported() {
            Ok(supported) => supported,
            Err(Stop::Interrupted) => return,
            Err(Stop::Failed(_)) => {
                self.shared.ring.lock().unwrap().stopped = Some((index, seconds));
                self.stop();
                return;
            }
        };
        let bit_perfect_formats = self
            .system
            .as_ref()
            .map(|system| system.bit_perfect_formats())
            .unwrap_or_default();
        let download = match (download, self.next.take()) {
            (Some(download), next) => {
                self.next = next;
                Ok(download)
            }
            (None, Some((next, download)))
                if next == index
                    && download
                        .as_ref()
                        .map_or(true, |fetched| plays_from(fetched, seconds)) =>
            {
                download
            }
            (None, next) => {
                // A track fetched again from elsewhere in it is still
                // followed by the one fetched ahead.
                self.next = next.filter(|(next, _)| self.after(index) == Some(*next));
                (self.fetch)(&track, seconds)
            }
        };
        let opened = download.map_err(Stop::Failed).and_then(|download| {
            let into_download = seconds - download.starts_at().unwrap_or(0.0);
            let spare = download
                .reader()
                .map_err(|e| Stop::Failed(format!("The track couldn't be read back: {e}")))?;
            let pending = self.pending.clone();
            let mut decoding = Decoding::open(
                download,
                &track,
                self.leveling,
                &self.dsp,
                &supported,
                &bit_perfect_formats,
                move || pending.load(Relaxed) > 0,
            )?;
            if into_download > 0.0 {
                decoding.seek(into_download)?;
            }
            Ok((decoding, spare))
        });
        match opened {
            Ok((decoding, download)) => {
                self.failures = 0;
                self.push(segment(index, &decoding, seconds));
                self.current = Some(Current {
                    index,
                    decoding,
                    download,
                });
            }
            // A command came; it says what's next.
            Err(Stop::Interrupted) => {}
            Err(Stop::Failed(problem)) => self.skip(index, seconds, problem),
        }
    }

    /// Reports why the `index`th track can't play from `seconds` into it and
    /// goes on with the next. With none left, the queue stopped short at the
    /// first of the tracks that failed one after the other.
    fn skip(&mut self, index: usize, seconds: f64, problem: String) {
        if self.failures == 0 {
            self.failed_from = (index, seconds);
        }
        self.shared.ring.lock().unwrap().skipped = Some((index, problem));
        self.current = None;
        self.failures += 1;
        // Round a repeating queue whose every track fails, it would go on
        // for good.
        let next = self
            .following(index)
            .filter(|_| self.failures < self.queue.len());
        if next.is_none() {
            self.shared.ring.lock().unwrap().stopped = Some(self.failed_from);
        }
        self.open(next.unwrap_or(self.queue.len()), 0.0, None);
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
        // How far into the track its audio reaches.
        let reached = {
            let mut ring = self.shared.ring.lock().unwrap();
            let reached = ring
                .segments
                .back_mut()
                .filter(|s| s.track == index)
                .map(|segment| {
                    segment.data.extend(out);
                    segment.ended |= finished;
                    (segment.start + segment.played) as f64 / f64::from(segment.format.rate)
                        + segment.data.len() as f64 / segment.format.bytes_per_second()
                });
            self.shared.changed.notify_all();
            reached
        };
        match result {
            Ok(true) => {}
            Ok(false) => {
                let next = self.after(index);
                // A track played again is read again from what has arrived.
                let again = self
                    .current
                    .take()
                    .filter(|current| next == Some(index) && plays_from(&current.download, 0.0))
                    .and_then(|current| current.download.reader().ok());
                self.open(next.unwrap_or(self.queue.len()), 0.0, again);
            }
            // A command is waiting and is handled next.
            Err(Stop::Interrupted) => current.decoding.resume(),
            Err(Stop::Failed(problem)) => self.skip(index, reached.unwrap_or(0.0), problem),
        }
        // The next track starts downloading once this one is in, so a gap
        // in the network doesn't become one in the music. One played again
        // needs none.
        let ahead = self
            .current
            .as_ref()
            .filter(|current| current.download.complete())
            .and_then(|current| {
                self.after(current.index)
                    .filter(|&next| next != current.index)
            })
            .filter(|&next| {
                self.next
                    .as_ref()
                    .is_none_or(|(fetched, _)| *fetched != next)
            });
        // Tried once: a failure shows when the track's turn comes.
        if let Some(next) = ahead {
            self.next = Some((next, (self.fetch)(&self.queue[next], 0.0)));
        }
    }
}

/// Whether `download` plays its track from `seconds`: a whole file does, read
/// on to there. One whose source began it part way does from where it began,
/// and once all of it has arrived from any later point too: a seek in it then
/// leaves no gap, where fetching it again from there leaves one of 50 to
/// 100 ms.
fn plays_from(download: &Download, seconds: f64) -> bool {
    download
        .starts_at()
        .is_none_or(|start| start == seconds || (start < seconds && download.complete()))
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
