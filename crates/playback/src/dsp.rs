//! The sound processing the listener chooses, between the decoder and the
//! device: headroom, a parametric EQ and crossfeed, each an FFmpeg filter.
//! Upsampling picks the device's rate, so the decoder sees to it.

use crate::{Quality, Step};

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

impl Filter {
    /// Whether the filter has a gain: passes have none.
    pub fn has_gain(self) -> bool {
        !matches!(self, Filter::LowPass | Filter::HighPass)
    }
}

impl Band {
    /// Whether FFmpeg gets a filter of it at `rate`: not at a frequency at
    /// or above what `rate` holds.
    fn fits(&self, rate: f64) -> bool {
        self.frequency > 0.0 && self.frequency < rate / 2.0 && self.q > 0.0
    }

    /// The FFmpeg filter, in double precision, or none where it would
    /// change nothing: a flat peak or shelf, or a frequency at or above
    /// what `rate` holds.
    fn filter(&self, rate: u32) -> Option<String> {
        if !self.fits(f64::from(rate)) {
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

    /// How many dB the band changes `frequency` by at `rate`, from the Audio
    /// EQ Cookbook's formulas, which FFmpeg's biquads follow.
    pub fn response(&self, rate: f64, frequency: f64) -> f64 {
        use std::f64::consts::PI;
        if !self.fits(rate) {
            return 0.0;
        }
        let a = 10f64.powf(self.gain / 40.0);
        let (sin, cos) = (2.0 * PI * self.frequency / rate).sin_cos();
        let alpha = sin / (2.0 * self.q);
        let root = 2.0 * a.sqrt() * alpha;
        let (b, d) = match self.filter {
            Filter::Peak => (
                [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
                [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
            ),
            Filter::LowShelf => (
                [
                    a * ((a + 1.0) - (a - 1.0) * cos + root),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - root),
                ],
                [
                    (a + 1.0) + (a - 1.0) * cos + root,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - root,
                ],
            ),
            Filter::HighShelf => (
                [
                    a * ((a + 1.0) + (a - 1.0) * cos + root),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - root),
                ],
                [
                    (a + 1.0) - (a - 1.0) * cos + root,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - root,
                ],
            ),
            Filter::LowPass => (
                [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
                [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            ),
            Filter::HighPass => (
                [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
                [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            ),
        };
        let w = 2.0 * PI * frequency / rate;
        let size = |c: [f64; 3]| {
            let re = c[0] + c[1] * w.cos() + c[2] * (2.0 * w).cos();
            let im = c[1] * w.sin() + c[2] * (2.0 * w).sin();
            re.hypot(im)
        };
        20.0 * (size(b) / size(d)).log10()
    }
}

fn volume(db: f64) -> String {
    format!("volume=volume={db:.2}dB:precision=double")
}

fn changed(text: String) -> Step {
    Step {
        text,
        quality: Quality::Enhanced,
    }
}
