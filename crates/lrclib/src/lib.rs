//! The [LRCLIB](https://lrclib.net/docs) lyrics service: the words of a song
//! found by its artist, title, album and length. It needs no account or key.
//!
//! LRCLIB asks for a User-Agent that names the app and its version. A song
//! only matches when its length is within two seconds of the record's, so
//! the length is sent whenever it is known.

use std::fmt;

use reqwest::{RequestBuilder, Url};
use serde::Deserialize;

pub const API_ROOT: &str = "https://lrclib.net/api/";

/// LRCLIB takes lengths from one second to an hour; any other is left out.
const LENGTHS: std::ops::RangeInclusive<i64> = 1..=3600;

/// The most of an answer read, in bytes; no song's words come near it.
const ANSWER_BYTES: usize = 512 * 1024;

#[cfg(not(target_family = "wasm"))]
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
        let builder = reqwest::Client::builder().user_agent(user_agent);
        #[cfg(not(target_family = "wasm"))]
        let builder = builder.timeout(PATIENCE);
        Ok(Self {
            http: builder.build()?,
            root: Url::parse(root).map_err(|e| Error::Address(e.to_string()))?,
        })
    }

    /// The words of `song` as lines of text, or `None` when LRCLIB has none:
    /// no record, or one of an instrumental.
    pub async fn words(&self, song: Song<'_>) -> Result<Option<String>> {
        let url = self.root.join("get").expect("get is a relative path");
        let response = exchange(self.http.get(url).query(&query(song))).await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        words_in_answer(status, &body)
    }
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

/// The words in LRCLIB's answer: none for a 404, which is how it says it
/// has no record.
fn words_in_answer(status: u16, body: &[u8]) -> Result<Option<String>> {
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
    Ok(record.words())
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
    /// The plain lyrics, else the synced ones without their times, as the
    /// server's synced lyrics are shown. None when there is no text.
    fn words(self) -> Option<String> {
        if self.instrumental {
            return None;
        }
        let plain = self.plain_lyrics.filter(|text| !text.trim().is_empty());
        let words = plain.or_else(|| self.synced_lyrics.as_deref().map(without_times));
        words
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }
}

/// LRC text as plain lines: each line loses the `[mm:ss.xx]` times in front
/// of it.
fn without_times(lrc: &str) -> String {
    let lines: Vec<&str> = lrc.lines().map(without_leading_times).collect();
    lines.join("\n")
}

fn without_leading_times(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some(after_tag) = rest.strip_prefix('[').and_then(strip_time_tag) {
        rest = after_tag.trim_start();
    }
    rest.trim_end()
}

/// What follows the `[` of a time such as `00:17.12]`, or `None` when the
/// tag isn't a time.
fn strip_time_tag(after_bracket: &str) -> Option<&str> {
    let (tag, rest) = after_bracket.split_once(']')?;
    let is_time = tag.contains(':')
        && tag
            .chars()
            .all(|c| c.is_ascii_digit() || c == ':' || c == '.');
    is_time.then_some(rest)
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

    fn words(json: &str) -> Result<Option<String>> {
        words_in_answer(200, json.as_bytes())
    }

    #[test]
    fn the_plain_lyrics_are_the_words() {
        let json = r#"{"id":1,"instrumental":false,"plainLyrics":"First\nSecond\n","syncedLyrics":"[00:01.00] First\n[00:02.50] Second"}"#;
        assert_eq!(words(json).unwrap().as_deref(), Some("First\nSecond"));
    }

    #[test]
    fn synced_lyrics_alone_lose_their_times() {
        let json = r#"{"plainLyrics":null,"syncedLyrics":"[00:17.12] I feel your breath\n[01:02.5][01:30.00] Again\n[03:25.72] "}"#;
        assert_eq!(
            words(json).unwrap().as_deref(),
            Some("I feel your breath\nAgain")
        );
    }

    #[test]
    fn a_line_that_starts_with_a_bracketed_word_keeps_it() {
        let json = r#"{"syncedLyrics":"[00:05.00] [Chorus] Sing"}"#;
        assert_eq!(words(json).unwrap().as_deref(), Some("[Chorus] Sing"));
    }

    #[test]
    fn an_instrumental_or_an_empty_record_has_no_words() {
        assert_eq!(
            words(r#"{"instrumental":true,"plainLyrics":"la"}"#).unwrap(),
            None
        );
        assert_eq!(
            words(r#"{"plainLyrics":"  \n","syncedLyrics":null}"#).unwrap(),
            None
        );
        assert_eq!(words("{}").unwrap(), None);
    }

    #[test]
    fn a_404_is_no_record_and_other_failures_are_errors() {
        let missing = br#"{"message":"Failed to find specified track","name":"TrackNotFound","statusCode":404}"#;
        assert_eq!(words_in_answer(404, missing).unwrap(), None);
        assert!(matches!(
            words_in_answer(503, b""),
            Err(Error::Refused(503))
        ));
        assert!(matches!(
            words_in_answer(200, b"<html>"),
            Err(Error::Unreadable(_))
        ));
        let huge = vec![b' '; ANSWER_BYTES + 1];
        assert!(matches!(words_in_answer(200, &huge), Err(Error::TooLarge)));
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
