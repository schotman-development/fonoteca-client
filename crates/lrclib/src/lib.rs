//! The [LRCLIB](https://lrclib.net/docs) lyrics service: the words of a song
//! found by its artist, title, album and length. It needs no account or key.
//!
//! LRCLIB asks for a User-Agent that names the app and its version. A song
//! only matches when its length is within two seconds of the record's, so
//! the length is sent whenever it is known.

use std::fmt;

use reqwest::{RequestBuilder, Url};
use serde::{Deserialize, Serialize};

pub const API_ROOT: &str = "https://lrclib.net/api/";

/// LRCLIB takes lengths from one second to an hour; any other is left out.
const LENGTHS: std::ops::RangeInclusive<i64> = 1..=3600;

/// The most of an answer read, in bytes; no song's words come near it.
const ANSWER_BYTES: usize = 512 * 1024;

const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What LRCLIB is asked to find. A song without an artist or a title can't
/// be asked about.
#[derive(Debug, Clone, Copy)]
pub struct Song<'a> {
    pub artist: &'a str,
    pub title: &'a str,
    pub album: Option<&'a str>,
    pub seconds: Option<i64>,
}

pub struct Client {
    http: reqwest::Client,
    root: Url,
}

impl Client {
    /// `user_agent` names the app and its version, such as "app/1.2".
    pub fn new(user_agent: &str) -> Result<Self> {
        Self::with_root(API_ROOT, user_agent)
    }

    /// The same at another server, such as a test's.
    pub fn with_root(root: &str, user_agent: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().user_agent(user_agent).build()?,
            root: Url::parse(root).map_err(|e| Error::Address(e.to_string()))?,
        })
    }

    /// The lyrics of `song`, or `None` when LRCLIB has none: no record, or
    /// one of an instrumental.
    pub async fn lyrics(&self, song: Song<'_>) -> Result<Option<Lyrics>> {
        let url = self.root.join("get").expect("get is a relative path");
        let request = self.http.get(url).query(&query(song)).timeout(PATIENCE);
        let response = exchange(request).await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        lyrics_in_answer(status, &body)
    }
}

/// A song's words, and the line each starts at where the song has them
/// synced.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Lyrics {
    /// One line of text to a line, with no times.
    pub text: String,
    /// In the order they are sung, none where the lyrics aren't synced.
    pub timed: Vec<TimedLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimedLine {
    /// Where in the song the line starts, in seconds.
    pub seconds: f64,
    pub text: String,
}

fn query(song: Song<'_>) -> Vec<(&'static str, String)> {
    let mut query = vec![
        ("artist_name", song.artist.to_owned()),
        ("track_name", song.title.to_owned()),
    ];
    if let Some(album) = song.album {
        query.push(("album_name", album.to_owned()));
    }
    if let Some(seconds) = song.seconds.filter(|seconds| LENGTHS.contains(seconds)) {
        query.push(("duration", seconds.to_string()));
    }
    query
}

/// The lyrics in LRCLIB's answer: none for a 404, which is how it says it
/// has no record.
fn lyrics_in_answer(status: u16, body: &[u8]) -> Result<Option<Lyrics>> {
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(Error::Refused(status));
    }
    if body.len() > ANSWER_BYTES {
        return Err(Error::TooLarge);
    }
    let record: Record = serde_json::from_slice(body).map_err(Error::Unreadable)?;
    Ok(record.lyrics())
}

/// The fields of a lyrics record that matter here; the ones LRCLIB sends as
/// null for an instrumental are absent or null.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Record {
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

impl Record {
    /// The plain lyrics, or the synced ones' text where there are no plain;
    /// the synced lyrics' times go with either. None when there is no text.
    fn lyrics(self) -> Option<Lyrics> {
        if self.instrumental {
            return None;
        }
        let timed = self
            .synced_lyrics
            .as_deref()
            .map(parse_lrc)
            .unwrap_or_default();
        let plain = self.plain_lyrics.map(|text| text.trim().to_owned());
        let text = match plain.filter(|text| !text.is_empty()) {
            Some(text) => text,
            None => timed
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        (!text.is_empty()).then_some(Lyrics { text, timed })
    }
}

/// The lines of LRC text with their times. A line may start with several
/// times, which repeats it, as a chorus is; a line of text-less times, and a
/// tag such as `[ar:Artist]` or `[ti:Title]`, adds nothing. Sorted by time.
fn parse_lrc(lrc: &str) -> Vec<TimedLine> {
    let mut lines: Vec<TimedLine> = lrc.lines().flat_map(parse_lrc_line).collect();
    lines.sort_by(|a, b| a.seconds.total_cmp(&b.seconds));
    lines
}

fn parse_lrc_line(line: &str) -> Vec<TimedLine> {
    let mut rest = line.trim_start();
    let mut times = Vec::new();
    while let Some((time, after_tag)) = rest.strip_prefix('[').and_then(time_tag) {
        times.push(time);
        rest = after_tag.trim_start();
    }
    let text = rest.trim_end();
    if text.is_empty() {
        return Vec::new();
    }
    times
        .into_iter()
        .map(|seconds| TimedLine {
            seconds,
            text: text.to_owned(),
        })
        .collect()
}

/// The seconds in a time tag such as `00:17.12]`, and what follows the tag;
/// None when the tag isn't a time.
fn time_tag(after_bracket: &str) -> Option<(f64, &str)> {
    let (tag, rest) = after_bracket.split_once(']')?;
    let (minutes, seconds) = tag.split_once(':')?;
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits(minutes) || !digits(whole) || !(fraction.is_empty() || digits(fraction)) {
        return None;
    }
    let fraction = if fraction.is_empty() {
        0.0
    } else {
        fraction.parse::<f64>().ok()? / 10f64.powi(fraction.len() as i32)
    };
    let time = minutes.parse::<f64>().ok()? * 60.0 + whole.parse::<f64>().ok()? + fraction;
    Some((time, rest))
}

/// Sends `request` from the runtime's threads. Connecting checks the
/// server's certificate, which on Android can go online to see whether it
/// was revoked, and the app awaits its calls on its UI thread.
#[cfg(not(target_family = "wasm"))]
async fn exchange(request: RequestBuilder) -> reqwest::Result<reqwest::Response> {
    tokio::spawn(request.send())
        .await
        .expect("the runtime outlives the app's calls")
}

#[cfg(target_family = "wasm")]
async fn exchange(request: RequestBuilder) -> reqwest::Result<reqwest::Response> {
    request.send().await
}

#[derive(Debug)]
pub enum Error {
    /// The request failed.
    Http(reqwest::Error),
    /// The server's address isn't a URL.
    Address(String),
    /// The server answered with a status that is neither success nor 404.
    Refused(u16),
    /// The answer is larger than any lyrics.
    TooLarge,
    /// The answer isn't the JSON of a lyrics record.
    Unreadable(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "{e}"),
            Error::Address(e) => write!(f, "not a server address: {e}"),
            Error::Refused(status) => write!(f, "answered with status {status}"),
            Error::TooLarge => write!(f, "the answer is too large to be lyrics"),
            Error::Unreadable(e) => write!(f, "the answer isn't a lyrics record: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            Error::Unreadable(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lyrics(json: &str) -> Result<Option<Lyrics>> {
        lyrics_in_answer(200, json.as_bytes())
    }

    fn line(seconds: f64, text: &str) -> TimedLine {
        TimedLine {
            seconds,
            text: text.to_owned(),
        }
    }

    #[test]
    fn the_plain_lyrics_are_the_text_and_the_synced_ones_give_the_times() {
        let json = r#"{"id":1,"instrumental":false,"plainLyrics":"First\nSecond\n","syncedLyrics":"[00:01.00] First\n[00:02.50] Second"}"#;
        let found = lyrics(json).unwrap().unwrap();
        assert_eq!(found.text, "First\nSecond");
        assert_eq!(found.timed, [line(1.0, "First"), line(2.5, "Second")]);
    }

    #[test]
    fn synced_lyrics_alone_give_their_text_without_times() {
        let json =
            r#"{"plainLyrics":null,"syncedLyrics":"[00:17.12] I feel your breath\n[03:25.72] "}"#;
        let found = lyrics(json).unwrap().unwrap();
        assert_eq!(found.text, "I feel your breath");
        assert_eq!(found.timed, [line(17.12, "I feel your breath")]);
    }

    #[test]
    fn plain_lyrics_alone_have_no_times() {
        let found = lyrics(r#"{"plainLyrics":"La la","syncedLyrics":null}"#)
            .unwrap()
            .unwrap();
        assert_eq!((found.text.as_str(), found.timed.len()), ("La la", 0));
    }

    #[test]
    fn a_line_with_several_times_is_repeated_and_the_lines_come_sorted() {
        let lrc = "[01:30.00][00:10.5] Chorus\n[00:05] Verse\n[01:00.250]Bridge";
        assert_eq!(
            parse_lrc(lrc),
            [
                line(5.0, "Verse"),
                line(10.5, "Chorus"),
                line(60.25, "Bridge"),
                line(90.0, "Chorus"),
            ]
        );
    }

    #[test]
    fn tags_that_are_not_times_are_not_lines_and_stay_in_the_text() {
        let lrc = "[ar:Artist]\n[ti:Title]\n[length: 03:25]\n[offset:+100]\n[00:05.00] [Chorus] Sing\n[x:1] [00:06]\nno time";
        assert_eq!(parse_lrc(lrc), [line(5.0, "[Chorus] Sing")]);
    }

    #[test]
    fn an_instrumental_or_an_empty_record_has_no_lyrics() {
        assert_eq!(
            lyrics(r#"{"instrumental":true,"plainLyrics":"la"}"#).unwrap(),
            None
        );
        assert_eq!(
            lyrics(r#"{"plainLyrics":"  \n","syncedLyrics":null}"#).unwrap(),
            None
        );
        assert_eq!(lyrics("{}").unwrap(), None);
    }

    #[test]
    fn a_404_is_no_record_and_other_failures_are_errors() {
        let missing = br#"{"message":"Failed to find specified track","name":"TrackNotFound","statusCode":404}"#;
        assert_eq!(lyrics_in_answer(404, missing).unwrap(), None);
        assert!(matches!(
            lyrics_in_answer(503, b""),
            Err(Error::Refused(503))
        ));
        assert!(matches!(
            lyrics_in_answer(200, b"<html>"),
            Err(Error::Unreadable(_))
        ));
        let huge = vec![b' '; ANSWER_BYTES + 1];
        assert!(matches!(lyrics_in_answer(200, &huge), Err(Error::TooLarge)));
    }

    #[test]
    fn the_length_is_sent_only_within_what_lrclib_takes() {
        let song = |seconds| Song {
            artist: "A",
            title: "T",
            album: Some("Al"),
            seconds,
        };
        let has_duration = |seconds| {
            query(song(seconds))
                .iter()
                .any(|(name, _)| *name == "duration")
        };
        assert!(has_duration(Some(233)));
        assert!(!has_duration(Some(0)));
        assert!(!has_duration(Some(3601)));
        assert!(!has_duration(None));
    }
}
