//! The sound processing the listener chooses, between the decoder and the
//! device: headroom, a parametric EQ and crossfeed, each an FFmpeg filter.
//! Upsampling picks the device's rate, so the decoder sees to it.

use crate::Step;

/// What the listener has the sound go through. The default changes nothing.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Dsp {
    /// How many dB it is turned down first, to leave room for what boosts
    /// it.
    pub headroom: f64,
    /// The EQ, when it is on.
    pub eq: Option<Equalizer>,
    pub crossfeed: Crossfeed,
    /// Played at the highest rate the device takes that is the file's times
    /// two, four or eight.
    pub upsampling: bool,
}

/// A parametric EQ: a gain for all of it, then its bands in turn.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Equalizer {
    pub preamp: f64,
    pub bands: Vec<Band>,
}

/// One of the EQ's filters: its centre or corner in Hz, its gain in dB
/// (passes have none) and its Q.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub filter: Filter,
    pub frequency: f64,
    pub gain: f64,
    pub q: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

/// How much of each ear's channel the other ear hears, as from speakers in
/// front: for headphones, on stereo only.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Crossfeed {
    #[default]
    Off,
    Light,
    Medium,
    Strong,
}

/// Gains closer to nothing than this change nothing.
const UNCHANGED: f64 = 0.005;

impl Dsp {
    /// The FFmpeg filters for audio of `rate` and `channels`, and the signal
    /// path's steps for them.
    pub(crate) fn filters(&self, rate: u32, channels: u16) -> (Vec<String>, Vec<Step>) {
        let mut filters = Vec::new();
        let mut path = Vec::new();
        if self.headroom.abs() >= UNCHANGED {
            filters.push(volume(self.headroom));
            path.push(changed(format!("Headroom {:+.1} dB", self.headroom)));
        }
        if let Some(eq) = &self.eq {
            let bands: Vec<String> = eq
                .bands
                .iter()
                .filter_map(|band| band.filter(rate))
                .collect();
            if eq.preamp.abs() >= UNCHANGED {
                filters.push(volume(eq.preamp));
            }
            let mut parts = match bands.len() {
                0 => Vec::new(),
                1 => vec!["1 band".to_owned()],
                n => vec![format!("{n} bands")],
            };
            if eq.preamp.abs() >= UNCHANGED {
                parts.push(format!("preamp {:+.1} dB", eq.preamp));
            }
            if !parts.is_empty() {
                path.push(changed(format!("EQ, {}", parts.join(", "))));
            }
            filters.extend(bands);
        }
        let strength = match self.crossfeed {
            Crossfeed::Off => None,
            Crossfeed::Light => Some((0.15, "light")),
            Crossfeed::Medium => Some((0.3, "medium")),
            Crossfeed::Strong => Some((0.5, "strong")),
        };
        if let Some((strength, name)) = strength.filter(|_| channels == 2) {
            filters.push(format!("crossfeed=strength={strength}"));
            path.push(changed(format!("Crossfeed, {name}")));
        }
        (filters, path)
    }
}

impl Band {
    /// The FFmpeg filter, in double precision, or none where it would
    /// change nothing: a flat peak or shelf, or a frequency at or above
    /// what `rate` holds.
    fn filter(&self, rate: u32) -> Option<String> {
        if self.frequency <= 0.0 || self.frequency >= f64::from(rate) / 2.0 || self.q <= 0.0 {
            return None;
        }
        let name = match self.filter {
            Filter::Peak => "equalizer",
            Filter::LowShelf => "lowshelf",
            Filter::HighShelf => "highshelf",
            Filter::LowPass => "lowpass",
            Filter::HighPass => "highpass",
        };
        let shape = format!("{name}=f={}:t=q:w={}:precision=f64", self.frequency, self.q);
        match self.filter {
            Filter::LowPass | Filter::HighPass => Some(shape),
            _ if self.gain.abs() < UNCHANGED => None,
            _ => Some(format!("{shape}:g={}", self.gain)),
        }
    }
}

fn volume(db: f64) -> String {
    format!("volume=volume={db:.2}dB:precision=double")
}

fn changed(text: String) -> Step {
    Step {
        text,
        lossless: false,
    }
}
