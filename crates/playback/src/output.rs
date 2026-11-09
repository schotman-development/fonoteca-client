//! The sound device: the decoded audio waiting for it, and the thread that
//! opens it in the format of whatever plays next.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    DeviceDescriptionBuilder, ErrorKind, SampleFormat, SupportedBufferSize,
    SupportedStreamConfigRange,
};

use crate::outputs::{self, Found, OutputDevice};
#[cfg(target_os = "linux")]
use crate::pipewire_default::default_sink;
use crate::{Format, Step, switches};

/// One track's decoded audio, in the device's format.
pub(crate) struct Segment {
    /// The track's place in the queue.
    pub track: usize,
    pub format: Format,
    pub data: VecDeque<u8>,
    /// The decoder has put all of it in.
    pub ended: bool,
    /// Where in the track `data` starts, and how much the device has played
    /// since, in frames.
    pub start: u64,
    pub played: u64,
    /// The steps from the file to `format`.
    pub path: Vec<Step>,
}

/// What the operating system can do about how it mixes: Android's USB output
/// can be told to pass a format on untouched, and otherwise resamples what it
/// is given to the rate it runs the output at.
pub trait SystemMixer: Send + Sync {
    /// The formats the output passes on untouched, none where it can't.
    fn bit_perfect_formats(&self) -> Vec<Format>;
    /// Has the output pass `format` on untouched, until released or asked
    /// again; false if it won't.
    fn engage_bit_perfect(&self, format: Format) -> bool;
    /// Gives the output back to the system's mixer.
    fn release_bit_perfect(&self);
}

/// How the system carries what the device is given.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// The device is used as it is: no system mixer to ask about.
    #[default]
    Direct,
    /// The system passes it on untouched.
    BitPerfect,
    /// The system's mixer takes it, and may change its rate.
    Mixed,
}

/// What the device in use is.
#[derive(Clone, Default)]
pub(crate) struct Device {
    pub name: String,
    /// It is the hardware itself, so nothing else mixes in or converts.
    pub exclusive: bool,
    pub supported: Vec<SupportedStreamConfigRange>,
}

#[derive(Default)]
pub(crate) struct Ring {
    pub segments: VecDeque<Segment>,
    pub paused: bool,
    /// The device asked for, by cpal id, or the system's default; once found,
    /// outside Android, the one found in its place if it isn't there. And the
    /// one open, once it is.
    pub wanted: Option<String>,
    pub device: Option<Device>,
    /// The format the device is open in.
    pub open: Option<Format>,
    /// How the system carries what is played in `open`.
    pub route: Route,
    pub problem: Option<String>,
    /// The device went away, or the system moved the stream to another,
    /// since it was found: nothing plays or opens on it until it is found
    /// again.
    pub device_lost: bool,
    /// The last track that couldn't play, by its place in the queue, and
    /// why.
    pub skipped: Option<(usize, String)>,
    /// Nothing more is coming, so the device can be let go once it has
    /// played what is here.
    pub idle: bool,
    /// The device couldn't play what came, so the decoder stops.
    pub halt: bool,
    /// Where the queue stopped short of its end: the track it couldn't go on
    /// with, and how far into it.
    pub stopped: Option<(usize, f64)>,
    /// The track and time being opened, until its audio comes.
    pub loading: Option<(usize, f64)>,
    /// When the last audio handed to the device will have been heard.
    pub sounding_until: Option<Instant>,
    /// When the stream open last asked for sound; none until it first does.
    pub asked_at: Option<Instant>,
    /// How many streams were let go of: a stream's errors are its own only
    /// until it is let go too.
    pub streams_let_go: u64,
    pub quit: bool,
    /// Bumped to make the output thread find the device again.
    pub generation: u64,
    /// How many of the queue's changes the decoder has made.
    pub requeues: u64,
}

impl Ring {
    /// How much audio is waiting, in seconds.
    pub fn buffered(&self) -> f64 {
        self.segments
            .iter()
            .map(|segment| segment.data.len() as f64 / segment.format.bytes_per_second())
            .sum()
    }

    /// The track heard, by its place in the queue, and how far into it, in
    /// seconds; or the one being opened.
    pub fn heard(&self) -> Option<(usize, f64)> {
        match self.segments.front() {
            Some(front) => {
                let frames = front.start + front.played;
                Some((front.track, frames as f64 / f64::from(front.format.rate)))
            }
            None => self.loading,
        }
    }

    /// The track that couldn't play and why, while the one played in its
    /// place is heard, or nothing is.
    pub fn shown_skip(&self) -> Option<&(usize, String)> {
        let heard = self.heard().map(|(index, _)| index);
        self.skipped
            .as_ref()
            .filter(|(track, _)| heard.is_none_or(|heard| heard <= track + 1))
    }

    /// The format the device should be open in: that of the first audio
    /// still to play, none once everything has played and nothing more is
    /// coming, and `open` while waiting for more or for the lost device.
    fn wanted(&self, open: Option<Format>) -> Option<Format> {
        if self.device_lost {
            return open;
        }
        let next = self
            .segments
            .iter()
            .find(|segment| !segment.data.is_empty() || !segment.ended);
        match next {
            Some(segment) => Some(segment.format),
            None if self.idle => None,
            None => open,
        }
    }

    /// Plays from now on. A device lost is found again first: true then, as
    /// what plays has to be opened again for it.
    pub fn unpause(&mut self) -> bool {
        self.paused = false;
        if self.device_lost {
            self.find_device_again();
        }
        self.device_lost
    }

    /// Has the output thread find the device wanted, or the system's
    /// default, anew.
    pub fn find_device_again(&mut self) {
        self.generation += 1;
        // The decoder waits for the new one's formats, not the old one's.
        self.device = None;
        self.problem = None;
        // Whatever the old stream says until it is closed is about the old.
        self.streams_let_go += 1;
    }

    /// Whether the stream open has stopped asking for sound, as one does
    /// whose output went without its host saying so.
    pub fn went_quiet(&self, now: Instant) -> bool {
        !self.device_lost
            && self
                .asked_at
                .is_some_and(|at| now.saturating_duration_since(at) > QUIET)
    }

    /// What an error of the stream opened after `let_go` streams were let
    /// go of does: nothing once it is let go of itself, as PipeWire's says its
    /// output went as it closes.
    pub fn stream_said(&mut self, let_go: u64, error: &cpal::Error) {
        if self.streams_let_go == let_go {
            self.stream_failed(error);
        }
    }

    /// What an error of the open stream does to what plays.
    pub fn stream_failed(&mut self, error: &cpal::Error) {
        match error.kind() {
            // A skip or no realtime priority leaves it playing.
            ErrorKind::Xrun | ErrorKind::RealtimeDenied => {}
            // Gone, or moved by the system to another device, often the
            // speaker: it stops as a press of pause stops it, rather than
            // play on there.
            ErrorKind::DeviceNotAvailable | ErrorKind::DeviceChanged => {
                self.paused = true;
                self.device_lost = true;
                let name = self
                    .device
                    .as_ref()
                    .map_or("The sound device", |device| device.name.as_str());
                self.problem = Some(format!("{name} is unavailable."));
            }
            _ => self.problem = Some(format!("The sound device failed: {error}")),
        }
    }
}

#[derive(Default)]
pub(crate) struct Shared {
    pub ring: Mutex<Ring>,
    pub changed: Condvar,
}

/// How much of a track, in seconds, is decoded before it starts.
const START: f64 = 0.5;
/// How long past the device's own estimate it is kept open before closing.
const SETTLE: Duration = Duration::from_millis(200);
/// How long a stream goes without asking for sound before its output is
/// taken as gone, where a device asks every tenth of a second or so.
const QUIET: Duration = Duration::from_secs(2);

/// Fills the device's buffer from the segments in front: silence while
/// paused or the device is lost, waiting for data, or when the next segment
/// needs the device in another format, which the output thread then opens.
/// `latency` is how long the device takes to play what it is handed.
fn fill(shared: &Shared, format: Format, out: &mut [u8], latency: Duration) {
    let mut ring = shared.ring.lock().unwrap();
    let now = Instant::now();
    ring.asked_at = Some(now);
    let mut filled = 0;
    let frame = format.frame_bytes();
    while !ring.paused && !ring.device_lost && filled < out.len() {
        // A track played to its end gives way to the next, whatever its
        // format; the last one stays, so its position can still be read.
        while ring.segments.len() > 1 && ring.segments[0].ended && ring.segments[0].data.is_empty()
        {
            ring.segments.pop_front();
        }
        let Some(front) = ring.segments.front_mut() else {
            break;
        };
        if front.format != format {
            break;
        }
        // A track starts once more of it is in than the device takes in a
        // row as it starts, or all of it, so it can't run dry right after.
        let lead = ((format.bytes_per_second() * START) as usize).max(out.len() - filled);
        if front.played == 0 && !front.ended && front.data.len() < lead {
            break;
        }
        let take = (out.len() - filled).min(front.data.len()) / frame * frame;
        for (to, byte) in out[filled..filled + take]
            .iter_mut()
            .zip(front.data.drain(..take))
        {
            *to = byte;
        }
        filled += take;
        front.played += (take / frame) as u64;
        if take == 0 {
            break;
        }
    }
    out[filled..].fill(0);
    if filled > 0 {
        let frames = (filled / frame) as f64 / f64::from(format.rate);
        ring.sounding_until = Some(now + latency + Duration::from_secs_f64(frames));
    }
    shared.changed.notify_all();
}

/// Runs the device until `quit`: finds the device wanted, opens it in the
/// format of the audio in front and reopens it when that changes, and lets
/// it go for other apps once everything has played.
pub(crate) fn run(shared: Arc<Shared>, system: Option<Arc<dyn SystemMixer>>) {
    let system = system.as_deref();
    let mut generation = None;
    let mut device = None;
    let mut stream: Option<(cpal::Stream, Format)> = None;
    loop {
        let mut ring = shared.ring.lock().unwrap();
        let open = stream.as_ref().map(|(_, format)| *format);
        ring = shared
            .changed
            .wait_while(ring, |ring| {
                !ring.quit && generation == Some(ring.generation) && ring.wanted(open) == open
            })
            .unwrap();
        if ring.quit {
            drop(ring);
            drop(stream.take());
            release(system);
            return;
        }
        if generation != Some(ring.generation) {
            generation = Some(ring.generation);
            let wanted = ring.wanted.clone();
            ring.open = None;
            drop(ring);
            close(&shared, &mut stream);
            device = find(wanted.as_deref());
            let found = device
                .as_ref()
                .and_then(|device| device.id().ok())
                .map(|id| id.to_string());
            let mut ring = shared.ring.lock().unwrap();
            // One that takes no format at all, such as ALSA's default on a
            // machine without a sound card, is none.
            ring.device = device
                .as_ref()
                .map(describe)
                .filter(|device| !device.supported.is_empty());
            match ring.device {
                Some(_) => ring.device_lost = false,
                None => ring.problem = Some("There is no sound device to play on.".into()),
            }
            // Unless another was asked for meanwhile, which is found next.
            let usable = device.as_ref().filter(|_| ring.device.is_some());
            if let Some(device) = usable.filter(|_| generation == Some(ring.generation)) {
                if found.is_some() && !cfg!(target_os = "android") {
                    ring.wanted = found;
                }
                switches::watch(shared.clone(), ring.generation, device);
            }
            shared.changed.notify_all();
            continue;
        }
        let wanted = ring.wanted(open);
        let wanted_device = ring.wanted.clone();
        let supported = ring
            .device
            .as_ref()
            .map(|device| device.supported.clone())
            .unwrap_or_default();
        // Its own count of what it holds is approximate; a little longer
        // costs nothing, and too short cuts off the end of the last track.
        let sounding = ring
            .sounding_until
            .map(|until| (until + SETTLE).saturating_duration_since(Instant::now()));
        drop(ring);
        // The end of the last track is still in the device's buffer; closing
        // now would cut it off.
        if let Some(sounding) = sounding {
            std::thread::sleep(sounding);
        }
        // The old stream goes first, since a device opened directly can only
        // be open once.
        close(&shared, &mut stream);
        let opened = open_output(
            system,
            wanted_device.as_deref(),
            wanted,
            device.as_ref(),
            &supported,
            &shared,
        );
        let mut ring = shared.ring.lock().unwrap();
        match opened {
            Ok((opened, route)) => {
                ring.open = opened.as_ref().map(|(_, format)| *format);
                ring.route = route;
                stream = opened;
            }
            Err(problem) => {
                ring.problem = Some(problem);
                ring.open = None;
                ring.stopped = ring.heard().or(ring.stopped);
                ring.segments.clear();
                ring.halt = true;
            }
        }
        shared.changed.notify_all();
    }
}

/// Closes the stream, never under the lock: closing waits for the device's
/// thread, which may be waiting for the lock in `fill`.
fn close(shared: &Shared, stream: &mut Option<(cpal::Stream, Format)>) {
    shared.ring.lock().unwrap().streams_let_go += 1;
    drop(stream.take());
    shared.ring.lock().unwrap().asked_at = None;
}

/// Opens `format` on `device`, or nothing for none, and says how the system
/// carries it. Gives the output back to the system's mixer if it can't.
fn open_output(
    system: Option<&dyn SystemMixer>,
    wanted_device: Option<&str>,
    format: Option<Format>,
    device: Option<&cpal::Device>,
    supported: &[SupportedStreamConfigRange],
    shared: &Arc<Shared>,
) -> Result<(Option<(cpal::Stream, Format)>, Route), String> {
    // After the old stream is gone, before the new one opens: the system
    // reads its mixer settings as a stream opens. Only its own output can be
    // told to pass audio on.
    let route = route(system, format, wanted_device.is_none());
    let opened = match (format, device) {
        (None, _) => Ok(None),
        (Some(_), None) => Err("There is no sound device to play on.".to_owned()),
        (Some(format), Some(device)) => open_stream(device, format, supported, shared.clone())
            .map(|stream| Some((stream, format))),
    };
    opened
        .map(|opened| (opened, route))
        .inspect_err(|_| release(system))
}

/// Asks the system to pass `format` on untouched where `allowed`, or else
/// leaves it to the system's mixer; with no format, gives the output back.
fn route(system: Option<&dyn SystemMixer>, format: Option<Format>, allowed: bool) -> Route {
    let Some(system) = system else {
        return Route::Direct;
    };
    match format {
        Some(format) if allowed && system.engage_bit_perfect(format) => Route::BitPerfect,
        Some(_) => {
            system.release_bit_perfect();
            Route::Mixed
        }
        None => {
            system.release_bit_perfect();
            Route::Direct
        }
    }
}

fn release(system: Option<&dyn SystemMixer>) {
    if let Some(system) = system {
        system.release_bit_perfect();
    }
}

/// The device `wanted`, by cpal id. Android routes the sound itself, so
/// without one it is Android's default; elsewhere an output that isn't
/// listed now gives way to the system's own, found by its id so the system
/// can't move what plays from it.
fn find(wanted: Option<&str>) -> Option<cpal::Device> {
    if cfg!(target_os = "android") {
        return wanted
            .and_then(by_id)
            .or_else(|| cpal::default_host().default_output_device());
    }
    let listed = devices();
    let known = wanted.filter(|wanted| listed.iter().any(|output| output.id == *wanted));
    if let Some(known) = known {
        return by_id(known);
    }
    let system = system_output(&listed)?;
    // ALSA lists its default by name only where a sound server's plugin
    // gives it a hint; without one, the default is still there to play on.
    by_id(&system).or_else(|| {
        cpal::default_host()
            .default_output_device()
            .filter(|device| device.id().is_ok_and(|id| id.to_string() == system))
    })
}

fn by_id(id: &str) -> Option<cpal::Device> {
    let id: cpal::DeviceId = id.parse().ok()?;
    cpal::host_from_id(id.host()).ok()?.device_by_id(&id)
}

/// The id of the output the system plays on now. PipeWire's stand-in for it
/// doesn't say which output that is, so PipeWire is asked.
pub fn system_output(listed: &[OutputDevice]) -> Option<String> {
    let host = cpal::default_host();
    let id = host.default_output_device()?.id().ok()?.to_string();
    if !outputs::is_stand_in(&id) {
        return Some(id);
    }
    outputs::behind_the_stand_in(default_sink(&host).as_deref(), listed)
}

/// Only Linux has PipeWire.
#[cfg(not(target_os = "linux"))]
fn default_sink(_pipewire: &cpal::Host) -> Option<String> {
    None
}

fn describe(device: &cpal::Device) -> Device {
    let id = device.id().map(|id| id.to_string()).unwrap_or_default();
    Device {
        name: listed_name(device),
        exclusive: outputs::exclusive(&id),
        supported: device
            .supported_output_configs()
            .map(Iterator::collect)
            .unwrap_or_default(),
    }
}

/// The name the menu of devices gives the device, so that the signal path
/// and its problems call it the same; the system's for one it doesn't list.
fn listed_name(device: &cpal::Device) -> String {
    found(device)
        .and_then(|found| outputs::list(vec![found]).pop())
        .map_or_else(|| name(device), |output| output.name)
}

/// The device's name as the system gives it, without ALSA's second line or
/// the kind cpal prints after it.
fn name(device: &cpal::Device) -> String {
    let name = device
        .description()
        .map(|description| description.name().to_owned())
        .unwrap_or_else(|_| device.to_string());
    name.lines().next().unwrap_or_default().to_owned()
}

/// The devices to choose from, each output once.
pub fn devices() -> Vec<OutputDevice> {
    let found = hosts()
        .iter()
        .filter_map(|host| host.output_devices().ok())
        .flatten()
        .filter_map(|device| found(&device))
        .collect();
    outputs::list(found)
}

/// Linux has PipeWire's outputs where it runs, and ALSA's, which reach the
/// hardware around it; elsewhere there is the system's own host.
fn hosts() -> Vec<cpal::Host> {
    #[cfg(target_os = "linux")]
    return [cpal::HostId::PipeWire, cpal::HostId::Alsa]
        .into_iter()
        .filter_map(|id| cpal::host_from_id(id).ok())
        .collect();
    #[cfg(not(target_os = "linux"))]
    vec![cpal::default_host()]
}

fn found(device: &cpal::Device) -> Option<Found> {
    Some(Found {
        id: device.id().ok()?.to_string(),
        description: device
            .description()
            .unwrap_or_else(|_| DeviceDescriptionBuilder::new(name(device)).build()),
        safe_speaker: format!("{device:?}").contains("BuiltinSpeakerSafe"),
    })
}

fn open_stream(
    device: &cpal::Device,
    format: Format,
    supported: &[SupportedStreamConfigRange],
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String> {
    // A tenth of a second ahead, in two halves, where the device allows it:
    // nobody hears the delay, and a busy system doesn't make the music skip.
    let half = supported
        .iter()
        .find(|range| {
            range.channels() == format.channels
                && range.sample_format() == format.sample
                && (range.min_sample_rate()..=range.max_sample_rate()).contains(&format.rate)
        })
        .and_then(|range| match *range.buffer_size() {
            SupportedBufferSize::Range { min, max } => Some((format.rate / 20).clamp(min, max)),
            SupportedBufferSize::Unknown => None,
        });
    let config = cpal::StreamConfig {
        channels: format.channels,
        sample_rate: format.rate,
        buffer_size: half.map_or(cpal::BufferSize::Default, cpal::BufferSize::Fixed),
    };
    let problem = shared.clone();
    let let_go = shared.ring.lock().unwrap().streams_let_go;
    let stream = device
        .build_output_stream_raw(
            config,
            format.sample,
            move |data, info| {
                let at = info.timestamp();
                let latency = at.playback.duration_since(at.callback);
                fill(&shared, format, data.bytes_mut(), latency)
            },
            move |e: cpal::Error| {
                problem.ring.lock().unwrap().stream_said(let_go, &e);
                problem.changed.notify_all();
            },
            None,
        )
        .map_err(|e| format!("The sound device couldn't play {format}: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("The sound device couldn't start: {e}"))?;
    Ok(stream)
}

/// The format to open the device in for audio of `rate` and `channels` in
/// samples of `kind`, and whether it needs converting to another rate:
/// the format itself where the device takes it, then a wider integer (which
/// only pads), then the nearest it does take.
pub(crate) fn choose(
    rate: u32,
    channels: u16,
    kind: Kind,
    supported: &[SupportedStreamConfigRange],
) -> Option<Format> {
    let samples: &[SampleFormat] = match kind {
        Kind::Int(bits) if bits <= 16 => &[
            SampleFormat::I16,
            SampleFormat::I32,
            SampleFormat::I24,
            SampleFormat::F32,
        ],
        Kind::Int(bits) if bits <= 24 => &[
            SampleFormat::I32,
            SampleFormat::I24,
            SampleFormat::F32,
            SampleFormat::I16,
        ],
        Kind::Int(_) => &[
            SampleFormat::I32,
            SampleFormat::F32,
            SampleFormat::I24,
            SampleFormat::I16,
        ],
        Kind::Float => &[
            SampleFormat::F32,
            SampleFormat::I32,
            SampleFormat::I24,
            SampleFormat::I16,
        ],
    };
    let channels = [channels, 2]
        .into_iter()
        .find(|&channels| supported.iter().any(|range| range.channels() == channels))?;
    let takes = |sample: SampleFormat, rate: u32| {
        supported.iter().any(|range| {
            range.channels() == channels
                && range.sample_format() == sample
                && range.contains_rate(rate)
        })
    };
    let format = |sample, rate| Format {
        rate,
        channels,
        sample,
    };
    if let Some(&sample) = samples.iter().find(|&&sample| takes(sample, rate)) {
        return Some(format(sample, rate));
    }
    const RATES: [u32; 8] = [
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
    ];
    samples.iter().find_map(|&sample| {
        let rates = RATES.iter().copied().filter(|&rate| takes(sample, rate));
        let above = rates.clone().find(|&other| other >= rate);
        above
            .or_else(|| rates.max())
            .map(|rate| format(sample, rate))
    })
}

/// The format to hand the system's bit-perfect output for audio of `rate` and
/// `channels` in samples of `kind`, if it offers one that keeps the rate (or,
/// with `upsampling`, multiplies it) and the channels.
pub(crate) fn bit_perfect(
    rate: u32,
    channels: u16,
    kind: Kind,
    upsampling: bool,
    offered: &[Format],
) -> Option<Format> {
    let ranges: Vec<_> = offered
        .iter()
        .map(|format| {
            SupportedStreamConfigRange::new(
                format.channels,
                format.rate,
                format.rate,
                SupportedBufferSize::Unknown,
                format.sample,
            )
        })
        .collect();
    upsampling
        .then(|| upsampled(rate, channels, kind, &ranges))
        .flatten()
        .or_else(|| choose(rate, channels, kind, &ranges).filter(|format| format.rate == rate))
        .filter(|format| format.channels == channels)
}

/// The format for audio of `rate` played at the highest rate the device
/// takes of eight, four or two times it, if it takes one.
pub(crate) fn upsampled(
    rate: u32,
    channels: u16,
    kind: Kind,
    supported: &[SupportedStreamConfigRange],
) -> Option<Format> {
    [8, 4, 2].into_iter().find_map(|times| {
        let wanted = rate * times;
        choose(wanted, channels, kind, supported).filter(|format| format.rate == wanted)
    })
}

/// What a decoder's samples are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    /// Integers with this many bits that mean something.
    Int(u32),
    Float,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::SupportedBufferSize::Unknown;

    fn range(
        channels: u16,
        min: u32,
        max: u32,
        sample: SampleFormat,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(channels, min, max, Unknown, sample)
    }

    #[test]
    fn keeps_the_format_pads_integers_and_converts_only_the_rate_it_must() {
        let dac = [
            range(2, 44_100, 192_000, SampleFormat::I16),
            range(2, 44_100, 192_000, SampleFormat::I32),
        ];
        let laptop = [
            range(2, 48_000, 48_000, SampleFormat::I16),
            range(2, 48_000, 48_000, SampleFormat::I32),
        ];
        let format = |rate, sample| Format {
            rate,
            channels: 2,
            sample,
        };
        assert_eq!(
            choose(44_100, 2, Kind::Int(16), &dac),
            Some(format(44_100, SampleFormat::I16))
        );
        assert_eq!(
            choose(96_000, 2, Kind::Int(24), &dac),
            Some(format(96_000, SampleFormat::I32))
        );
        // An MP3 decodes to floats, which this DAC doesn't take.
        assert_eq!(
            choose(44_100, 2, Kind::Float, &dac),
            Some(format(44_100, SampleFormat::I32))
        );
        assert_eq!(
            choose(44_100, 2, Kind::Int(16), &laptop),
            Some(format(48_000, SampleFormat::I16))
        );
        assert_eq!(
            choose(192_000, 2, Kind::Int(24), &laptop),
            Some(format(48_000, SampleFormat::I32))
        );
        // Mono on a stereo-only device plays as stereo.
        assert_eq!(
            choose(44_100, 1, Kind::Int(16), &dac),
            Some(format(44_100, SampleFormat::I16))
        );
        assert_eq!(choose(44_100, 2, Kind::Int(16), &[]), None);
    }

    fn fake(accepts: bool) -> crate::tests::FakeMixer {
        crate::tests::FakeMixer {
            accepts,
            ..Default::default()
        }
    }

    const SOME_FORMAT: Format = Format {
        rate: 44_100,
        channels: 2,
        sample: SampleFormat::I16,
    };

    #[test]
    fn the_output_is_engaged_or_given_back_by_the_route() {
        use std::sync::atomic::Ordering::Relaxed;
        let engaged = fake(true);
        assert_eq!(
            route(Some(&engaged), Some(SOME_FORMAT), true),
            Route::BitPerfect
        );
        assert_eq!(engaged.released.load(Relaxed), 0);
        let refused = fake(false);
        assert_eq!(route(Some(&refused), Some(SOME_FORMAT), true), Route::Mixed);
        assert_eq!(refused.released.load(Relaxed), 1);
        // Another output than the system's own is left to the mixer unasked.
        let elsewhere = fake(true);
        assert_eq!(
            route(Some(&elsewhere), Some(SOME_FORMAT), false),
            Route::Mixed
        );
        assert_eq!(elsewhere.engaged.load(Relaxed), 0);
        let idle = fake(true);
        assert_eq!(route(Some(&idle), None, true), Route::Direct);
        assert_eq!(idle.released.load(Relaxed), 1);
        assert_eq!(route(None, Some(SOME_FORMAT), true), Route::Direct);
    }

    #[test]
    fn another_device_than_the_systems_is_not_engaged() {
        use std::sync::atomic::Ordering::Relaxed;
        let system = fake(true);
        let shared = Arc::new(Shared::default());
        let _ = open_output(
            Some(&system),
            Some("alsa:hw:CARD=Other"),
            Some(SOME_FORMAT),
            None,
            &[],
            &shared,
        );
        assert_eq!(system.engaged.load(Relaxed), 0);
    }

    #[test]
    fn a_stream_that_cant_open_gives_the_output_back() {
        use std::sync::atomic::Ordering::Relaxed;
        let system = fake(true);
        let shared = Arc::new(Shared::default());
        let opened = open_output(Some(&system), None, Some(SOME_FORMAT), None, &[], &shared);
        assert!(opened.is_err());
        assert_eq!(system.released.load(Relaxed), 1);
    }

    #[test]
    fn a_device_lost_pauses_and_is_found_again_on_play() {
        for kind in [ErrorKind::DeviceNotAvailable, ErrorKind::DeviceChanged] {
            let mut ring = Ring {
                device: Some(Device {
                    name: "WH-1000XM4".into(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            ring.stream_failed(&kind.into());
            assert!(ring.paused, "{kind:?}");
            assert_eq!(
                ring.problem.as_deref(),
                Some("WH-1000XM4 is unavailable."),
                "{kind:?}"
            );
            assert!(ring.unpause(), "{kind:?}");
            assert!(!ring.paused, "{kind:?}");
            assert_eq!(ring.generation, 1, "{kind:?}");
            // The decoder waits for the device found again.
            assert!(ring.device.is_none() && ring.problem.is_none(), "{kind:?}");
            ring.stream_said(0, &kind.into());
            assert!(!ring.paused, "the old stream is let go of: {kind:?}");
        }
    }

    #[test]
    fn a_stream_let_go_of_is_no_longer_heard() {
        let mut ring = Ring {
            streams_let_go: 1,
            ..Default::default()
        };
        ring.stream_said(0, &ErrorKind::DeviceNotAvailable.into());
        assert!(!ring.paused && ring.problem.is_none());
        ring.stream_said(1, &ErrorKind::DeviceNotAvailable.into());
        assert!(ring.paused && ring.device_lost);
    }

    #[test]
    fn a_stream_closed_is_let_go_of_and_no_longer_asks() {
        let shared = Shared::default();
        shared.ring.lock().unwrap().asked_at = Some(Instant::now());
        close(&shared, &mut None);
        let ring = shared.ring.lock().unwrap();
        assert_eq!(ring.streams_let_go, 1);
        assert_eq!(ring.asked_at, None);
    }

    #[test]
    fn a_skip_plays_on_and_a_failure_is_told_of() {
        let mut ring = Ring::default();
        ring.stream_failed(&ErrorKind::Xrun.into());
        assert!(!ring.paused);
        assert_eq!(ring.problem, None);
        ring.stream_failed(&ErrorKind::BackendError.into());
        assert!(!ring.paused);
        assert!(ring.problem.is_some());
        assert!(!ring.unpause(), "the device is still there");
        assert_eq!(ring.generation, 0);
    }

    #[test]
    fn nothing_plays_or_opens_on_a_device_lost() {
        let shared = Shared::default();
        {
            let mut ring = shared.ring.lock().unwrap();
            ring.segments.push_back(Segment {
                track: 0,
                format: SOME_FORMAT,
                data: VecDeque::from(vec![1; SOME_FORMAT.frame_bytes() * 44_100]),
                ended: true,
                start: 0,
                played: 0,
                path: Vec::new(),
            });
            ring.device_lost = true;
        }
        let mut out = [1; 64];
        fill(&shared, SOME_FORMAT, &mut out, Duration::ZERO);
        assert_eq!(out, [0; 64]);
        let ring = shared.ring.lock().unwrap();
        assert_eq!(ring.segments[0].played, 0);
        assert_eq!(ring.wanted(None), None);
    }

    #[test]
    fn a_stream_that_stopped_asking_for_sound_went_quiet() {
        let shared = Shared::default();
        let start = Instant::now();
        assert!(!shared.ring.lock().unwrap().went_quiet(start), "none open");
        fill(&shared, SOME_FORMAT, &mut [0; 64], Duration::ZERO);
        let mut ring = shared.ring.lock().unwrap();
        let asked = ring.asked_at.expect("the ask is noted");
        assert!(!ring.went_quiet(asked + QUIET), "asking as a device does");
        assert!(ring.went_quiet(asked + QUIET + Duration::from_millis(1)));
        ring.device_lost = true;
        assert!(!ring.went_quiet(asked + QUIET * 2), "lost already");
    }

    #[test]
    fn bit_perfect_keeps_the_rate_and_channels_or_none() {
        let format = |rate, channels, sample| Format {
            rate,
            channels,
            sample,
        };
        let offered = [
            format(44_100, 2, SampleFormat::I16),
            format(96_000, 2, SampleFormat::F32),
            format(192_000, 2, SampleFormat::F32),
        ];
        assert_eq!(
            bit_perfect(44_100, 2, Kind::Int(16), false, &offered),
            Some(format(44_100, 2, SampleFormat::I16))
        );
        // 24 bits fit a float exactly.
        assert_eq!(
            bit_perfect(96_000, 2, Kind::Int(24), false, &offered),
            Some(format(96_000, 2, SampleFormat::F32))
        );
        // Upsampling takes the multiple the output offers.
        assert_eq!(
            bit_perfect(96_000, 2, Kind::Int(24), true, &offered),
            Some(format(192_000, 2, SampleFormat::F32))
        );
        // A rate it doesn't offer is left to the system's mixer, not
        // resampled here and called bit-perfect.
        assert_eq!(bit_perfect(48_000, 2, Kind::Int(16), false, &offered), None);
        assert_eq!(bit_perfect(44_100, 1, Kind::Int(16), false, &offered), None);
        assert_eq!(bit_perfect(44_100, 2, Kind::Int(16), false, &[]), None);
    }
}
