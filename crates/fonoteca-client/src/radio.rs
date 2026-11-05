//! A radio station for the player: its stream fetched by [`radiostream`]
//! over the app's own HTTPS client into a live download, which the player
//! reads like any track.

use std::path::PathBuf;

use playback::{Fetch, Fill};
use tokio::runtime::Handle;

/// How much of a live stream is kept for the player: about two minutes of
/// the usual 128 kbit/s, more than it is ever behind, and all the disk a
/// station takes, however long it plays.
const LIVE_BYTES: u64 = 2 << 20;

/// A station to play: its name, and the address of its stream, which may be
/// an HLS playlist.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Station {
    pub name: String,
    pub stream: String,
}

/// The player's one track for a station.
pub(crate) fn track(station: &Station) -> playback::Track {
    playback::Track {
        id: station.stream.clone(),
        gain: playback::Gain::default(),
    }
}

/// Fetches `stream` whatever track is asked for, from where it is now: a
/// live stream has nowhere else. The fetching stops by itself once the player
/// lets go of the download. Runs on the player's thread, and every request
/// goes out on the runtime: certificate checks (Android's revocation lookup)
/// and connecting happen inside the request, which must not block the UI.
pub(crate) fn fetch(stream: String, folder: PathBuf, user_agent: &'static str) -> Fetch {
    let runtime = Handle::current();
    Box::new(move |_, _| {
        let (download, fill) = playback::live_download(&folder, LIVE_BYTES)
            .map_err(|e| format!("The station couldn't be kept on disk: {e}"))?;
        let radio = radiostream::Radio::new(user_agent)
            .map_err(|e| format!("The station couldn't be reached: {e}"))?;
        let stream = stream.clone();
        runtime.spawn(async move {
            let mut sink = FillSink(fill);
            match radio.play(&stream, &mut sink).await {
                Ok(()) => sink.0.finish(),
                Err(e) => sink.0.fail(e.to_string()),
            }
        });
        Ok(download)
    })
}

/// The player's end of a station's bytes.
struct FillSink(Fill);

impl radiostream::Sink for FillSink {
    fn accept(&mut self, bytes: &[u8]) -> bool {
        self.0.append(bytes)
    }

    fn wanted(&self) -> bool {
        self.0.wanted()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    use super::*;

    /// A local station: answers every request with `reply`, whatever the
    /// path, and counts them.
    fn serve(
        reply: fn(&str) -> (String, Vec<u8>),
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut head = [0; 1024];
                let read = stream.read(&mut head).unwrap();
                let head = String::from_utf8_lossy(&head[..read]).into_owned();
                let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                log.lock().unwrap().push(path.clone());
                let (content_type, body) = reply(&path);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(&body);
            }
        });
        (address, asked)
    }

    fn folder() -> PathBuf {
        std::env::temp_dir().join("fonoteca-client-radio-tests")
    }

    fn read_all(download: &mut playback::Download) -> Vec<u8> {
        let mut all = Vec::new();
        let mut buf = [0; 256];
        loop {
            match download.read(&mut buf) {
                Ok(0) => return all,
                Ok(read) => all.extend_from_slice(&buf[..read]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => panic!("{e}"),
            }
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn a_stations_stream_arrives_in_the_download_the_player_reads() {
        let (server, asked) = serve(|_| ("audio/aac".into(), b"stream bytes".to_vec()));
        let runtime = runtime();
        let _inside = runtime.enter();
        let mut fetch = fetch(format!("{server}/live.aac"), folder(), "test");
        let station = Station {
            name: "Test FM".into(),
            stream: String::new(),
        };
        let mut download = fetch(&track(&station), 0.0).unwrap();
        assert!(download.is_live());
        let started = Instant::now();
        let mut all = Vec::new();
        let mut buf = [0; 256];
        while all.len() < b"stream bytes".len() {
            match download.read(&mut buf) {
                Ok(read) => all.extend_from_slice(&buf[..read]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => panic!("{e}"),
            }
            assert!(started.elapsed() < Duration::from_secs(10));
        }
        assert_eq!(&all[..12], b"stream bytes");
        assert_eq!(asked.lock().unwrap()[0], "/live.aac");
    }

    #[test]
    fn an_hls_station_reaches_the_player_as_its_segments_in_order() {
        let (server, asked) = serve(|path| {
            match path {
            "/live.m3u8" => (
                "application/vnd.apple.mpegurl".into(),
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\none.ts\n#EXTINF:1,\ntwo.ts\n#EXT-X-ENDLIST\n"
                    .to_vec(),
            ),
            path => ("video/mp2t".into(), path.as_bytes().to_vec()),
        }
        });
        let runtime = runtime();
        let _inside = runtime.enter();
        let mut fetch = fetch(format!("{server}/live.m3u8"), folder(), "test");
        let station = Station {
            name: "Test FM".into(),
            stream: String::new(),
        };
        let mut download = fetch(&track(&station), 0.0).unwrap();
        // The playlist ends, so the download does too.
        assert_eq!(read_all(&mut download), b"/one.ts/two.ts");
        assert_eq!(asked.lock().unwrap().len(), 3);
    }

    #[test]
    fn letting_go_of_the_download_stops_the_fetching() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (closed, closed_at) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = [0; 1024];
            let _ = stream.read(&mut head).unwrap();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/aac\r\n\r\n");
            // A stream that never ends, until the other side hangs up.
            while stream.write_all(&[0; 1024]).is_ok() {
                std::thread::sleep(Duration::from_millis(10));
            }
            closed.send(()).unwrap();
        });
        let runtime = runtime();
        let _inside = runtime.enter();
        let mut fetch = fetch(format!("{address}/live.aac"), folder(), "test");
        let station = Station {
            name: "Test FM".into(),
            stream: String::new(),
        };
        let mut download = fetch(&track(&station), 0.0).unwrap();
        let mut buf = [0; 256];
        while download.read(&mut buf).is_err() {}
        drop(download);
        closed_at
            .recv_timeout(Duration::from_secs(10))
            .expect("the connection was closed");
    }

    #[test]
    fn the_player_is_told_to_stop_wanting_a_station_once_it_lets_go() {
        use radiostream::Sink;
        let (download, fill) = playback::live_download(&folder(), 1024).unwrap();
        let sink = FillSink(fill);
        assert!(sink.wanted());
        drop(download);
        assert!(!sink.wanted());
    }

    #[test]
    fn a_station_that_cannot_be_reached_fails_the_download_with_the_reason() {
        let runtime = runtime();
        let _inside = runtime.enter();
        let mut fetch = fetch("not an address".into(), folder(), "test");
        let station = Station {
            name: "Test FM".into(),
            stream: String::new(),
        };
        let mut download = fetch(&track(&station), 0.0).unwrap();
        let error = loop {
            match download.read(&mut [0; 16]) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => break e,
                Ok(_) => panic!("nothing was there to read"),
            }
        };
        assert!(error.to_string().contains("address"), "{error}");
    }
}
