//! The session against a real receiver, shairport-sync, whose `stdout`
//! backend writes what it plays (48 kHz, 32-bit, stereo, after its own
//! volume and resampling) to a file, to be checked against [`sample`]: the
//! track's ramp comes out whole, 3 seconds long, with the right channel the
//! left's opposite. Start it first:
//! `podman run --rm --network host --entrypoint shairport-sync
//! docker.io/mikebrady/shairport-sync --service-type=classic --mdns=metadata
//! -o stdout > played.raw`, then
//! `SHAIRPORT_ADDRESS=127.0.0.1:5000 cargo test -p network-output --test
//! shairport_sync -- --ignored`.

use std::io::Read;
use std::net::SocketAddrV4;
use std::sync::Arc;
use std::time::{Duration, Instant};

use network_output::{Device, Media, OpenPcm, OutputState, Protocol};

const FRAMES: u64 = 44_100 * 3;

/// Frame `frame` of the track: a counter in both channels, easy to find in
/// what the receiver wrote.
fn sample(frame: u64, channel: u64) -> i16 {
    (frame % 30_000) as i16 * if channel == 0 { 1 } else { -1 }
}

struct Track(u64);

impl Read for Track {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while self.0 < FRAMES && buf.len() - written >= 4 {
            for channel in 0..2 {
                buf[written..written + 2].copy_from_slice(&sample(self.0, channel).to_ne_bytes());
                written += 2;
            }
            self.0 += 1;
        }
        Ok(written)
    }
}

#[test]
#[ignore = "needs shairport-sync, at SHAIRPORT_ADDRESS"]
fn a_track_is_played_to_shairport_sync_to_its_end() {
    let address: SocketAddrV4 = std::env::var("SHAIRPORT_ADDRESS")
        .expect("the address of shairport-sync")
        .parse()
        .unwrap();
    let device = Device {
        protocol: Protocol::AirPlay,
        id: "A1B2C3D4E5F6".into(),
        name: "shairport-sync".into(),
        model: None,
        address: *address.ip(),
        port: address.port(),
    };
    let pcm: OpenPcm = Arc::new(|_, start| Ok(Box::new(Track((start * 44_100.0) as u64))));
    let mut output = device.connect(pcm);
    output.load(Media {
        url: "http://unused/".into(),
        path: "unused".into(),
        content_type: "audio/flac".into(),
        start: 0.0,
        paused: false,
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = output.status().expect("the receiver stays available");
        if status.state == OutputState::Finished {
            return;
        }
        assert!(Instant::now() < deadline, "never finished: {status:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}
