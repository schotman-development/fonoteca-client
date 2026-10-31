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
    let _ = ffmpeg_next::init();
    let mut decoding = Decoding::open(download, &track(gain), leveling, supported, || false)
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
        decoding.path.iter().all(|step| step.lossless),
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
    assert!(decoding.path.iter().all(|step| step.lossless));
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
    assert!(decoding.path.iter().all(|step| step.lossless));

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
        decoding
            .path
            .iter()
            .any(|step| step.text == "Resampled 44.1 kHz to 48 kHz" && !step.lossless)
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
            .any(|step| step.text == "1 channels played on 2" && !step.lossless),
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
        &device(&[44_100], &[I16]),
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
    player.play(queue, 0);
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

    assert!(path.iter().all(|step| step.lossless), "{path:?}");
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
    player.play(queue, 0);
    let started = std::time::Instant::now();
    let mut seen = Vec::new();
    while !player.status().ended {
        let status = player.status();
        assert_eq!(status.problem, None);
        if let (Some(index), Some(device)) = (status.index, status.path.last())
            && status.bit_perfect
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
