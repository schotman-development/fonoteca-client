//! Streaming from a server with the `transcoding` extension: it is told what
//! this player decodes and how much quality is wanted, and answers whether to
//! send the song as it is or converted.

use std::collections::HashMap;
use std::sync::Mutex;

use opensubsonic::{
    Client, ClientInfo, DirectPlayProfile, Stream, TranscodeDecision, TranscodingProfile,
};

use crate::prefs::Prefs;
use crate::quality::{self, Connection, Level};

/// The only protocol the player reads; it has no HLS.
const HTTP: &str = "http";
const BITS_PER_KILOBIT: i64 = 1000;
/// What the player's decoder plays as the server keeps it.
const DECODED_CODECS: [&str; 8] = [
    "flac",
    "alac",
    "aac",
    "mp3",
    "opus",
    "vorbis",
    "pcm_s16le",
    "pcm_s24le",
];
/// What a conversion may be sent as, the first the server can make winning.
const CONVERSIONS: [(&str, &str); 3] = [("ogg", "opus"), ("mp4", "aac"), ("mp3", "mp3")];

/// What to ask the server for when fetching a song to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    /// The format and most kbit/s, in a `stream` call: the original, or any
    /// quality from a server that doesn't decide.
    Asked(&'static str, u32),
    /// The server's decision for this level.
    Decided(Level),
}

/// What to fetch over `connection`, from a server that `decides` or not.
pub fn wanted(prefs: &Prefs, connection: Connection, decides: bool) -> Wanted {
    if !decides {
        let (format, max_bit_rate) = quality::request(prefs, connection);
        return Wanted::Asked(format, max_bit_rate);
    }
    match quality::level(prefs, connection) {
        Level::Original => Wanted::Asked(quality::ORIGINAL_FORMAT, 0),
        level => Wanted::Decided(level),
    }
}

impl Wanted {
    /// The format and kbit/s a kept download must match to play instead.
    pub fn as_request(self) -> (&'static str, u32) {
        match self {
            Wanted::Asked(format, max_bit_rate) => (format, max_bit_rate),
            Wanted::Decided(level) => (quality::CONVERTED_FORMAT, level.kbit_per_second()),
        }
    }
}

/// What this player tells the server about itself for `level`: it plays what
/// its decoder knows, within the level's bitrate, and takes a conversion
/// as Opus, else AAC, else MP3.
pub fn client_info(level: Level) -> ClientInfo {
    let most = i64::from(level.kbit_per_second()) * BITS_PER_KILOBIT;
    ClientInfo {
        name: "Fonoteca".to_owned(),
        platform: std::env::consts::OS.to_owned(),
        max_audio_bitrate: most,
        max_transcoding_audio_bitrate: most,
        direct_play_profiles: vec![DirectPlayProfile {
            containers: vec![],
            audio_codecs: DECODED_CODECS.map(str::to_owned).to_vec(),
            protocols: vec![HTTP.to_owned()],
            max_audio_channels: None,
        }],
        transcoding_profiles: CONVERSIONS
            .map(|(container, audio_codec)| TranscodingProfile {
                container: container.to_owned(),
                audio_codec: audio_codec.to_owned(),
                protocol: HTTP.to_owned(),
                max_audio_channels: None,
            })
            .to_vec(),
        codec_profiles: vec![],
    }
}

/// What a decision says to fetch.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    /// The song as the server keeps it.
    Original,
    /// The conversion the `transcodeParams` stand for.
    Converted(String),
}

/// Where the song comes from by `decision`, or why it can't.
pub fn source_of(decision: &TranscodeDecision) -> Result<Source, String> {
    if decision.can_direct_play {
        return Ok(Source::Original);
    }
    match (&decision.transcode_params, decision.can_transcode) {
        (Some(params), true) => Ok(Source::Converted(params.clone())),
        _ => Err(match decision.error_reason.as_deref() {
            Some(reason) if !reason.is_empty() => reason.to_owned(),
            _ => "the server can neither send it as it is nor convert it".to_owned(),
        }),
    }
}

/// The decisions heard for each song and level, so that a song fetched again,
/// as when seeking, doesn't ask again.
#[derive(Default)]
pub struct Decisions(Mutex<HashMap<(String, Level), TranscodeDecision>>);

impl Decisions {
    fn kept(&self, id: &str, level: Level) -> Option<TranscodeDecision> {
        let kept = self.0.lock().unwrap_or_else(|e| e.into_inner());
        kept.get(&(id.to_owned(), level)).cloned()
    }

    fn keep(&self, id: &str, level: Level, decision: TranscodeDecision) {
        let mut kept = self.0.lock().unwrap_or_else(|e| e.into_inner());
        kept.insert((id.to_owned(), level), decision);
    }

    fn forget(&self, id: &str, level: Level) {
        let mut kept = self.0.lock().unwrap_or_else(|e| e.into_inner());
        kept.remove(&(id.to_owned(), level));
    }
}

/// Opens song `id` as the server decides for `level`, from `seconds` in, and
/// says where in the song the stream begins when it begins part way. A kept
/// decision the server no longer honors is asked for again once.
pub async fn open(
    client: &Client,
    decisions: &Decisions,
    id: &str,
    level: Level,
    seconds: f64,
) -> Result<(Stream, Option<f64>), String> {
    if let Some(decision) = decisions.kept(id, level) {
        match fetch(client, id, &decision, seconds).await {
            Ok(opened) => return Ok(opened),
            Err(_) => decisions.forget(id, level),
        }
    }
    let decision = client
        .get_transcode_decision(id, &client_info(level))
        .await
        .map_err(|e| format!("The server wouldn't say how to send the track: {e}"))?;
    let opened = fetch(client, id, &decision, seconds).await;
    if opened.is_ok() {
        decisions.keep(id, level, decision);
    }
    opened
}

/// How a song kept for `level` was converted, for naming its file.
#[derive(Debug, PartialEq, Eq)]
pub struct Converted {
    /// The codec the server converted to, as the file's extension.
    pub format: String,
    /// The most kbit/s it was asked for, which is what a kept file is held
    /// to when weighed against the quality wanted.
    pub kbit_per_second: u32,
}

/// Opens song `id` to keep at `level`, as the server decides, and says how
/// it was converted when it was.
pub async fn open_to_keep(
    client: &Client,
    id: &str,
    level: Level,
) -> Result<(Stream, Option<Converted>), String> {
    let decision = client
        .get_transcode_decision(id, &client_info(level))
        .await
        .map_err(|e| format!("The server wouldn't say how to send the track: {e}"))?;
    let (stream, _) = fetch(client, id, &decision, 0.0).await?;
    let converted = matches!(source_of(&decision), Ok(Source::Converted(_)))
        .then(|| converted_as(&decision, level));
    Ok((stream, converted))
}

fn converted_as(decision: &TranscodeDecision, level: Level) -> Converted {
    let details = decision.transcode_stream.as_ref();
    let named = details.and_then(|d| d.codec.as_deref().or(d.container.as_deref()));
    let format = named
        .map(|name| name.to_ascii_lowercase())
        .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| quality::CONVERTED_FORMAT.to_owned());
    Converted {
        format,
        kbit_per_second: level.kbit_per_second(),
    }
}

async fn fetch(
    client: &Client,
    id: &str,
    decision: &TranscodeDecision,
    seconds: f64,
) -> Result<(Stream, Option<f64>), String> {
    let failed = |e: opensubsonic::Error| format!("The track couldn't be fetched: {e}");
    match source_of(decision).map_err(|reason| format!("The track can't be played: {reason}"))? {
        Source::Original => {
            let original = client.stream(id, quality::ORIGINAL_FORMAT, 0, None);
            Ok((original.await.map_err(failed)?, None))
        }
        Source::Converted(params) => {
            // A song played from its start is asked for plainly, so its
            // conversion keeps what makes it gapless.
            let offset = Some(seconds).filter(|&seconds| seconds > 0.0);
            let converted = client.get_transcode_stream(id, &params, offset);
            Ok((converted.await.map_err(failed)?, Some(seconds)))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn decision(direct: bool, transcode: bool, params: Option<&str>) -> TranscodeDecision {
        TranscodeDecision {
            can_direct_play: direct,
            can_transcode: transcode,
            transcode_reason: vec![],
            error_reason: Some("No such codec".to_owned()),
            transcode_params: params.map(str::to_owned),
            source_stream: None,
            transcode_stream: None,
        }
    }

    #[test]
    fn a_decision_says_to_fetch_the_original_the_conversion_or_why_not() {
        assert_eq!(source_of(&decision(true, true, None)), Ok(Source::Original));
        assert_eq!(
            source_of(&decision(false, true, Some("p-1"))),
            Ok(Source::Converted("p-1".to_owned()))
        );
        assert_eq!(
            source_of(&decision(false, false, None)),
            Err("No such codec".to_owned())
        );
        assert!(source_of(&decision(false, true, None)).is_err());
    }

    #[test]
    fn the_client_tells_http_only_and_the_levels_bitrate() {
        let info = client_info(Level::Medium);
        assert_eq!(info.max_audio_bitrate, 160_000);
        assert_eq!(info.max_transcoding_audio_bitrate, 160_000);
        let protocols = info.direct_play_profiles.iter().flat_map(|p| &p.protocols);
        assert!(protocols.eq([HTTP]));
        assert!(info.transcoding_profiles.iter().all(|p| p.protocol == HTTP));
        assert_eq!(info.transcoding_profiles[0].audio_codec, "opus");
    }

    fn decision_json(inner: &str) -> String {
        format!(
            r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","transcodeDecision":{inner}}}}}"#
        )
    }

    /// A server whose decisions depend on the song asked about: "d" plays
    /// direct, "t" converts, anything else can't be played. It answers
    /// `stream` with "orig" and `getTranscodeStream` with "conv", and keeps
    /// each request's first line.
    fn serve() -> (Client, Arc<Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let told = Arc::new(Mutex::new(Vec::new()));
        let seen = told.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = BufReader::new(stream);
                let (mut line, mut length) = (String::new(), 0);
                stream.read_line(&mut line).unwrap();
                let mut header = String::new();
                while stream.read_line(&mut header).unwrap_or(0) > 2 {
                    if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        length = value.trim().parse().unwrap();
                    }
                    header.clear();
                }
                stream.read_exact(&mut vec![0; length]).unwrap();
                seen.lock().unwrap().push(line.clone());
                let (kind, body) = if line.contains("/getTranscodeStream") {
                    ("audio/ogg", "conv".to_owned())
                } else if line.contains("/stream") {
                    ("audio/flac", "orig".to_owned())
                } else if line.contains("mediaId=d") {
                    (
                        "application/json",
                        decision_json(r#"{"canDirectPlay":true,"canTranscode":true}"#),
                    )
                } else if line.contains("mediaId=t") {
                    (
                        "application/json",
                        decision_json(
                            r#"{"canDirectPlay":false,"canTranscode":true,"transcodeParams":"p%2D1","transcodeStream":{"codec":"Opus","audioBitrate":150000}}"#,
                        ),
                    )
                } else {
                    (
                        "application/json",
                        decision_json(
                            r#"{"canDirectPlay":false,"canTranscode":false,"errorReason":"No such codec"}"#,
                        ),
                    )
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.get_mut().write_all(reply.as_bytes());
            }
        });
        (
            Client::new(&address, opensubsonic::Auth::ApiKey("K".into())).unwrap(),
            told,
        )
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Runtime::new().unwrap().block_on(future)
    }

    async fn read(mut stream: Stream) -> String {
        let mut all = Vec::new();
        while let Some(bytes) = stream.chunk().await.unwrap() {
            all.extend(bytes);
        }
        String::from_utf8(all).unwrap()
    }

    #[test]
    fn the_server_decides_to_send_the_original_a_conversion_or_nothing() {
        block_on(async {
            let (client, told) = serve();
            let decisions = Decisions::default();
            let (direct, start) = open(&client, &decisions, "d", Level::High, 30.0)
                .await
                .unwrap();
            assert_eq!((read(direct).await.as_str(), start), ("orig", None));
            let (converted, start) = open(&client, &decisions, "t", Level::High, 30.0)
                .await
                .unwrap();
            assert_eq!(
                (read(converted).await.as_str(), start),
                ("conv", Some(30.0))
            );
            let Err(refusal) = open(&client, &decisions, "e", Level::High, 0.0).await else {
                panic!("a song the server can't send was opened");
            };
            assert!(refusal.contains("No such codec"), "{refusal}");
            let told = told.lock().unwrap();
            let conversion = told
                .iter()
                .find(|line| line.contains("/getTranscodeStream"))
                .unwrap();
            for part in [
                "mediaId=t",
                "transcodeParams=p%2D1",
                "offset=30",
                "apiKey=K",
            ] {
                assert!(conversion.contains(part), "{conversion}");
            }
            assert!(
                told[0].starts_with("POST /rest/getTranscodeDecision?"),
                "{}",
                told[0]
            );
        });
    }

    #[test]
    fn a_decision_heard_once_is_kept_for_the_song_and_level() {
        block_on(async {
            let (client, told) = serve();
            let decisions = Decisions::default();
            for _ in 0..2 {
                open(&client, &decisions, "t", Level::Low, 0.0)
                    .await
                    .unwrap();
            }
            open(&client, &decisions, "t", Level::Medium, 0.0)
                .await
                .unwrap();
            let told = told.lock().unwrap();
            let asked = told
                .iter()
                .filter(|line| line.contains("getTranscodeDecision"))
                .count();
            assert_eq!(asked, 2, "once for each level");
            assert!(
                told.iter().all(|line| !line.contains("offset=")),
                "{told:?}"
            );
        });
    }

    #[test]
    fn a_song_to_keep_is_named_for_the_codec_the_server_converted_it_to() {
        block_on(async {
            let (client, _) = serve();
            let (direct, converted) = open_to_keep(&client, "d", Level::Medium).await.unwrap();
            assert_eq!((read(direct).await.as_str(), converted), ("orig", None));
            let (stream, converted) = open_to_keep(&client, "t", Level::Medium).await.unwrap();
            assert_eq!(read(stream).await, "conv");
            let converted = converted.unwrap();
            assert_eq!(
                (converted.format.as_str(), converted.kbit_per_second),
                ("opus", 160)
            );
            assert!(open_to_keep(&client, "e", Level::Medium).await.is_err());
        });
    }

    #[test]
    fn only_a_server_that_decides_is_asked_for_a_level() {
        let prefs = Prefs {
            wifi_level: Some(2),
            ..Prefs::default()
        };
        assert_eq!(
            wanted(&prefs, Connection::Wifi, true),
            Wanted::Decided(Level::Medium)
        );
        assert_eq!(
            wanted(&prefs, Connection::Wifi, false),
            Wanted::Asked("raw", 0)
        );
        assert_eq!(
            wanted(&Prefs::default(), Connection::Wifi, true),
            Wanted::Asked("raw", 0)
        );
    }
}
