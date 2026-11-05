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
    let file = wav_bytes(rate, channels, bits, samples);
    let folder = std::env::temp_dir().join("playback-tests");
    let (download, mut fill) = download::new(&folder, Some(file.len() as u64)).unwrap();
    assert!(fill.append(&file));
    fill.finish();
    download
}

fn wav_bytes(rate: u32, channels: u16, bits: u16, samples: &[i32]) -> Vec<u8> {
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
    file
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
fn a_file_on_disk_decodes_bit_perfect() {
    let samples = ramp(4_410, 2, 16);
    let folder = std::env::temp_dir().join(format!("playback-open-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("song.wav");
    std::fs::write(&path, wav_bytes(44_100, 2, 16, &samples)).unwrap();
    let (out, _) = decode(
        open_file(&path).unwrap(),
        Gain::default(),
        Leveling::Off,
        &device(&[44_100], &[I16]),
        None,
    );
    std::fs::remove_dir_all(&folder).unwrap();
    assert_eq!(ints(&out, 2), samples);
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
                band.response(48_000.0, frequency),
            );
            assert!(
                (measured - calculated).abs() < 0.1,
                "{band:?} at {frequency} Hz: measured {measured:.2} dB, calculated {calculated:.2} dB"
            );
        }
    }
}

#[test]
fn a_bands_response_reaches_its_gain_where_it_should() {
    let band = |filter, frequency, gain| Band {
        filter,
        frequency,
        gain,
        q: 0.707,
    };
    let at = |band: Band, frequency| band.response(48_000.0, frequency);
    assert!((at(band(Filter::Peak, 1_000.0, 6.0), 1_000.0) - 6.0).abs() < 1e-9);
    assert!((at(band(Filter::LowShelf, 200.0, -4.0), 20.0) + 4.0).abs() < 0.05);
    assert!((at(band(Filter::HighShelf, 2_000.0, 5.0), 20_000.0) - 5.0).abs() < 0.5);
    for filter in [Filter::LowPass, Filter::HighPass] {
        let corner = at(band(filter, 1_000.0, 9.0), 1_000.0);
        assert!((corner + 3.01).abs() < 0.01, "{filter:?} {corner:.3} dB");
    }
    assert_eq!(at(band(Filter::Peak, 30_000.0, 6.0), 1_000.0), 0.0);
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
            move |_: &Track, _: f64| {
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
        failed_from: (0, 0.0),
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
        Box::new(move |_, _| Ok(wav(44_100, 2, 16, &silence))),
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
        Box::new(|_, _| Err(String::new())),
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
            move |track: &Track, _: f64| {
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
        failed_from: (0, 0.0),
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
fn skips_faster_than_tracks_open_leave_only_the_last_ones_audio() {
    let (mut decoder, shared, _) = decoder_of(&["a", "b", "c", "d", "e"]);
    decoder.open(0, 0.0, None);
    assert_eq!(decode_until(&mut decoder, &shared, 1), ["a"]);
    for _ in 0..3 {
        decoder.handle(Command::Next);
    }
    assert_eq!(decode_until(&mut decoder, &shared, 2), ["d", "e"]);
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
        move |_: &Track, _: f64| {
            tries.fetch_add(1, Relaxed);
            Err("Gone.".into())
        }
    });
    decoder.repeat = Repeat::All;
    decoder.open(1, 0.0, None);
    assert_eq!(tries.load(Relaxed), 3);
    assert!(shared.ring.lock().unwrap().idle);
}

#[test]
fn a_queue_stopped_short_says_where_and_one_played_to_its_end_does_not() {
    let (mut decoder, shared, _) = decoder_of(&["a", "b", "c"]);
    let samples = ramp(4_410, 2, 16);
    decoder.fetch = Box::new(move |track: &Track, _: f64| match track.id.as_str() {
        "a" => Ok(wav(44_100, 2, 16, &samples)),
        _ => Err("Gone.".into()),
    });
    decoder.open(0, 0.0, None);
    assert_eq!(decode_until(&mut decoder, &shared, 3), ["a"]);
    let stopped = shared.ring.lock().unwrap().stopped;
    assert_eq!(stopped, Some((1, 0.0)), "at the first that failed");

    let (mut decoder, shared, _) = decoder_of(&["a", "b"]);
    decoder.open(0, 0.0, None);
    assert_eq!(decode_until(&mut decoder, &shared, 3), ["a", "b"]);
    assert_eq!(shared.ring.lock().unwrap().stopped, None);
}

#[test]
fn a_track_begun_part_way_is_read_on_once_in_and_fetched_again_from_before_it() {
    let (mut decoder, _, fetched) = decoder_of(&["a"]);
    decoder.open(0, 0.0, None);
    decoder.start(0, 0.05);
    assert_eq!(
        *fetched.lock().unwrap(),
        ["a"],
        "a whole file is read again"
    );

    let (mut decoder, shared, _) = decoder_of(&["a", "b"]);
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let samples = ramp(4_410, 2, 16);
    decoder.fetch = Box::new({
        let (asked, samples) = (asked.clone(), samples.clone());
        move |track: &Track, seconds: f64| {
            asked
                .lock()
                .unwrap()
                .push(format!("{}@{seconds}", track.id));
            Ok(wav(44_100, 2, 16, &samples).starting_at(seconds))
        }
    });
    decoder.open(0, 0.02, None);
    // a has all arrived, so b is fetched ahead.
    decoder.step();
    decoder.start(0, 0.05);
    while shared.ring.lock().unwrap().segments[0].data.is_empty() {
        decoder.step();
    }
    {
        let ring = shared.ring.lock().unwrap();
        assert_eq!(ring.segments[0].start, 2_205);
        // 0.05 s into the track is 0.03 s into a download that began at 0.02.
        let bytes: Vec<u8> = ring.segments[0].data.iter().copied().collect();
        let heard = ints(&bytes, 2);
        assert_eq!(heard, samples[1_323 * 2..][..heard.len()]);
    }
    decoder.start(0, 0.01);
    assert_eq!(decode_until(&mut decoder, &shared, 2), ["a", "b"]);
    assert_eq!(*asked.lock().unwrap(), ["a@0.02", "b@0", "a@0.01"]);
}

#[test]
fn a_track_begun_part_way_is_fetched_again_to_repeat_it_from_its_start() {
    let (mut decoder, shared, _) = decoder_of(&["a"]);
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let samples = ramp(4_410, 2, 16);
    decoder.fetch = Box::new({
        let asked = asked.clone();
        move |_: &Track, seconds: f64| {
            asked.lock().unwrap().push(seconds);
            Ok(wav(44_100, 2, 16, &samples).starting_at(seconds))
        }
    });
    decoder.repeat = Repeat::One;
    decoder.open(0, 0.05, None);
    decode_until(&mut decoder, &shared, 2);
    assert_eq!(*asked.lock().unwrap(), [0.05, 0.0]);
    assert_eq!(shared.ring.lock().unwrap().segments[1].start, 0);
}

#[test]
fn a_seek_into_a_download_of_unknown_length_does_not_wait_for_the_rest() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let (mut decoder, shared, _) = decoder_of(&["a"]);
    let sought = Arc::new(AtomicBool::new(false));
    let waited_in_vain = Arc::new(AtomicBool::new(false));
    decoder.fetch = Box::new({
        let (sought, waited_in_vain) = (sought.clone(), waited_in_vain.clone());
        move |_: &Track, seconds: f64| {
            let whole = silent_mp3(400);
            let (download, mut fill) =
                download::new(&std::env::temp_dir().join("playback-tests"), None).unwrap();
            if seconds > 0.0 {
                fill.append(&whole);
                fill.finish();
                return Ok(download.starting_at(seconds));
            }
            let (sought, waited_in_vain) = (sought.clone(), waited_in_vain.clone());
            std::thread::spawn(move || {
                let (first, rest) = whole.split_at(whole.len() * 3 / 4);
                fill.append(first);
                let patience = std::time::Instant::now();
                while !sought.load(SeqCst) {
                    if patience.elapsed() > std::time::Duration::from_secs(10) {
                        waited_in_vain.store(true, SeqCst);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                fill.append(rest);
                fill.finish();
            });
            Ok(download.starting_at(0.0))
        }
    });
    decoder.open(0, 0.0, None);
    decoder.handle(Command::Seek(5.0));
    sought.store(true, SeqCst);
    assert!(
        !waited_in_vain.load(SeqCst),
        "the seek waited for the whole download"
    );
    let ring = shared.ring.lock().unwrap();
    assert_eq!(
        ring.segments[0].start,
        5 * 44_100,
        "the track's own position"
    );
}

const PACKET_BYTES: usize = 188;
const PMT_PID: u16 = 0x100;
const AUDIO_PID: u16 = 0x101;
/// What a transport stream's program map calls MP3, AAC in ADTS frames and
/// AAC in LATM.
const MPEG_AUDIO_STREAM_TYPE: u8 = 0x03;
const ADTS_STREAM_TYPE: u8 = 0x0F;
const LATM_STREAM_TYPE: u8 = 0x11;
/// A transport stream counts 90 kHz ticks. A layer 3 frame is 1152 samples
/// at 44.1 kHz, an AAC frame 1024.
const TICKS_PER_FRAME: u64 = 1152 * 90_000 / 44_100;
const TICKS_PER_AAC_FRAME: u64 = 1024 * 90_000 / 44_100;

fn mpeg_crc(bytes: &[u8]) -> u32 {
    bytes.iter().fold(u32::MAX, |mut crc, &byte| {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 1 << 31 != 0 {
                crc << 1 ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
        crc
    })
}

/// One 188-byte packet of `pid` carrying `payload`, padded with stuffing
/// in its adaptation field when short.
fn ts_packet(pid: u16, starts_unit: bool, counter: u8, payload: &[u8]) -> Vec<u8> {
    let room = PACKET_BYTES - 4;
    let mut packet = vec![
        0x47,
        (u8::from(starts_unit) << 6) | (pid >> 8) as u8,
        pid as u8,
    ];
    if payload.len() == room {
        packet.push(0x10 | counter);
    } else {
        packet.push(0x30 | counter);
        let stuffing = room - payload.len();
        packet.push((stuffing - 1) as u8);
        if stuffing > 1 {
            packet.push(0);
            packet.resize(packet.len() + stuffing - 2, 0xFF);
        }
    }
    packet.extend_from_slice(payload);
    packet
}

/// A table section as a transport stream carries it: after a pointer, with
/// its length and CRC.
fn ts_section(table_id: u8, body: &[u8]) -> Vec<u8> {
    let length = body.len() + 4;
    let mut section = vec![table_id, 0xB0 | (length >> 8) as u8, length as u8];
    section.extend_from_slice(body);
    section.extend_from_slice(&mpeg_crc(&section).to_be_bytes());
    let mut payload = vec![0];
    payload.extend(section);
    payload
}

/// `frames` silent MP3 frames as an MPEG transport stream segment, as an
/// HLS station serves them, starting `first_frame` frames into the stream.
fn ts_segment(first_frame: u64, frames: usize) -> Vec<u8> {
    transport_stream(
        MPEG_AUDIO_STREAM_TYPE,
        &[silent_mp3(frames)],
        first_frame * TICKS_PER_FRAME,
        0,
    )
}

/// Each of `audio` of `stream_type` as a packet of one transport stream
/// segment, the first due at `ticks` and each `ticks_between` after the one
/// before.
fn transport_stream(stream_type: u8, audio: &[Vec<u8>], ticks: u64, ticks_between: u64) -> Vec<u8> {
    let mut counters = [0u8; 3];
    let mut out = Vec::new();
    let mut put = |out: &mut Vec<u8>, which: usize, pid, first, payload: &[u8]| {
        out.extend(ts_packet(pid, first, counters[which], payload));
        counters[which] = (counters[which] + 1) % 16;
    };
    let pat = ts_section(
        0,
        &[
            0,
            1,
            0xC1,
            0,
            0,
            0,
            1,
            0xE0 | (PMT_PID >> 8) as u8,
            PMT_PID as u8,
        ],
    );
    put(&mut out, 0, 0, true, &pat);
    let pmt = ts_section(
        2,
        &[
            0,
            1,
            0xC1,
            0,
            0,
            0xE0 | (AUDIO_PID >> 8) as u8,
            AUDIO_PID as u8,
            0xF0,
            0,
            stream_type,
            0xE0 | (AUDIO_PID >> 8) as u8,
            AUDIO_PID as u8,
            0xF0,
            0,
        ],
    );
    put(&mut out, 1, PMT_PID, true, &pmt);
    for (n, audio) in audio.iter().enumerate() {
        let ticks = ticks + n as u64 * ticks_between;
        let mut pes = vec![0, 0, 1, 0xC0];
        pes.extend(((audio.len() + 8) as u16).to_be_bytes());
        pes.extend([0x80, 0x80, 5]);
        pes.extend([
            0x21 | ((ticks >> 29) & 0x0E) as u8,
            (ticks >> 22) as u8,
            0x01 | ((ticks >> 14) & 0xFE) as u8,
            (ticks >> 7) as u8,
            0x01 | ((ticks << 1) & 0xFE) as u8,
        ]);
        pes.extend(audio);
        for (n, chunk) in pes.chunks(PACKET_BYTES - 4).enumerate() {
            put(&mut out, 2, AUDIO_PID, n == 0, chunk);
        }
    }
    out
}

/// The raw data block of one silent AAC-LC mono frame: a single channel
/// element with no bands coded.
const SILENT_AAC_BLOCK: [u8; 4] = [0x00, 0x00, 0x00, 0x07];

/// `frames` silent AAC-LC frames of 44.1 kHz mono, each in an ADTS header.
fn silent_adts(frames: usize) -> Vec<u8> {
    const HEADER_BYTES: usize = 7;
    let length = HEADER_BYTES + SILENT_AAC_BLOCK.len();
    let mut frame = vec![
        0xFF,
        0xF1,
        // AAC-LC, 44.1 kHz, then the top bit of one channel.
        0x50,
        0x40 | (length >> 11) as u8,
        (length >> 3) as u8,
        ((length & 7) << 5) as u8 | 0x1F,
        0xFC,
    ];
    frame.extend(SILENT_AAC_BLOCK);
    frame.repeat(frames)
}

/// Bits written one after the other, the last byte padded with zeros.
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    used: usize,
}

impl Bits {
    fn put(&mut self, count: usize, value: u32) {
        for bit in (0..count).rev() {
            if self.used.is_multiple_of(8) {
                self.bytes.push(0);
            }
            *self.bytes.last_mut().unwrap() |= (((value >> bit) & 1) as u8) << (7 - self.used % 8);
            self.used += 1;
        }
    }
}

/// `frames` silent AAC-LC frames as LATM in LOAS, each with its stream
/// configuration, as a station that sends AAC this way does.
fn silent_latm(frames: usize) -> Vec<u8> {
    let mut mux = Bits::default();
    mux.put(1, 0); // the configuration follows
    mux.put(1, 0); // audio mux version
    mux.put(1, 1); // all streams have the same time framing
    mux.put(6, 0); // one sub-frame
    mux.put(4, 0); // one program
    mux.put(3, 0); // one layer
    mux.put(5, 2); // AAC-LC
    mux.put(4, 4); // 44.1 kHz
    mux.put(4, 1); // one channel
    mux.put(3, 0); // GASpecificConfig: no frame length flag, core coder or extension
    mux.put(3, 0); // frame length type: variable
    mux.put(8, 0xFF); // buffer fullness: variable
    mux.put(2, 0); // no other data, no CRC
    mux.put(8, SILENT_AAC_BLOCK.len() as u32);
    for byte in SILENT_AAC_BLOCK {
        mux.put(8, u32::from(byte));
    }
    let mut frame = Bits::default();
    frame.put(11, 0x2B7); // the LOAS sync word
    frame.put(13, mux.bytes.len() as u32);
    frame.bytes.extend(mux.bytes);
    frame.bytes.repeat(frames)
}

/// The 16-bit stereo frames a decoder makes of everything that came out.
fn frames_of(out: &[u8]) -> usize {
    out.len() / 4
}

#[test]
fn a_live_stream_of_transport_stream_segments_decodes_as_one_stream() {
    let segments = [ts_segment(0, 12), ts_segment(12, 12), ts_segment(24, 12)];
    let (download, mut fill) =
        download::live(&std::env::temp_dir().join("playback-tests"), 1 << 20).unwrap();
    // Each segment as it was fetched, one after the other.
    for segment in &segments {
        assert!(fill.append(segment));
    }
    fill.finish();
    let (out, decoding) = decode(
        download,
        Gain::default(),
        Leveling::Off,
        &device(&[44_100], &[I16]),
        None,
    );
    assert!(decoding.live);
    assert_eq!(decoding.format.rate, 44_100);
    // 36 frames of 1152 samples; the decoder holds back a frame or two at
    // either end.
    let decoded = frames_of(&out);
    assert!(
        (34 * 1152..=36 * 1152).contains(&decoded),
        "{decoded} frames of {}",
        36 * 1152
    );
    let path: Vec<&str> = decoding
        .path
        .iter()
        .map(|step| step.text.as_str())
        .collect();
    assert_eq!(path[0], "MP3 44.1 kHz");
    assert_eq!(decoding.path[0].quality, Quality::Low);
}

#[test]
fn a_live_stream_of_aac_in_a_transport_stream_decodes() {
    type Silence = fn(usize) -> Vec<u8>;
    let streams: [(u8, Silence); 2] = [
        (ADTS_STREAM_TYPE, silent_adts),
        (LATM_STREAM_TYPE, silent_latm),
    ];
    for (stream_type, silent) in streams {
        let (download, mut fill) =
            download::live(&std::env::temp_dir().join("playback-tests"), 1 << 20).unwrap();
        // A packet of its own for each frame, as stations send AAC.
        let packets: Vec<Vec<u8>> = (0..30).map(|_| silent(1)).collect();
        let segment = transport_stream(stream_type, &packets, 0, TICKS_PER_AAC_FRAME);
        assert!(fill.append(&segment));
        fill.finish();
        let (out, decoding) = decode(
            download,
            Gain::default(),
            Leveling::Off,
            &device(&[44_100], &[I16]),
            None,
        );
        assert!(decoding.live);
        assert_eq!(decoding.format.rate, 44_100, "type {stream_type:#x}");
        let decoded = frames_of(&out);
        assert!(
            (29 * 1024..=30 * 1024).contains(&decoded),
            "type {stream_type:#x}: {decoded} frames of {}",
            30 * 1024
        );
        assert!(
            decoding.path[0].text.starts_with("AAC"),
            "{:?}",
            decoding.path
        );
    }
}

#[test]
fn why_a_live_stream_broke_off_is_what_the_player_says() {
    let (mut decoder, shared, _) = decoder_of(&["station"]);
    decoder.fetch = Box::new(|_: &Track, _: f64| {
        let (download, mut fill) =
            download::live(&std::env::temp_dir().join("playback-tests"), 1 << 20).unwrap();
        assert!(fill.append(&ts_segment(0, 12)));
        fill.fail("The station stopped answering: no more.".into());
        Ok(download)
    });
    decoder.open(0, 0.0, None);
    decode_until(&mut decoder, &shared, 2);
    assert_eq!(
        shared.ring.lock().unwrap().skipped,
        Some((0, "The station stopped answering: no more.".into()))
    );
}

#[test]
fn the_end_of_a_download_that_failed_is_not_the_end_of_the_track() {
    let (download, mut fill) =
        download::live(&std::env::temp_dir().join("playback-tests"), 1 << 20).unwrap();
    assert!(fill.append(&silent_mp3(400)));
    let _ = ffmpeg_next::init();
    let decoding = Decoding::open(
        download,
        &track(Gain::default()),
        Leveling::Off,
        &Dsp::default(),
        &device(&[44_100], &[I16]),
        &[],
        || false,
    )
    .unwrap_or_else(|_| panic!("opens"));
    let end = ffmpeg_next::Error::Eof;
    assert!(decoding.is_end_of_track(&end));
    fill.fail("The station stopped answering: cut off.".into());
    assert!(
        !decoding.is_end_of_track(&end),
        "FFmpeg may call a read that failed the end"
    );
    assert!(!decoding.is_end_of_track(&ffmpeg_next::Error::InvalidData));
}

#[test]
fn a_live_stream_is_joined_where_it_is_and_fetched_again_rather_than_sought() {
    let (mut decoder, shared, _) = decoder_of(&["station"]);
    let fetched = Arc::new(std::sync::Mutex::new(Vec::new()));
    decoder.fetch = Box::new({
        let fetched = fetched.clone();
        move |_: &Track, seconds: f64| {
            fetched.lock().unwrap().push(seconds);
            let (download, mut fill) =
                download::live(&std::env::temp_dir().join("playback-tests"), 1 << 20).unwrap();
            assert!(fill.append(&ts_segment(0, 40)));
            fill.finish();
            Ok(download)
        }
    });
    decoder.open(0, 0.0, None);
    decode_until(&mut decoder, &shared, 1);
    shared.ring.lock().unwrap().segments[0].played = 5 * 44_100;
    // A change of sound processing opens what plays again, where it is.
    decoder.handle(Command::Dsp(Dsp {
        headroom: 3.0,
        ..Dsp::default()
    }));
    assert_eq!(*fetched.lock().unwrap(), [0.0, 5.0], "fetched again");
    let ring = shared.ring.lock().unwrap();
    assert_eq!(
        ring.segments[0].start, 0,
        "it has no place of its own to seek to"
    );
}

#[test]
fn only_a_download_that_is_not_live_plays_on_from_where_it_was() {
    let folder = std::env::temp_dir().join("playback-tests");
    let (whole, mut fill) = download::new(&folder, Some(1)).unwrap();
    fill.append(b"x");
    fill.finish();
    assert!(plays_from(&whole, 0.0));
    let (live, _fill) = download::live(&folder, 16).unwrap();
    assert!(!plays_from(&live, 0.0));
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

    let fetch: Fetch = Box::new(move |track: &Track, _: f64| {
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
    let fetch: Fetch = Box::new(move |track: &Track, _: f64| {
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
