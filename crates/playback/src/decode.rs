//! One track: its download opened by FFmpeg, decoded, and turned by an
//! FFmpeg filter chain into the format the device is opened in.

use cpal::{SampleFormat, SupportedStreamConfigRange};
use ffmpeg_next::format::context::{Input, StreamIo};
use ffmpeg_next::util::frame::Audio as Frame;
use ffmpeg_next::{
    ChannelLayout, Error, Packet, Rational, codec, decoder, ffi, filter, format, media,
};

use crate::download::Download;
use crate::output::{Kind, choose};
use crate::{Format, Leveling, Step, Track};

pub(crate) struct Decoding {
    input: Input,
    stream: usize,
    time_base: Rational,
    decoder: decoder::Audio,
    graph: filter::Graph,
    pub format: Format,
    pub path: Vec<Step>,
    /// Frames of output still to drop after a seek, to start where asked
    /// rather than where the file's last seek point was; and the time asked
    /// for, until the first frame shows how far off that is.
    skip: u64,
    seeked_to: Option<f64>,
    /// The demuxer has run out, and the decoder and filters have been told.
    draining: bool,
    frame: Frame,
    filtered: Frame,
}

/// Why a step stopped.
pub(crate) enum Stop {
    /// Waiting for the download took long enough that a command may want
    /// the decoder; the step can be tried again.
    Interrupted,
    Failed(String),
}

impl From<Error> for Stop {
    fn from(e: Error) -> Self {
        match e {
            Error::Exit => Stop::Interrupted,
            e => Stop::Failed(e.to_string()),
        }
    }
}

impl Decoding {
    /// Opens `download` and readies it for a device that takes `supported`.
    /// A read waiting for the download gives up once `abort` says so.
    pub fn open(
        download: Download,
        track: &Track,
        leveling: Leveling,
        supported: &[SupportedStreamConfigRange],
        abort: impl FnMut() -> bool + Send + 'static,
    ) -> Result<Self, Stop> {
        let io = StreamIo::from_read_seek(download)?;
        let input = format::input_from_stream_with_interrupt(io, None, None, abort)?;
        let stream = input
            .streams()
            .best(media::Type::Audio)
            .ok_or_else(|| Stop::Failed("The file has no audio in it.".into()))?;
        let index = stream.index();
        let time_base = stream.time_base();
        let mut decoder = codec::context::Context::from_parameters(stream.parameters())?.decoder();
        decoder.set_packet_time_base(time_base);
        let decoder = decoder.audio()?;
        let codec = decoder.id().name().to_uppercase();
        // How many bits of the samples count: 24 of the 32 FLAC decodes a
        // 24-bit file into.
        let raw_bits = unsafe { (*decoder.as_ptr()).bits_per_raw_sample } as u32;
        let lossless = unsafe { ffi::avcodec_descriptor_get(decoder.id().into()).as_ref() }
            .is_some_and(|codec| codec.props as u32 & ffi::AV_CODEC_PROP_LOSSLESS as u32 != 0);
        let kind = match decoder.format() {
            format::Sample::F32(_) | format::Sample::F64(_) => Kind::Float,
            sample => Kind::Int(match raw_bits {
                0 => sample.bytes() as u32 * 8,
                bits => bits,
            }),
        };
        let channels = decoder.channels();
        let rate = decoder.rate();
        let out = choose(rate, channels, kind, supported).ok_or_else(|| {
            Stop::Failed("The sound device takes no format this can play.".into())
        })?;

        let mut path = vec![Step {
            text: format!("{codec} {}", describe(rate, kind)),
            lossless,
        }];
        let mut filters = Vec::new();
        if let Some(gain) = track.gain.applied(leveling) {
            filters.push(format!("volume=volume={gain:.2}dB:precision=double"));
            let which = match leveling {
                Leveling::Album => "album",
                _ => "track",
            };
            path.push(Step {
                text: format!("Volume leveling {gain:+.1} dB ({which})"),
                lossless: false,
            });
        }
        if out.rate != rate {
            filters.push(format!(
                "aresample=out_sample_rate={}:filter_size=64:phase_shift=10:cutoff=0.97",
                out.rate
            ));
            path.push(Step {
                text: format!("Resampled {} to {}", khz(rate), khz(out.rate)),
                lossless: false,
            });
        }
        let out_bits = match out.sample {
            SampleFormat::I16 => 16,
            SampleFormat::I24 => 24,
            _ => 32,
        };
        let sample_name = match out.sample {
            SampleFormat::I16 => "s16",
            SampleFormat::F32 => "flt",
            _ => "s32",
        };
        // Leveling or resampling made new samples, finer than 16 bits.
        let changed = path.len() > 1;
        match (kind, out.sample) {
            (Kind::Int(bits), SampleFormat::I16 | SampleFormat::I24 | SampleFormat::I32)
                if bits > out_bits =>
            {
                path.push(Step {
                    text: format!("Reduced from {bits} to {out_bits} bits"),
                    lossless: false,
                });
            }
            (Kind::Float, SampleFormat::F32) => {}
            (Kind::Float, _) => path.push(Step {
                text: format!("Converted to {out_bits}-bit integers"),
                lossless: false,
            }),
            (Kind::Int(bits), SampleFormat::F32) if bits > 24 => path.push(Step {
                text: "Converted to 32-bit floats".into(),
                lossless: false,
            }),
            _ => {}
        }
        if out.sample == SampleFormat::I16 && (changed || kind != Kind::Int(16)) {
            filters.push("aresample=out_sample_fmt=s16:dither_method=triangular".into());
        }
        let layout = ChannelLayout::default(i32::from(out.channels));
        filters.push(format!(
            "aformat=sample_fmts={sample_name}:sample_rates={}:channel_layouts=0x{:x}",
            out.rate,
            layout.bits()
        ));
        // Mixing down, or spreading mono over two, changes the samples.
        if out.channels != channels {
            path.push(Step {
                text: format!("{channels} channels played on {}", out.channels),
                lossless: false,
            });
        }

        let mut in_layout = decoder.channel_layout();
        if in_layout.bits() == 0 {
            in_layout = ChannelLayout::default(i32::from(channels));
        }
        let mut graph = filter::Graph::new();
        let args = format!(
            "time_base={time_base}:sample_rate={rate}:sample_fmt={}:channel_layout=0x{:x}",
            decoder.format().name(),
            in_layout.bits()
        );
        graph.add(
            &filter::find("abuffer").expect("FFmpeg's abuffer"),
            "in",
            &args,
        )?;
        graph.add(
            &filter::find("abuffersink").expect("FFmpeg's abuffersink"),
            "out",
            "",
        )?;
        graph
            .output("in", 0)?
            .input("out", 0)?
            .parse(&filters.join(","))?;
        graph.validate()?;

        Ok(Decoding {
            input,
            stream: index,
            time_base,
            decoder,
            graph,
            format: out,
            path,
            skip: 0,
            seeked_to: None,
            draining: false,
            frame: Frame::empty(),
            filtered: Frame::empty(),
        })
    }

    /// Moves to `seconds` into the track. What it decodes next starts there.
    pub fn seek(&mut self, seconds: f64) -> Result<(), Stop> {
        let at = (seconds * f64::from(ffmpeg_next::ffi::AV_TIME_BASE)) as i64;
        self.input.seek(at, ..at)?;
        self.decoder.flush();
        self.seeked_to = Some(seconds);
        self.skip = 0;
        Ok(())
    }

    /// Lets a read that gave up for a command that turned out to need
    /// nothing from this track go on.
    pub fn resume(&mut self) {
        self.input.clear_interrupt();
    }

    /// Decodes the next packet into `out`, in the device's format. False
    /// once the track has all been decoded.
    pub fn step(&mut self, out: &mut Vec<u8>) -> Result<bool, Stop> {
        if self.draining {
            return Ok(false);
        }
        let mut packet = Packet::empty();
        match packet.read(&mut self.input) {
            Ok(()) if packet.stream() == self.stream => {
                // A damaged packet is skipped, as players do.
                if self.decoder.send_packet(&packet).is_ok() {
                    self.receive(out)?;
                }
                Ok(true)
            }
            Ok(()) | Err(Error::InvalidData) => Ok(true),
            Err(Error::Eof) => {
                self.draining = true;
                let _ = self.decoder.send_eof();
                self.receive(out)?;
                self.graph.get("in").expect("the source").source().flush()?;
                self.drain(out)?;
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn receive(&mut self, out: &mut Vec<u8>) -> Result<(), Stop> {
        while self.decoder.receive_frame(&mut self.frame).is_ok() {
            if let Some(target) = self.seeked_to.take() {
                let start = self
                    .frame
                    .timestamp()
                    .map(|pts| pts as f64 * f64::from(self.time_base))
                    .unwrap_or(target);
                self.skip = ((target - start).max(0.0) * f64::from(self.format.rate)) as u64;
            }
            self.graph
                .get("in")
                .expect("the source")
                .source()
                .add(&self.frame)?;
            self.drain(out)?;
        }
        Ok(())
    }

    /// Takes what the filters have finished into `out`.
    fn drain(&mut self, out: &mut Vec<u8>) -> Result<(), Stop> {
        let mut sink = self.graph.get("out").expect("the sink");
        while sink.sink().frame(&mut self.filtered).is_ok() {
            let frame_bytes = self.format.frame_bytes();
            let frames = self.filtered.samples();
            let dropped = (self.skip as usize).min(frames);
            self.skip -= dropped as u64;
            let width = match self.format.sample {
                SampleFormat::I16 => 2,
                _ => 4,
            };
            let bytes = &self.filtered.data(0)[dropped * width * usize::from(self.format.channels)
                ..frames * width * usize::from(self.format.channels)];
            if self.format.sample == SampleFormat::I24 {
                // cpal's 24-bit samples are the low three bytes of four.
                for sample in bytes.as_chunks::<4>().0 {
                    let value = i32::from_ne_bytes(*sample) >> 8;
                    out.extend_from_slice(&value.to_ne_bytes());
                }
            } else {
                out.extend_from_slice(bytes);
            }
            debug_assert_eq!(out.len() % frame_bytes, 0);
        }
        Ok(())
    }
}

fn khz(rate: u32) -> String {
    let khz = f64::from(rate) / 1000.0;
    if khz.fract() == 0.0 {
        format!("{khz:.0} kHz")
    } else {
        format!("{khz:.1} kHz")
    }
}

fn describe(rate: u32, kind: Kind) -> String {
    match kind {
        Kind::Int(bits) => format!("{} {bits}-bit", khz(rate)),
        Kind::Float => khz(rate),
    }
}
