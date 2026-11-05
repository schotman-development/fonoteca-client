//! The reader against stand-in stations on local sockets.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::*;

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

fn ok(content_type: &'static str, body: impl Into<Vec<u8>>) -> Option<Reply> {
    Some(Reply {
        status: 200,
        content_type,
        body: body.into(),
    })
}

fn playlist(body: &str) -> Option<Reply> {
    ok("application/vnd.apple.mpegurl", body)
}

fn status(status: u16) -> Option<Reply> {
    Some(Reply {
        status,
        content_type: "text/plain",
        body: Vec::new(),
    })
}

/// The paths asked for, in order.
type Log = Arc<Mutex<Vec<String>>>;

/// A local station that answers a request for a path, given how many times
/// it has been asked for, with `answer`; none closes the connection without
/// a word. Bodies have no length: they end when the connection closes, as a
/// stream does.
fn serve(answer: impl Fn(&str, usize) -> Option<Reply> + Send + Sync + 'static) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let log = Log::default();
    let (answer, asked, seen) = (Arc::new(answer), log.clone(), Mutex::new(HashMap::new()));
    let seen = Arc::new(seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let (answer, asked, seen) = (answer.clone(), asked.clone(), seen.clone());
            std::thread::spawn(move || {
                let mut stream = BufReader::new(stream.unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                let nth = {
                    let mut seen = seen.lock().unwrap();
                    let count = seen.entry(path.clone()).or_insert(0);
                    *count += 1;
                    *count - 1
                };
                asked.lock().unwrap().push(path.clone());
                let Some(reply) = answer(&path, nth) else {
                    return;
                };
                let head = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                    reply.status, reply.content_type
                );
                let stream = stream.get_mut();
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&reply.body);
            });
        }
    });
    (address, log)
}

/// Collects what arrives, and wants no more once it has `enough` bytes.
struct Collector {
    bytes: Vec<u8>,
    enough: usize,
}

impl Collector {
    fn wanting(enough: usize) -> Self {
        Self {
            bytes: Vec::new(),
            enough,
        }
    }
}

impl Sink for Collector {
    fn accept(&mut self, bytes: &[u8]) -> bool {
        self.bytes.extend_from_slice(bytes);
        self.wanted()
    }

    fn wanted(&self) -> bool {
        self.bytes.len() < self.enough
    }
}

fn radio() -> Radio {
    Radio::new("radiostream-test")
        .unwrap()
        .retrying_after(Duration::from_millis(10))
}

fn count(log: &Log, path: &str) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|asked| *asked == path)
        .count()
}

#[tokio::test]
async fn a_plain_stream_is_passed_on_until_the_sink_wants_no_more() {
    let (server, log) = serve(|_, _| ok("audio/aac", vec![7; 5000]));
    let mut sink = Collector::wanting(3000);
    radio()
        .play(&format!("{server}/live.aac"), &mut sink)
        .await
        .unwrap();
    assert!(sink.bytes.len() >= 3000);
    assert!(sink.bytes.iter().all(|&byte| byte == 7));
    assert_eq!(count(&log, "/live.aac"), 1);
}

#[tokio::test]
async fn a_stream_that_drops_is_opened_again_and_goes_on() {
    let (server, log) = serve(|_, nth| ok("audio/mpeg", vec![nth as u8; 100]));
    let mut sink = Collector::wanting(300);
    radio()
        .play(&format!("{server}/live.mp3"), &mut sink)
        .await
        .unwrap();
    let parts: Vec<u8> = sink.bytes.iter().step_by(100).copied().collect();
    assert_eq!(parts, [0, 1, 2]);
    assert_eq!(count(&log, "/live.mp3"), 3);
}

#[tokio::test]
async fn a_station_that_keeps_failing_is_tried_a_few_times_and_then_given_up() {
    let (server, log) = serve(|_, _| None);
    let error = radio()
        .play(&format!("{server}/live.mp3"), &mut Collector::wanting(1))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Dropped(_)), "{error}");
    assert!(
        error
            .to_string()
            .starts_with("The station stopped answering")
    );
    assert_eq!(count(&log, "/live.mp3"), 1 + RETRIES as usize);

    let (server, log) = serve(|_, _| status(503));
    let error = radio()
        .play(&format!("{server}/down"), &mut Collector::wanting(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("HTTP 503"), "{error}");
    assert_eq!(count(&log, "/down"), 1 + RETRIES as usize);
}

#[tokio::test]
async fn a_station_that_is_not_there_is_not_tried_again() {
    let (server, log) = serve(|_, _| status(404));
    let error = radio()
        .play(&format!("{server}/gone"), &mut Collector::wanting(1))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Status(404)), "{error}");
    assert_eq!(count(&log, "/gone"), 1);
    assert!(matches!(
        radio()
            .play("not an address", &mut Collector::wanting(1))
            .await,
        Err(Error::Address(_))
    ));
}

#[tokio::test]
async fn a_failure_that_follows_data_starts_the_retries_afresh() {
    // Two failures after each piece of data: more than the retries in all,
    // fewer in a row.
    let (server, log) = serve(|_, nth| match nth {
        0 | 3 | 6 => ok("audio/aac", vec![1; 10]),
        _ => None,
    });
    let mut sink = Collector::wanting(30);
    radio()
        .play(&format!("{server}/s"), &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.bytes.len(), 30);
    assert_eq!(count(&log, "/s"), 7);
}

const MASTER: &str = "#EXTM3U\n\
    #EXT-X-STREAM-INF:BANDWIDTH=64000\nlow.m3u8\n\
    #EXT-X-STREAM-INF:BANDWIDTH=128000\nhigh/index.m3u8\n";

#[tokio::test]
async fn an_hls_station_is_followed_through_its_master_playlist_and_refreshed_for_new_segments() {
    let (server, log) = serve(|path, nth| match (path, nth) {
        ("/radio.m3u8", _) => playlist(MASTER),
        ("/high/index.m3u8", 0) => playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:0\n#EXT-X-MEDIA-SEQUENCE:7\n\
             #EXTINF:5,\ns7.ts\n#EXTINF:5,\ns8.ts\n#EXTINF:5,\ns9.ts\n",
        ),
        ("/high/index.m3u8", _) => playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:0\n#EXT-X-MEDIA-SEQUENCE:8\n\
             #EXTINF:5,\ns8.ts\n#EXTINF:5,\ns9.ts\n#EXTINF:5,\ns10.ts\n#EXT-X-ENDLIST\n",
        ),
        (path, _) if path.starts_with("/high/s") => ok("video/mp2t", path.as_bytes()),
        _ => status(404),
    });
    let mut sink = Collector::wanting(usize::MAX);
    radio()
        .play(&format!("{server}/radio.m3u8"), &mut sink)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(sink.bytes).unwrap(),
        "/high/s7.ts/high/s8.ts/high/s9.ts/high/s10.ts"
    );
    assert_eq!(count(&log, "/low.m3u8"), 0);
    assert_eq!(count(&log, "/high/s8.ts"), 1);
}

#[tokio::test]
async fn an_hls_station_is_recognised_by_its_content_type_and_a_lost_segment_is_skipped() {
    let (server, _) = serve(|path, _| match path {
        "/station" => playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nlost.ts\n#EXTINF:1,\nthere.ts\n#EXT-X-ENDLIST\n",
        ),
        "/there.ts" => ok("video/mp2t", "there"),
        _ => status(404),
    });
    let mut sink = Collector::wanting(usize::MAX);
    radio()
        .play(&format!("{server}/station"), &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.bytes, b"there");
}

#[tokio::test]
async fn an_init_segment_comes_first_and_again_after_a_discontinuity() {
    let (server, _) = serve(|path, _| match path {
        "/live.m3u8" => playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MAP:URI=\"a.init\"\n#EXTINF:1,\none.m4s\n\
             #EXTINF:1,\ntwo.m4s\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"b.init\"\n\
             #EXTINF:1,\nthree.m4s\n#EXT-X-ENDLIST\n",
        ),
        path => ok("video/mp4", path.trim_start_matches('/').as_bytes()),
    });
    let mut sink = Collector::wanting(usize::MAX);
    radio()
        .play(&format!("{server}/live.m3u8"), &mut sink)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(sink.bytes).unwrap(),
        "a.initone.m4stwo.m4sb.initthree.m4s"
    );
}

#[tokio::test]
async fn an_encrypted_station_is_refused_without_a_retry() {
    let (server, log) = serve(|_, _| playlist("#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n"));
    let error = radio()
        .play(&format!("{server}/live.m3u8"), &mut Collector::wanting(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("encrypted"), "{error}");
    assert_eq!(count(&log, "/live.m3u8"), 1);
}

#[tokio::test]
async fn stopping_an_hls_station_ends_the_wait_for_its_next_playlist() {
    let (server, log) = serve(|path, _| match path {
        "/live.m3u8" => playlist("#EXTM3U\n#EXT-X-TARGETDURATION:30\n#EXTINF:30,\nonly.ts\n"),
        _ => ok("video/mp2t", "x"),
    });
    let started = Instant::now();
    radio()
        .play(&format!("{server}/live.m3u8"), &mut Collector::wanting(1))
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(count(&log, "/live.m3u8"), 1);
}

#[test]
fn a_playlist_is_read_again_after_its_target_duration_or_half_of_it() {
    let media = |target_seconds| Media {
        target_seconds,
        segments: Vec::new(),
        ended: false,
    };
    assert_eq!(refresh_after(&media(6.0), true), Duration::from_secs(6));
    assert_eq!(refresh_after(&media(6.0), false), Duration::from_secs(3));
    assert_eq!(refresh_after(&media(0.0), true), Duration::from_millis(500));
}

#[tokio::test]
async fn retries_wait_longer_each_time_and_start_over_with_data() {
    let sink = Collector::wanting(usize::MAX);
    let mut retries = Retries::new(Duration::from_millis(20));
    let started = Instant::now();
    for _ in 0..2 {
        assert!(retries.spend(Error::Closed, &sink).await.is_ok());
    }
    assert!(
        started.elapsed() >= Duration::from_millis(60),
        "20 ms, then 40"
    );
    retries.reset();
    for _ in 0..RETRIES {
        assert!(retries.spend(Error::Closed, &sink).await.is_ok());
    }
    let last = retries.spend(Error::Closed, &sink).await;
    assert!(matches!(last, Err(Stop::Failed(Error::Dropped(_)))));
}

#[test]
fn a_playlist_is_read_again_within_bounds_whatever_its_target_duration_says() {
    let media = |target_seconds| Media {
        target_seconds,
        segments: Vec::new(),
        ended: false,
    };
    let longest = Duration::from_secs_f64(MAX_REFRESH);
    for odd in [1e30, f64::INFINITY] {
        assert_eq!(refresh_after(&media(odd), true), longest);
    }
    for odd in [f64::NAN, -5.0] {
        assert_eq!(
            refresh_after(&media(odd), true),
            Duration::from_secs_f64(MIN_REFRESH)
        );
    }
}

#[tokio::test]
async fn waiting_ends_as_soon_as_the_sink_wants_no_more() {
    let started = Instant::now();
    let waited = wait(Duration::from_secs(30), &Collector::wanting(0)).await;
    assert!(matches!(waited, Err(Stop::Unwanted)));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        wait(Duration::from_millis(10), &Collector::wanting(1))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_playlist_is_recognised_by_its_address_whatever_content_type_it_is_served_as() {
    let (server, log) = serve(|path, _| match path {
        "/station.m3u8" => ok(
            "text/plain",
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nthere.ts\n#EXT-X-ENDLIST\n",
        ),
        _ => ok("video/mp2t", "there"),
    });
    // Enough to end a read of the playlist as if it were the stream.
    let mut sink = Collector::wanting(200);
    radio()
        .play(&format!("{server}/station.m3u8"), &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.bytes, b"there");
    assert_eq!(count(&log, "/there.ts"), 1);
}

#[tokio::test]
async fn failures_in_different_segments_do_not_add_up_to_a_dropped_station() {
    // Two failures before each of three segments: more than the retries in
    // all, fewer in a row.
    let (server, _) = serve(|path, nth| match (path, nth) {
        ("/live.m3u8", _) => playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n\
             #EXTINF:1,\nc.ts\n#EXT-X-ENDLIST\n",
        ),
        (_, 0 | 1) => status(503),
        (path, _) => ok("video/mp2t", path.trim_start_matches('/').as_bytes()),
    });
    let mut sink = Collector::wanting(usize::MAX);
    radio()
        .play(&format!("{server}/live.m3u8"), &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.bytes, b"a.tsb.tsc.ts");
}

#[tokio::test]
async fn an_init_segment_comes_again_after_a_discontinuity_that_names_the_same_one() {
    let (server, _) = serve(|path, _| match path {
        "/live.m3u8" => playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MAP:URI=\"a.init\"\n#EXTINF:1,\none.m4s\n\
             #EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"a.init\"\n#EXTINF:1,\ntwo.m4s\n#EXT-X-ENDLIST\n",
        ),
        path => ok("video/mp4", path.trim_start_matches('/').as_bytes()),
    });
    let mut sink = Collector::wanting(usize::MAX);
    radio()
        .play(&format!("{server}/live.m3u8"), &mut sink)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(sink.bytes).unwrap(),
        "a.initone.m4sa.inittwo.m4s"
    );
}

/// A local station that answers every request with `answer` as it is, then
/// closes the connection.
fn serve_raw(answer: &'static [u8]) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let log = Log::default();
    let asked = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = BufReader::new(stream.unwrap());
            let mut request = String::new();
            while stream.read_line(&mut request).unwrap() > 2 {}
            asked
                .lock()
                .unwrap()
                .push(request.split(' ').nth(1).unwrap_or_default().to_owned());
            let _ = stream.get_mut().write_all(answer);
        }
    });
    (address, log)
}

#[tokio::test]
async fn a_shoutcast_station_that_answers_icy_is_passed_on_like_any_other() {
    let (server, log) = serve_raw(b"ICY 200 OK\r\nicy-name:Test\r\nicy-br:128\r\n\r\nstream bytes");
    // The body ends when the connection does, as a stream that dropped, and
    // is asked for again.
    let mut sink = Collector::wanting(2 * b"stream bytes".len());
    radio()
        .play(&format!("{server}/live?x=1"), &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.bytes, b"stream bytesstream bytes");
    assert_eq!(log.lock().unwrap()[0], "/live?x=1");
}

#[tokio::test]
async fn an_icy_station_that_is_not_there_is_not_tried_again() {
    let (server, log) = serve_raw(b"ICY 404 Not Found\r\n\r\n");
    let error = radio()
        .play(&format!("{server}/gone"), &mut Collector::wanting(1))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Status(404)), "{error}");
    // One request the HTTP client made, one the ICY reader did.
    assert_eq!(log.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_reply_that_is_neither_http_nor_icy_is_a_station_that_stopped_answering() {
    let (server, _) = serve_raw(b"NOT A STATUS LINE\r\n\r\n");
    let error = radio()
        .play(&format!("{server}/live"), &mut Collector::wanting(1))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Dropped(_)), "{error}");
}
