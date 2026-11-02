//! The pipeline checked by numbers: what comes out of the decoder and filters
//! against what went in. The device itself is checked with the loopback test
//! at the end, on a machine with ALSA's loopback card.

use std::io::Read;

use cpal::SampleFormat::{self, F32, I16, I24, I32};
use cpal::SupportedBufferSize::Unknown;
use cpal::SupportedStreamConfigRange;

use super::*;

/// A WAV file of `samples` (interleaved, `bits` of each significant), as a
/// finished download.
fn wav(rate: u32, channels: u16, bits: u16, samples: &[i32]) -> Download {
    let width = bits / 8;
    let data_len = samples.len() as u32 * u32::from(width);
    let mut file = Vec::new();
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + data_len).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&channels.to_le_bytes());
    file.extend_from_slice(&rate.to_le_bytes());
    file.extend_from_slice(&(rate * u32::from(channels * width)).to_le_bytes());
    file.extend_from_slice(&(channels * width).to_le_bytes());
    file.extend_from_slice(&bits.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        file.extend_from_slice(&sample.to_le_bytes()[..usize::from(width)]);
    }
    let folder = std::env::temp_dir().join("playback-tests");
    let (download, mut fill) = download::new(&folder, Some(file.len() as u64)).unwrap();
    assert!(fill.append(&file));
    fill.finish();
    download
}

/// A ramp that runs through the whole range of `bits`, so a sample out of
/// place or changed shows.
fn ramp(frames: usize, channels: usize, bits: u32) -> Vec<i32> {
    let span = 1i64 << bits;
    (0..frames * channels)
        .map(|at| ((at as i64 * 7919) % span - span / 2) as i32)
        .collect()
}

fn device(rates: &[u32], samples: &[SampleFormat]) -> Vec<SupportedStreamConfigRange> {
    let mut supported = Vec::new();
    for &rate in rates {
        for &sample in samples {
            supported.push(SupportedStreamConfigRange::new(
                2, rate, rate, Unknown, sample,
            ));
        }
    }
    supported
}

/// A system mixer that offers `offered`, takes an engage if `accepts`, and
/// counts what it is asked.
#[derive(Default)]
pub(crate) struct FakeMixer {
    pub offered: Vec<Format>,
    pub accepts: bool,
    pub asked: AtomicUsize,
    pub engaged: AtomicUsize,
    pub released: AtomicUsize,
}

impl SystemMixer for FakeMixer {
    fn bit_perfect_formats(&self) -> Vec<Format> {
        self.asked.fetch_add(1, Relaxed);
        self.offered.clone()
    }

    fn engage_bit_perfect(&self, _: Format) -> bool {
        self.engaged.fetch_add(1, Relaxed);
        self.accepts
    }

    fn release_bit_perfect(&self) {
        self.released.fetch_add(1, Relaxed);
    }
}

fn track(gain: Gain) -> Track {
    Track {
        id: "test".into(),
        gain,
    }
}

/// Everything the decoder makes of `download`, and the decoding, for its
/// format and path.
fn decode(
    download: Download,
    gain: Gain,
    leveling: Leveling,
    supported: &[SupportedStreamConfigRange],
    seek: Option<f64>,
) -> (Vec<u8>, Decoding) {
    process(download, gain, leveling, &Dsp::default(), supported, seek)
}

/// The same, through `dsp`.
fn process(
    download: Download,
    gain: Gain,
    leveling: Leveling,
    dsp: &Dsp,
    supported: &[SupportedStreamConfigRange],
    seek: Option<f64>,
) -> (Vec<u8>, Decoding) {
    let _ = ffmpeg_next::init();
    let mut decoding = Decoding::open(
        download,
        &track(gain),
        leveling,
        dsp,
        supported,
        &[],
        || false,
    )
    .unwrap_or_else(|_| panic!("the test file opens"));
    if let Some(seconds) = seek {
        decoding.seek(seconds).unwrap_or_else(|_| panic!("seeks"));
    }
    let mut out = Vec::new();
    while decoding
        .step(&mut out)
        .unwrap_or_else(|_| panic!("decodes"))
    {}
    (out, decoding)
}

fn ints(bytes: &[u8], width: usize) -> Vec<i32> {
    bytes
        .chunks_exact(width)
        .map(|sample| match width {
            2 => i32::from(i16::from_ne_bytes(sample.try_into().unwrap())),
            _ => i32::from_ne_bytes(sample.try_into().unwrap()),
        })
        .collect()
}

#[test]
fn sixteen_bits_reach_a_sixteen_bit_device_untouched() {
    let samples = ramp(44_100, 2, 16);
    let (out, decoding) = decode(
        wav(44_100, 2, 16, &samples),
        Gain::default(),
        Leveling::Off,
        &device(&[44_100, 48_000], &[I16, I32]),
        None,
    );
    assert_eq!(decoding.format.sample, I16);
    assert_eq!(ints(&out, 2), samples);
    assert!(
        decoding
            .path
            .iter()
            .all(|step| step.quality == Quality::Lossless),
        "{:?}",
        decoding.path
    );
}

#[test]
fn sixteen_bits_are_only_padded_on_a_thirty_two_bit_device() {
    let samples = ramp(4410, 2, 16);
    let (out, decoding) = decode(
        wav(44_100, 2, 16, &samples),
        Gain::default(),
        Leveling::Off,
        &device(&[44_100], &[I32]),
        None,
    );
    assert_eq!(decoding.format.sample, I32);
    let padded: Vec<i32> = samples.iter().map(|sample| sample << 16).collect();
    assert_eq!(ints(&out, 4), padded);
    assert!(
        decoding
            .path
            .iter()
            .all(|step| step.quality == Quality::Lossless)
    );
}

#[test]
fn twenty_four_bits_reach_the_device_untouched_in_either_container() {
    let samples = ramp(9600, 2, 24);
    let (out, decoding) = decode(
        wav(96_000, 2, 24, &samples),
        Gain::default(),
        Leveling::Off,
        &device(&[96_000], &[I16, I32]),
        None,
    );
    assert_eq!(decoding.format.sample, I32);
    let left: Vec<i32> = samples.iter().map(|sample| sample << 8).collect();
    assert_eq!(ints(&out, 4), left);
    assert!(
        decoding
            .path
            .iter()
            .all(|step| step.quality == Quality::Lossless)
    );

    let (out, decoding) = decode(
        wav(96_000, 2, 24, &samples),
        Gain::default(),
        Leveling::Off,
        &device(&[96_000], &[I24]),
        None,
    );
    assert_eq!(decoding.format.sample, I24);
    assert_eq!(ints(&out, 4), samples);
}

#[test]
fn a_rate_the_device_lacks_is_resampled_and_said_so() {
    let samples = ramp(44_100, 2, 16);
    let (out, decoding) = decode(
        wav(44_100, 2, 16, &samples),
        Gain::default(),
        Leveling::Off,
        &device(&[48_000], &[I16]),
        None,
    );
    assert_eq!(decoding.format.rate, 48_000);
    let frames = out.len() / 4;
    assert!((47_900..=48_100).contains(&frames), "{frames} frames");
    assert!(
        decoding.path.iter().any(
            |step| step.text == "Resampled 44.1 kHz to 48 kHz" && step.quality == Quality::High
        )
    );
}

#[test]
fn mono_spread_over_two_channels_is_said_to_change_it() {
    let samples = ramp(4_410, 1, 16);
    let (_, decoding) = decode(
        wav(44_100, 1, 16, &samples),
        Gain::default(),
        Leveling::Off,
        &device(&[44_100], &[I16]),
        None,
    );
    assert_eq!(decoding.format.channels, 2);
    assert!(
        decoding
            .path
            .iter()
            .any(|step| step.text == "1 channels played on 2" && step.quality == Quality::High),
        "{:?}",
        decoding.path
    );
}

#[test]
fn leveling_turns_it_down_by_the_gain_and_never_clips() {
    let samples: Vec<i32> = (0..4410 * 2)
        .map(|at| if at % 2 == 0 { 16_000 } else { -16_000 })
        .collect();
    let gain = Gain {
        track: Some(-6.0206),
        ..Gain::default()
    };
    let (out, decoding) = decode(
        wav(44_100, 2, 16, &samples),
        gain.clone(),
        Leveling::Track,
        &device(&[44_100], &[F32]),
        None,
    );
    let floats: Vec<f32> = out
        .as_chunks::<4>()
        .0
        .iter()
        .map(|sample| f32::from_ne_bytes(*sample))
        .collect();
    let expected = 8_000.0 / 32_768.0;
    assert!(
        floats
            .iter()
            .all(|sample| (sample.abs() - expected).abs() < 1e-4)
    );
    assert!(
        decoding
            .path
            .iter()
            .any(|step| step.text == "Volume leveling -6.0 dB (track)")
    );

    // +10 dB on a track that already peaks at half scale is held to +6 dB.
    let loud = Gain {
        album: Some(10.0),
        album_peak: Some(0.5),
        ..Gain::default()
    };
    assert!((loud.applied(Leveling::Album).unwrap() - 6.0206).abs() < 1e-3);
    assert_eq!(loud.applied(Leveling::Off), None);
    // Without an album gain, the track's is used, and the fallback after that.
    assert_eq!(gain.applied(Leveling::Album), Some(-6.0206));
    let fallback = Gain {
        fallback: Some(-3.0),
        ..Gain::default()
    };
    assert_eq!(fallback.applied(Leveling::Track), Some(-3.0));
    // Without a peak, nothing says how far up is safe.
    let unknown = Gain {
        track: Some(4.0),
        ..Gain::default()
    };
    assert_eq!(unknown.applied(Leveling::Track), None);
}

#[test]
fn a_seek_starts_on_the_sample_asked_for() {
    let samples = ramp(44_100 * 3, 2, 16);
    let (out, _) = decode(
        wav(44_100, 2, 16, &samples),
        Gain::default(),
        Leveling::Off,
        &device(&[44_100], &[I16]),
        Some(1.5),
    );
    let from = 66_150 * 2;
    assert_eq!(ints(&out, 2), samples[from..]);
}

#[test]
fn a_download_still_arriving_is_waited_for() {
    let samples = ramp(4410, 2, 16);
    let mut whole = Vec::new();
    wav(44_100, 2, 16, &samples)
        .read_to_end(&mut whole)
        .unwrap();
    let (download, mut fill) = download::new(
        &std::env::temp_dir().join("playback-tests"),
        Some(whole.len() as u64),
    )
    .unwrap();
    let writer = std::thread::spawn(move || {
        for chunk in whole.chunks(1000) {
            std::thread::sleep(std::time::Duration::from_millis(2));
            fill.append(chunk);
        }
        fill.finish();
    });
    let _ = ffmpeg_next::init();
    let mut decoding = Decoding::open(
        download,
        &track(Gain::default()),
        Leveling::Off,
        &Dsp::default(),
        &device(&[44_100], &[I16]),
        &[],
        || false,
    )
    .unwrap_or_else(|_| panic!("opens"));
    let mut out = Vec::new();
    while decoding
        .step(&mut out)
        .unwrap_or_else(|_| panic!("decodes"))
    {}
    writer.join().unwrap();
    assert_eq!(ints(&out, 2), samples);
}

/// Silent MPEG-1 layer 3 frames, 128 kbit/s at 44.1 kHz: a header and zeros.
fn silent_mp3(frames: usize) -> Vec<u8> {
    const FRAME_BYTES: usize = 417;
    let mut frame = vec![0; FRAME_BYTES];
    frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
    frame.repeat(frames)
}

#[test]
fn a_download_of_unknown_length_opens_before_the_rest_has_arrived() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let whole = silent_mp3(400);
    let (download, mut fill) =
        download::new(&std::env::temp_dir().join("playback-tests"), None).unwrap();
    let opened = Arc::new(AtomicBool::new(false));
    let waited_in_vain = Arc::new(AtomicBool::new(false));
    let writer = std::thread::spawn({
        let (opened, waited_in_vain) = (opened.clone(), waited_in_vain.clone());
        move || {
            let (first, rest) = whole.split_at(whole.len() * 3 / 4);
            fill.append(first);
            let patience = std::time::Instant::now();
            while !opened.load(SeqCst) {
                if patience.elapsed() > std::time::Duration::from_secs(10) {
                    waited_in_vain.store(true, SeqCst);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            fill.append(rest);
            fill.finish();
        }
    });
    let _ = ffmpeg_next::init();
    let mut decoding = Decoding::open(
        download,
        &track(Gain::default()),
        Leveling::Off,
        &Dsp::default(),
        &device(&[44_100], &[I16]),
        &[],
        || false,
    )
    .unwrap_or_else(|_| panic!("opens"));
    opened.store(true, SeqCst);
    let mut out = Vec::new();
    while decoding
        .step(&mut out)
        .unwrap_or_else(|_| panic!("decodes"))
    {}
    writer.join().unwrap();
    assert!(
        !waited_in_vain.load(SeqCst),
        "open waited for the whole download"
    );
    assert!(!out.is_empty());
}

/// A sine of `frequency` at a quarter of full scale, `seconds` long, as
/// 24-bit stereo samples: in both channels, or the left only.
fn sine(rate: u32, frequency: f64, seconds: f64, left_only: bool) -> Vec<i32> {
    let amplitude = f64::from(1 << 23) / 4.0;
    let frames = (f64::from(rate) * seconds) as usize;
    (0..frames)
        .flat_map(|at| {
            let phase = 2.0 * std::f64::consts::PI * frequency * at as f64 / f64::from(rate);
            let value = (amplitude * phase.sin()).round() as i32;
            [value, if left_only { 0 } else { value }]
        })
        .collect()
}

/// The root mean square of a `channel` of 32-bit stereo `out`, past its
/// first half second at `rate`, when filters have settled.
fn rms(out: &[u8], channel: usize, rate: u32) -> f64 {
    let samples: Vec<f64> = ints(out, 4)
        .into_iter()
        .skip(channel)
        .step_by(2)
        .skip(rate as usize / 2)
        .map(f64::from)
        .collect();
    (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
}

/// How many dB `dsp` changes a sine of `frequency` by, measured.
fn measured(dsp: &Dsp, frequency: f64) -> f64 {
    let rate = 48_000;
    let device = device(&[rate], &[I32]);
    let samples = sine(rate, frequency, 1.5, false);
    let run = |dsp: &Dsp| {
        let download = wav(rate, 2, 24, &samples);
        process(download, Gain::default(), Leveling::Off, dsp, &device, None).0
    };
    20.0 * (rms(&run(dsp), 0, rate) / rms(&run(&Dsp::default()), 0, rate)).log10()
}

/// How many dB `band` changes `frequency` by at `rate`, from the Audio EQ
/// Cookbook's formulas, which FFmpeg's biquads follow.
fn calculated(band: Band, rate: f64, frequency: f64) -> f64 {
    use std::f64::consts::PI;
    let a = 10f64.powf(band.gain / 40.0);
    let (sin, cos) = (2.0 * PI * band.frequency / rate).sin_cos();
    let alpha = sin / (2.0 * band.q);
    let root = 2.0 * a.sqrt() * alpha;
    let (b, d) = match band.filter {
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

#[test]
fn each_eq_band_changes_the_sound_as_calculated() {
    let band = |filter, frequency, gain, q| Band {
        filter,
        frequency,
        gain,
        q,
    };
    let bands = [
        band(Filter::Peak, 1_000.0, 6.0, 1.0),
        band(Filter::LowShelf, 100.0, 4.0, 0.7),
        band(Filter::HighShelf, 8_000.0, -5.0, 0.7),
        band(Filter::LowPass, 2_000.0, 0.0, 0.707),
        band(Filter::HighPass, 200.0, 0.0, 0.707),
    ];
    for band in bands {
        let dsp = Dsp {
            eq: Some(Equalizer {
                preamp: 0.0,
                bands: vec![band],
            }),
            ..Dsp::default()
        };
        for frequency in [50.0, 200.0, 1_000.0, 3_000.0, 10_000.0] {
            let (measured, calculated) = (
                measured(&dsp, frequency),
                calculated(band, 48_000.0, frequency),
            );
            assert!(
                (measured - calculated).abs() < 0.1,
                "{band:?} at {frequency} Hz: measured {measured:.2} dB, calculated {calculated:.2} dB"
            );
        }
    }
}

#[test]
fn headroom_and_the_eqs_preamp_turn_it_down_by_their_db() {
    let dsp = Dsp {
        headroom: -3.0,
        eq: Some(Equalizer {
            preamp: -2.5,
            bands: Vec::new(),
        }),
        ..Dsp::default()
    };
    let change = measured(&dsp, 1_000.0);
    assert!((change + 5.5).abs() < 0.01, "{change:.3} dB");
}

#[test]
fn each_step_is_graded_as_roon_grades_it() {
    let samples = ramp(4_410, 2, 24);
    let dsp = Dsp {
        headroom: -3.0,
        ..Dsp::default()
    };
    let gain = Gain {
        track: Some(-6.0),
        ..Gain::default()
    };
    let (_, decoding) = process(
        wav(44_100, 2, 24, &samples),
        gain,
        Leveling::Track,
        &dsp,
        &device(&[44_100], &[I16]),
        None,
    );
    let path = &decoding.path;
    let graded = |start: &str, quality| {
        path.iter()
            .any(|step| step.text.starts_with(start) && step.quality == quality)
    };
    assert_eq!(path[0].quality, Quality::Lossless, "{path:?}");
    assert!(graded("Volume leveling", Quality::Enhanced), "{path:?}");
    assert!(graded("Headroom", Quality::Enhanced), "{path:?}");
    assert!(
        graded("Reduced from 24 to 16 bits", Quality::High),
        "{path:?}"
    );
}

#[test]
fn crossfeed_lets_one_ear_hear_the_other() {
    let rate = 48_000;
    let device = device(&[rate], &[I32]);
    let samples = sine(rate, 500.0, 1.5, true);
    let run = |crossfeed| {
        let dsp = Dsp {
            crossfeed,
            ..Dsp::default()
        };
        let download = wav(rate, 2, 24, &samples);
        let (out, decoding) = process(
            download,
            Gain::default(),
            Leveling::Off,
            &dsp,
            &device,
            None,
        );
        (rms(&out, 0, rate), rms(&out, 1, rate), decoding.path)
    };
    let (left, right, path) = run(Crossfeed::Off);
    assert_eq!(right, 0.0);
    assert_eq!(path.len(), 1);
    let mut heard = Vec::new();
    for crossfeed in [Crossfeed::Light, Crossfeed::Medium, Crossfeed::Strong] {
        let (_, other, path) = run(crossfeed);
        let db = 20.0 * (other / left).log10();
        assert!((-40.0..-3.0).contains(&db), "{crossfeed:?}: {db:.1} dB");
        assert!(path.iter().any(|step| step.text.starts_with("Crossfeed")));
        heard.push(db);
    }
    assert!(heard.is_sorted(), "stronger is louder: {heard:?}");
}

#[test]
fn upsampling_goes_to_the_highest_whole_multiple_the_device_takes() {
    let samples = ramp(44_100 / 10, 2, 16);
    let open = |rates: &[u32], upsampling| {
        let dsp = Dsp {
            upsampling,
            ..Dsp::default()
        };
        let download = wav(44_100, 2, 16, &samples);
        let device = device(rates, &[I32]);
        let (_, decoding) = process(
            download,
            Gain::default(),
            Leveling::Off,
            &dsp,
            &device,
            None,
        );
        let steps: Vec<(String, Quality)> = decoding
            .path
            .into_iter()
            .map(|step| (step.text, step.quality))
            .collect();
        (decoding.format.rate, steps)
    };
    let dac = [44_100, 88_200, 176_400, 192_000];
    let (rate, steps) = open(&dac, true);
    assert_eq!(rate, 176_400);
    assert!(
        steps.contains(&("Upsampled 44.1 kHz to 176.4 kHz".into(), Quality::Enhanced)),
        "{steps:?}"
    );
    assert_eq!(open(&dac, false).0, 44_100);
    // No multiple of 44.1 kHz: the nearest rate above, as without.
    let (rate, steps) = open(&[48_000, 96_000], true);
    assert_eq!(rate, 48_000);
    assert!(
        steps.contains(&("Resampled 44.1 kHz to 48 kHz".into(), Quality::High)),
        "{steps:?}"
    );
}

#[test]
fn processing_that_changes_nothing_leaves_it_bit_perfect() {
    let samples = ramp(44_100 / 4, 2, 24);
    let device = device(&[44_100], &[I32]);
    let flat = Dsp {
        eq: Some(Equalizer {
            preamp: 0.0,
            bands: vec![
                Band {
                    filter: Filter::Peak,
                    frequency: 1_000.0,
                    gain: 0.0,
                    q: 1.0,
                },
                // Above what 44.1 kHz holds.
                Band {
                    filter: Filter::HighShelf,
                    frequency: 30_000.0,
                    gain: 3.0,
                    q: 0.7,
                },
            ],
        }),
        ..Dsp::default()
    };
    let run = |dsp: &Dsp| {
        let download = wav(44_100, 2, 24, &samples);
        process(download, Gain::default(), Leveling::Off, dsp, &device, None)
    };
    let (plain, _) = run(&Dsp::default());
    let (out, decoding) = run(&flat);
    assert_eq!(out, plain);
    assert!(
        decoding
            .path
            .iter()
            .all(|step| step.quality == Quality::Lossless)
    );
}

#[test]
fn processing_leaves_out_what_changes_nothing_or_cant_be_done() {
    let band = |filter, frequency, gain, q| Band {
        filter,
        frequency,
        gain,
        q,
    };
    let dsp = Dsp {
        headroom: -0.004,
        eq: Some(Equalizer {
            preamp: 0.004,
            bands: vec![
                band(Filter::Peak, 1_000.0, 0.004, 1.0),
                // At what 48 kHz holds, and without a Q.
                band(Filter::Peak, 24_000.0, 3.0, 1.0),
                band(Filter::Peak, 1_000.0, 3.0, 0.0),
                // A pass has no gain to be flat.
                band(Filter::LowPass, 5_000.0, 0.0, 0.7),
            ],
        }),
        crossfeed: Crossfeed::Medium,
        upsampling: false,
    };
    let (filters, path) = dsp.filters(48_000, 2);
    let lowpass = "lowpass=f=5000:t=q:w=0.7:precision=f64";
    assert_eq!(filters, [lowpass, "crossfeed=strength=0.3"]);
    let steps: Vec<&str> = path.iter().map(|step| step.text.as_str()).collect();
    assert_eq!(steps, ["EQ, 1 band", "Crossfeed, medium"]);
    // Mono has no other ear to feed.
    assert_eq!(dsp.filters(48_000, 1).0, [lowpass]);
}

#[test]
fn nothing_is_fetched_without_a_device() {
    let shared = Arc::new(Shared::default());
    shared.ring.lock().unwrap().problem = Some("There is no sound device to play on.".into());
    let fetched = Arc::new(AtomicUsize::new(0));
    let mut decoder = Decoder {
        shared: shared.clone(),
        pending: Arc::default(),
        system: None,
        fetch: Box::new({
            let fetched = fetched.clone();
            move |_: &Track| {
                fetched.fetch_add(1, Relaxed);
                Err("Not here.".into())
            }
        }),
        queue: vec![track(Gain::default()); 3],
        leveling: Leveling::Off,
        dsp: Dsp::default(),
        repeat: Repeat::Off,
        current: None,
        next: None,
        failures: 0,
    };
    decoder.open(0, 0.0, None);
    assert_eq!(fetched.load(Relaxed), 0);
    let ring = shared.ring.lock().unwrap();
    assert!(ring.idle);
    assert_eq!(ring.skipped, None);
}

#[test]
fn a_skipped_track_is_told_of_while_the_one_in_its_place_plays() {
    let mut ring = output::Ring {
        skipped: Some((1, "Gone.".into())),
        ..Default::default()
    };
    assert!(ring.shown_skip().is_some(), "before anything plays");
    for (track, shown) in [(2, true), (3, false)] {
        ring.segments = VecDeque::from([Segment {
            track,
            format: Format {
                rate: 44_100,
                channels: 2,
                sample: I16,
            },
            data: VecDeque::new(),
            ended: false,
            start: 0,
            played: 0,
            path: Vec::new(),
        }]);
        assert_eq!(ring.shown_skip().is_some(), shown, "track {track}");
    }
}

#[test]
fn the_offered_formats_decide_the_format_the_file_is_played_in() {
    let offered = Format {
        rate: 44_100,
        channels: 2,
        sample: I16,
    };
    let download = wav(44_100, 2, 16, &ramp(441, 2, 16));
    let _ = ffmpeg_next::init();
    // A device that takes only 48 kHz would resample, were it not offered.
    let supported = device(&[48_000], &[I16]);
    let open = |offered: &[Format]| {
        let download = download.reader().unwrap();
        Decoding::open(
            download,
            &track(Gain::default()),
            Leveling::Off,
            &Dsp::default(),
            &supported,
            offered,
            || false,
        )
        .unwrap_or_else(|_| panic!("opens"))
        .format
    };
    assert_eq!(open(&[offered]), offered);
    assert_eq!(open(&[]).rate, 48_000);
}

#[test]
fn the_decoder_asks_the_system_what_it_offers() {
    let (mut decoder, shared, _) = decoder_of(&["a"]);
    let offered = Format {
        rate: 44_100,
        channels: 2,
        sample: F32,
    };
    decoder.system = Some(Arc::new(FakeMixer {
        offered: vec![offered],
        ..Default::default()
    }));
    decoder.open(0, 0.0, None);
    assert_eq!(shared.ring.lock().unwrap().segments[0].format, offered);
}

#[test]
fn a_player_asks_its_system_what_it_offers() {
    let system = Arc::new(FakeMixer::default());
    let silence = vec![0; 882];
    let player = Player::with_system(
        None,
        Box::new(move |_| Ok(wav(44_100, 2, 16, &silence))),
        system.clone(),
    );
    // Let the output thread find its device before putting one in its place.
    let started = std::time::Instant::now();
    while {
        let ring = player.shared.ring.lock().unwrap();
        ring.device.is_none() && ring.problem.is_none()
    } {
        assert!(started.elapsed().as_secs() < 10, "the output thread starts");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    {
        let mut ring = player.shared.ring.lock().unwrap();
        ring.problem = None;
        ring.device = Some(output::Device {
            supported: device(&[44_100], &[I16]),
            ..Default::default()
        });
    }
    // Silent, whatever the machine's own device does with it.
    player.set_paused(true);
    player.play(vec![track(Gain::default())], 0, 0.0);
    player.set_paused(true);
    while system.asked.load(Relaxed) == 0 {
        assert!(started.elapsed().as_secs() < 10, "the decoder asks");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn the_output_is_given_back_when_the_player_quits() {
    let system = Arc::new(FakeMixer::default());
    drop(Player::with_system(
        None,
        Box::new(|_| Err(String::new())),
        system.clone(),
    ));
    assert!(system.released.load(Relaxed) >= 1);
}

#[test]
fn the_signal_path_says_who_carries_the_audio() {
    let dac = output::Device {
        name: "DAC".into(),
        ..Default::default()
    };
    let open = Format {
        rate: 96_000,
        channels: 2,
        sample: I16,
    };
    let steps = |route| device_steps(&dac, route, open);
    let [bit_perfect] = &steps(Route::BitPerfect)[..] else {
        panic!("one step");
    };
    assert!(bit_perfect.text.starts_with("Bit-perfect: DAC"));
    assert_eq!(bit_perfect.quality, Quality::Lossless);
    let [shared, mixer] = &steps(Route::Mixed)[..] else {
        panic!("two steps");
    };
    assert!(shared.text.starts_with("Shared: DAC"));
    assert!(mixer.text.contains("Android's mixer"));
    assert_eq!(
        (shared.quality, mixer.quality),
        (Quality::High, Quality::High)
    );
    assert_eq!(steps(Route::Direct).len(), 1);
}

/// A decoder of tracks named `ids`, each a tenth of a second, for a 44.1
/// kHz device; and the ids it fetches, in turn.
fn decoder_of(ids: &[&str]) -> (Decoder, Arc<Shared>, Arc<std::sync::Mutex<Vec<String>>>) {
    let shared = Arc::new(Shared::default());
    shared.ring.lock().unwrap().device = Some(output::Device {
        supported: device(&[44_100], &[I16]),
        ..Default::default()
    });
    let fetched = Arc::new(std::sync::Mutex::new(Vec::new()));
    let samples = ramp(4_410, 2, 16);
    let decoder = Decoder {
        shared: shared.clone(),
        pending: Arc::default(),
        system: None,
        fetch: Box::new({
            let fetched = fetched.clone();
            move |track: &Track| {
                fetched.lock().unwrap().push(track.id.clone());
                Ok(wav(44_100, 2, 16, &samples))
            }
        }),
        queue: ids
            .iter()
            .map(|id| Track {
                id: (*id).into(),
                gain: Gain::default(),
            })
            .collect(),
        leveling: Leveling::Off,
        dsp: Dsp::default(),
        repeat: Repeat::Off,
        current: None,
        next: None,
        failures: 0,
    };
    (decoder, shared, fetched)
}

/// Decodes until `count` tracks are in, or the queue has ended; the ids of
/// the tracks in, in their order.
fn decode_until(decoder: &mut Decoder, shared: &Shared, count: usize) -> Vec<String> {
    while decoder.current.is_some() && shared.ring.lock().unwrap().segments.len() < count {
        decoder.step();
    }
    let ring = shared.ring.lock().unwrap();
    ring.segments
        .iter()
        .map(|segment| decoder.queue[segment.track].id.clone())
        .collect()
}

#[test]
fn a_changed_queue_plays_on_and_follows_its_new_order() {
    let (mut decoder, shared, _) = decoder_of(&["a", "b", "c", "d"]);
    decoder.open(0, 0.0, None);
    assert_eq!(decode_until(&mut decoder, &shared, 3), ["a", "b", "c"]);
    let heard = shared.ring.lock().unwrap().segments[0].data.len();
    // b and c swap, d is taken out.
    let queue = vec![
        decoder.queue[0].clone(),
        decoder.queue[2].clone(),
        decoder.queue[1].clone(),
    ];
    decoder.handle(Command::Requeue(
        queue,
        vec![Some(0), Some(2), Some(1), None],
    ));
    assert_eq!(decode_until(&mut decoder, &shared, 4), ["a", "c", "b"]);
    let ring = shared.ring.lock().unwrap();
    assert_eq!(ring.segments[0].data.len(), heard, "a went on as it was");
    assert_eq!(ring.requeues, 1);
}

#[test]
fn a_queue_without_the_track_heard_plays_the_first_after_it_that_stayed() {
    let (mut decoder, shared, _) = decoder_of(&["a", "b", "c", "d"]);
    decoder.open(1, 0.0, None);
    assert_eq!(decode_until(&mut decoder, &shared, 2), ["b", "c"]);
    // b, heard, and c go.
    let queue = vec![decoder.queue[0].clone(), decoder.queue[3].clone()];
    decoder.handle(Command::Requeue(queue, vec![Some(0), None, None, Some(1)]));
    assert_eq!(decode_until(&mut decoder, &shared, 1), ["d"]);
    decoder.handle(Command::Jump(0));
    assert_eq!(decode_until(&mut decoder, &shared, 1), ["a"]);
    // With none after it, it stops.
    let queue = vec![decoder.queue[1].clone()];
    decoder.handle(Command::Requeue(queue, vec![None, Some(0)]));
    assert_eq!(decode_until(&mut decoder, &shared, 1), ["d"]);
    let queue = Vec::new();
    decoder.handle(Command::Requeue(queue, vec![None]));
    assert!(decoder.current.is_none());
    assert!(shared.ring.lock().unwrap().idle);
}

#[test]
fn repeat_plays_a_track_again_or_the_queue_round() {
    let (mut decoder, shared, fetched) = decoder_of(&["a", "b"]);
    decoder.repeat = Repeat::One;
    decoder.open(0, 0.0, None);
    assert_eq!(decode_until(&mut decoder, &shared, 3), ["a", "a", "a"]);
    assert_eq!(*fetched.lock().unwrap(), ["a"], "read again, not fetched");

    let (mut decoder, shared, _) = decoder_of(&["a", "b"]);
    decoder.repeat = Repeat::All;
    decoder.open(1, 0.0, None);
    assert_eq!(decode_until(&mut decoder, &shared, 3), ["b", "a", "b"]);

    // Turned on while the next track is in already, it takes its place.
    let (mut decoder, shared, _) = decoder_of(&["a", "b"]);
    decoder.open(0, 0.0, None);
    assert_eq!(decode_until(&mut decoder, &shared, 2), ["a", "b"]);
    decoder.handle(Command::Repeat(Repeat::One));
    assert_eq!(decode_until(&mut decoder, &shared, 2), ["a", "a"]);
}

#[test]
fn a_repeating_queue_whose_tracks_all_fail_stops() {
    let (mut decoder, shared, _) = decoder_of(&["a", "b", "c"]);
    let tries = Arc::new(AtomicUsize::new(0));
    decoder.fetch = Box::new({
        let tries = tries.clone();
        move |_: &Track| {
            tries.fetch_add(1, Relaxed);
            Err("Gone.".into())
        }
    });
    decoder.repeat = Repeat::All;
    decoder.open(1, 0.0, None);
    assert_eq!(tries.load(Relaxed), 3);
    assert!(shared.ring.lock().unwrap().idle);
}

/// The loopback card, which one test at a time plays on.
static LOOPBACK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Plays two tracks on ALSA's loopback card, records the other end, and
/// checks that they came out as they went in with nothing between them.
/// Needs `snd-aloop` loaded and access to /dev/snd:
/// `cargo test -p playback -- --ignored`.
#[test]
#[ignore]
fn plays_bit_perfect_and_gapless_through_alsa_loopback() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::Mutex;

    let _card = LOOPBACK.lock().unwrap_or_else(|e| e.into_inner());

    let host = cpal::default_host();
    let card = |pcm: &str| {
        let id: cpal::DeviceId = format!("alsa:{pcm}").parse().unwrap();
        host.device_by_id(&id)
            .unwrap_or_else(|| panic!("no {pcm}: is snd-aloop loaded?"))
    };
    let first = ramp(96_000, 2, 24);
    let second: Vec<i32> = ramp(48_000, 2, 24).iter().map(|s| -s).collect();
    let tracks = [first.clone(), second.clone()];

    // The capture side opens first and fixes the loopback's format. The
    // loopback hands it the player's periods whole, a twentieth of a second
    // each, so its own buffer holds two of them.
    let recorded = Arc::new(Mutex::new(Vec::<i32>::new()));
    let capture = card("hw:CARD=Loopback,DEV=1")
        .build_input_stream::<i32, _, _>(
            cpal::StreamConfig {
                channels: 2,
                sample_rate: 96_000,
                buffer_size: cpal::BufferSize::Fixed(96_000 / 20),
            },
            {
                let recorded = recorded.clone();
                move |data, _| recorded.lock().unwrap().extend_from_slice(data)
            },
            |e| panic!("capture failed: {e}"),
            None,
        )
        .expect("capture opens at 96 kHz 32-bit");
    capture.play().unwrap();

    let fetch: Fetch = Box::new(move |track: &Track| {
        let index: usize = track.id.parse().unwrap();
        Ok(wav(96_000, 2, 24, &tracks[index]))
    });
    let player = Player::new(Some("alsa:hw:CARD=Loopback,DEV=0".into()), fetch);
    let queue = (0..2)
        .map(|index| Track {
            id: index.to_string(),
            gain: Gain::default(),
        })
        .collect();
    player.play(queue, 0, 0.0);
    let started = std::time::Instant::now();
    let mut path = Vec::new();
    while !player.status().ended {
        let status = player.status();
        assert_eq!(status.problem, None);
        if status.index.is_some() && !status.path.is_empty() {
            path = status.path.clone();
        }
        assert!(
            started.elapsed().as_secs() < 20,
            "still playing: {status:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    drop(capture);

    assert!(
        path.iter().all(|step| step.quality == Quality::Lossless),
        "{path:?}"
    );
    assert!(
        path.last().unwrap().text.starts_with("Exclusive"),
        "{path:?}"
    );
    let recorded = recorded.lock().unwrap();
    let expected: Vec<i32> = first.iter().chain(&second).map(|s| s << 8).collect();
    let start = recorded
        .iter()
        .position(|&sample| sample != 0)
        .expect("something was recorded");
    // The ramp starts on a negative sample, so the first one recorded that
    // isn't silence is the first one played.
    assert!(
        recorded.len() >= start + expected.len(),
        "recorded too little"
    );
    let played = &recorded[start..start + expected.len()];
    if let Some(at) = played.iter().zip(&expected).position(|(a, b)| a != b) {
        let around = at.saturating_sub(4)..(at + 4).min(expected.len());
        panic!(
            "sample {at} of {} differs: recorded {:?}, played {:?}",
            expected.len(),
            &played[around.clone()],
            &expected[around]
        );
    }
}

/// Plays a 96 kHz track and then a 44.1 kHz one on ALSA's loopback card and
/// checks that the device was opened again in each track's own rate. Needs
/// what the test above needs.
#[test]
#[ignore]
fn opens_the_device_in_each_tracks_own_rate() {
    let _card = LOOPBACK.lock().unwrap_or_else(|e| e.into_inner());
    let rates = [96_000, 44_100];
    let fetch: Fetch = Box::new(move |track: &Track| {
        let rate = rates[track.id.parse::<usize>().unwrap()];
        Ok(wav(rate, 2, 16, &ramp(rate as usize / 4, 2, 16)))
    });
    let player = Player::new(Some("alsa:hw:CARD=Loopback,DEV=0".into()), fetch);
    let queue = (0..2)
        .map(|index| Track {
            id: index.to_string(),
            gain: Gain::default(),
        })
        .collect();
    player.play(queue, 0, 0.0);
    let started = std::time::Instant::now();
    let mut seen = Vec::new();
    while !player.status().ended {
        let status = player.status();
        assert_eq!(status.problem, None);
        if let (Some(index), Some(device)) = (status.index, status.path.last())
            && status.quality == Some(Quality::Lossless)
            && !seen.contains(&(index, device.text.clone()))
        {
            seen.push((index, device.text.clone()));
        }
        assert!(
            started.elapsed().as_secs() < 20,
            "still playing: {status:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let opened: Vec<_> = seen
        .iter()
        .map(|(index, text)| {
            (
                *index,
                text.contains(&format!("({} Hz 16-bit", rates[*index])),
            )
        })
        .collect();
    assert_eq!(opened, [(0, true), (1, true)], "{seen:?}");
}
