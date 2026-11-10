//! A track decoded to the one format a receiver that is sent its audio takes,
//! 44.1 kHz 16-bit stereo, by the decoder the player uses.

use std::io::{self, Read};
use std::path::Path;

use cpal::{SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};

use crate::decode::{Decoding, Stop};
use crate::{Dsp, Gain, Leveling, Track, download};

const RATE: u32 = 44_100;
const CHANNELS: u16 = 2;

/// The samples of a track, as the machine's own byte order has them.
pub struct Pcm {
    decoding: Decoding,
    decoded: Vec<u8>,
    taken: usize,
    ended: bool,
}

/// Opens the track in the file at `path`, from `start` seconds into it.
pub fn open(path: &Path, start: f64) -> Result<Pcm, String> {
    let file =
        download::open(path).map_err(|problem| format!("The track couldn't be read: {problem}"))?;
    let only = SupportedStreamConfigRange::new(
        CHANNELS,
        RATE,
        RATE,
        SupportedBufferSize::Unknown,
        SampleFormat::I16,
    );
    let track = Track {
        id: String::new(),
        gain: Gain::default(),
    };
    let mut decoding = Decoding::open(
        file,
        &track,
        Leveling::Off,
        &Dsp::default(),
        &[only],
        &[],
        || false,
    )
    .map_err(failed)?;
    if start > 0.0 {
        decoding.seek(start).map_err(failed)?;
    }
    Ok(Pcm {
        decoding,
        decoded: Vec::new(),
        taken: 0,
        ended: false,
    })
}

fn failed(stop: Stop) -> String {
    match stop {
        Stop::Failed(why) => why,
        Stop::Interrupted => "The track's decoding was interrupted.".into(),
    }
}

impl Read for Pcm {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.taken == self.decoded.len() && !self.ended {
            self.decoded.clear();
            self.taken = 0;
            match self.decoding.step(&mut self.decoded) {
                Ok(more) => self.ended = !more,
                Err(stop) => return Err(io::Error::other(failed(stop))),
            }
        }
        let count = buf.len().min(self.decoded.len() - self.taken);
        buf[..count].copy_from_slice(&self.decoded[self.taken..self.taken + count]);
        self.taken += count;
        Ok(count)
    }
}
