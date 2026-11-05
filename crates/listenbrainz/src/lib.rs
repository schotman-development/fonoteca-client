//! The [ListenBrainz](https://listenbrainz.readthedocs.io/en/latest/users/api/)
//! calls: submit listens and the track playing now, love, hate or clear a
//! recording, and read a user's feedback back.
//!
//! Recordings are identified by their MusicBrainz recording ID, which
//! OpenSubsonic servers such as Navidrome put in a song's `musicBrainzId`.
//!
//! ListenBrainz asks every client to make at most one call per second.
// ponytail: no pacing in here; the caller spaces calls with a timer that works
// natively and in the browser, since std's clock doesn't exist on wasm.

mod stats;

use std::fmt;

use reqwest::header::AUTHORIZATION;
use reqwest::{RequestBuilder, Url};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};

mod playlist;

pub use playlist::{Playlist, PlaylistTrack};

pub const API_ROOT: &str = "https://api.listenbrainz.org/1/";

/// The most listens one submit call takes.
pub const MAX_LISTENS_PER_REQUEST: usize = 1000;

pub use stats::{COVER_ART_ROOT, GenreActivity, Range, TopAlbum, TopArtist, TopGenre};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What ListenBrainz keeps of a track played.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackMetadata {
    pub artist_name: String,
    pub track_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_name: Option<String>,
    pub additional_info: AdditionalInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionalInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_mbid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracknumber: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    /// The program playing and the one submitting, which are this app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_player: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_client: Option<String>,
}

/// A track played, and when it began, in seconds since 1970.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listen {
    pub listened_at: i64,
    pub track_metadata: TrackMetadata,
}

/// A user's feedback on a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i8", into = "i8")]
pub enum Score {
    Love,
    Hate,
    /// No feedback. Submitting it removes the user's earlier feedback.
    Clear,
}

impl From<Score> for i8 {
    fn from(score: Score) -> i8 {
        match score {
            Score::Love => 1,
            Score::Hate => -1,
            Score::Clear => 0,
        }
    }
}

impl TryFrom<i8> for Score {
    type Error = String;

    fn try_from(value: i8) -> Result<Self, String> {
        match value {
            1 => Ok(Score::Love),
            -1 => Ok(Score::Hate),
            0 => Ok(Score::Clear),
            _ => Err(format!("unknown feedback score {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Feedback {
    /// The MusicBrainz user name.
    pub user_id: String,
    pub recording_mbid: Option<String>,
    /// ListenBrainz's own ID for recordings MusicBrainz doesn't know.
    pub recording_msid: Option<String>,
    pub score: Score,
    /// When the feedback was given, in seconds since 1970.
    pub created: Option<i64>,
}

/// One page of [`Client::user_feedback`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FeedbackPage {
    pub feedback: Vec<Feedback>,
    pub count: u32,
    pub total_count: u32,
    pub offset: u32,
}

pub struct Client {
    http: reqwest::Client,
    root: Url,
    token: String,
}

impl Client {
    /// `token` is the user token shown on <https://listenbrainz.org/settings/>.
    pub fn new(token: impl Into<String>) -> Result<Self> {
        Self::with_root(token, API_ROOT)
    }

    /// A client for a server other than ListenBrainz's own, so that tests
    /// never reach the real one. `root` ends in the API version.
    pub fn with_root(token: impl Into<String>, root: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().build()?,
            root: Url::parse(root).expect("the root is a valid URL"),
            token: token.into(),
        })
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.root.clone();
        url.path_segments_mut()
            .expect("API_ROOT is an http URL")
            .pop_if_empty()
            .extend(segments);
        url
    }

    fn authorized(&self, request: RequestBuilder) -> RequestBuilder {
        request.header(AUTHORIZATION, format!("Token {}", self.token))
    }

    /// The user name the token belongs to, or `None` when ListenBrainz
    /// doesn't accept the token.
    pub async fn validate_token(&self) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct Validation {
            valid: bool,
            user_name: Option<String>,
        }
        let validation: Validation =
            send(self.authorized(self.http.get(self.url(&["validate-token"])))).await?;
        Ok(validation.user_name.filter(|_| validation.valid))
    }

    /// Says that the user has just begun listening to a track.
    pub async fn playing_now(&self, track_metadata: &TrackMetadata) -> Result<()> {
        #[derive(Serialize)]
        struct Payload<'a> {
            track_metadata: &'a TrackMetadata,
        }
        let body = SubmitListens {
            listen_type: "playing_now",
            payload: &[Payload { track_metadata }],
        };
        self.submit(&body).await
    }

    /// Keeps these listens, at most [`MAX_LISTENS_PER_REQUEST`] of them. One
    /// is a `single`, which ListenBrainz asks of a track just finished; more
    /// are an `import`.
    pub async fn submit_listens(&self, listens: &[Listen]) -> Result<()> {
        let listen_type = if listens.len() == 1 {
            "single"
        } else {
            "import"
        };
        let body = SubmitListens {
            listen_type,
            payload: listens,
        };
        self.submit(&body).await
    }

    async fn submit<T: Serialize>(&self, body: &SubmitListens<'_, T>) -> Result<()> {
        let request = self.http.post(self.url(&["submit-listens"])).json(body);
        send::<IgnoredAny>(self.authorized(request)).await.map(drop)
    }

    /// Loves, hates or clears the user's feedback on a recording.
    pub async fn submit_feedback(&self, recording_mbid: &str, score: Score) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            recording_mbid: &'a str,
            score: Score,
        }
        let request = self
            .http
            .post(self.url(&["feedback", "recording-feedback"]))
            .json(&Body {
                recording_mbid,
                score,
            });
        send::<IgnoredAny>(self.authorized(request)).await.map(drop)
    }

    /// A page of `user`'s feedback. `score` picks loves or hates; `None` or
    /// [`Score::Clear`] returns both. `count` defaults to 25, at most 1000.
    pub async fn user_feedback(
        &self,
        user: &str,
        score: Option<Score>,
        count: Option<u32>,
        offset: Option<u32>,
    ) -> Result<FeedbackPage> {
        let query: Vec<(&str, String)> = [
            (
                "score",
                score
                    .filter(|s| *s != Score::Clear)
                    .map(|s| i8::from(s).to_string()),
            ),
            ("count", count.map(|c| c.to_string())),
            ("offset", offset.map(|o| o.to_string())),
        ]
        .into_iter()
        .filter_map(|(name, value)| Some((name, value?)))
        .collect();
        let request = self
            .http
            .get(self.url(&["feedback", "user", user, "get-feedback"]))
            .query(&query);
        send(self.authorized(request)).await
    }

    /// `user`'s feedback on each of these recordings. Recordings without
    /// feedback come back as [`Score::Clear`].
    pub async fn feedback_for_recordings(
        &self,
        user: &str,
        recording_mbids: &[&str],
    ) -> Result<Vec<Feedback>> {
        if recording_mbids.is_empty() {
            return Ok(Vec::new());
        }
        #[derive(Serialize)]
        struct Body<'a> {
            recording_mbids: &'a [&'a str],
            recording_msids: [&'a str; 0],
        }
        #[derive(Deserialize)]
        struct List {
            feedback: Vec<Feedback>,
        }
        let url = self.url(&["feedback", "user", user, "get-feedback-for-recordings"]);
        let request = self.http.post(url).json(&Body {
            recording_mbids,
            recording_msids: [],
        });
        let list: List = send(self.authorized(request)).await?;
        Ok(list.feedback)
    }
}

#[derive(Serialize)]
struct SubmitListens<'a, T> {
    listen_type: &'static str,
    payload: &'a [T],
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

async fn send<T: DeserializeOwned>(request: RequestBuilder) -> Result<T> {
    Ok(send_checked(request).await?.json().await?)
}

/// The response to `request` when ListenBrainz answered with a success.
async fn send_checked(request: RequestBuilder) -> Result<reqwest::Response> {
    let response = exchange(request).await?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status.as_u16() == 429 {
        let reset_in = response
            .headers()
            .get("X-RateLimit-Reset-In")
            .and_then(|v| v.to_str().ok()?.parse().ok());
        return Err(Error::RateLimited { reset_in });
    }
    #[derive(Deserialize)]
    struct ErrorBody {
        error: String,
    }
    let message = response
        .json::<ErrorBody>()
        .await
        .map(|body| body.error)
        .unwrap_or_default();
    Err(Error::Api {
        status: status.as_u16(),
        message,
    })
}

#[derive(Debug)]
pub enum Error {
    /// The request failed, the connection broke, or the JSON was unexpected.
    Http(reqwest::Error),
    /// Too many calls: wait `reset_in` seconds, when given, before the next.
    RateLimited { reset_in: Option<u64> },
    /// ListenBrainz refused the call.
    Api { status: u16, message: String },
}

impl Error {
    /// Whether ListenBrainz refused a listen as malformed (HTTP 400), which
    /// sending again won't change. Any other failure, an invalid or revoked
    /// token (401, 403), a wrong address (404), the network or ListenBrainz
    /// itself, says nothing about the listen, which may be sent later.
    pub fn is_malformed_listen(&self) -> bool {
        matches!(self, Error::Api { status: 400, .. })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "{e}"),
            Error::RateLimited {
                reset_in: Some(secs),
            } => write!(f, "ListenBrainz rate limit reached, retry in {secs} s"),
            Error::RateLimited { reset_in: None } => write!(f, "ListenBrainz rate limit reached"),
            Error::Api { status, message } => {
                write!(f, "ListenBrainz answered HTTP {status}: {message}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
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

    /// Polls `call` once from a thread that never runs the runtime, as the
    /// app's UI thread awaits its calls, and says how many requests the
    /// server got a second later. Connecting checks the server's certificate,
    /// which on Android can go online, so a call must go out on the
    /// runtime's own threads.
    #[cfg(not(target_family = "wasm"))]
    fn requests_while_the_caller_is_busy<F: std::future::Future>(
        call: impl FnOnce(Client, String) -> F,
    ) -> usize {
        use std::io::{Read, Write};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _inside = runtime.handle().enter();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let root = format!("http://{}/1/", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let _ = stream.read(&mut [0; 4096]);
                counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
            }
        });
        let client = Client::with_root("token", &root).unwrap();
        std::thread::spawn(move || runtime.block_on(std::future::pending::<()>()));
        let mut call = Box::pin(call(client, root));
        let mut caller = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(call.as_mut().poll(&mut caller).is_pending());
        std::thread::sleep(std::time::Duration::from_secs(1));
        seen.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn a_call_goes_out_while_its_caller_is_busy() {
        let sent =
            requests_while_the_caller_is_busy(
                |client, _| async move { client.validate_token().await },
            );
        assert_eq!(sent, 1, "the server got no call");
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn a_statistic_goes_out_while_its_caller_is_busy() {
        let sent = requests_while_the_caller_is_busy(|client, _| async move {
            client.top_artists("rob", Range::AllTime, 5).await
        });
        assert_eq!(sent, 1, "the server got no call");
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn a_cover_goes_out_while_its_caller_is_busy() {
        let sent = requests_while_the_caller_is_busy(|client, root| async move {
            client.cover_art(&root, "7c45", 42).await
        });
        assert_eq!(sent, 1, "the server got no call");
    }

    #[test]
    fn user_names_are_escaped_in_paths() {
        let client = Client::new("t").unwrap();
        let url = client.url(&["feedback", "user", "a b/c", "get-feedback"]);
        assert_eq!(
            url.as_str(),
            "https://api.listenbrainz.org/1/feedback/user/a%20b%2Fc/get-feedback"
        );
    }

    #[test]
    fn a_listen_is_the_json_listenbrainz_takes() {
        let listen = Listen {
            listened_at: 1700000000,
            track_metadata: TrackMetadata {
                artist_name: "Rob".into(),
                track_name: "Song".into(),
                release_name: None,
                additional_info: AdditionalInfo {
                    duration: Some(200),
                    ..Default::default()
                },
            },
        };
        let body = SubmitListens {
            listen_type: "single",
            payload: std::slice::from_ref(&listen),
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({"listen_type": "single", "payload": [{
                "listened_at": 1700000000,
                "track_metadata": {"artist_name": "Rob", "track_name": "Song",
                    "additional_info": {"duration": 200}}}]})
        );
        let kept = serde_json::to_string(&listen).unwrap();
        assert_eq!(serde_json::from_str::<Listen>(&kept).unwrap(), listen);
    }

    #[test]
    fn only_a_400_says_the_listen_is_malformed() {
        let api = |status| Error::Api {
            status,
            message: String::new(),
        };
        assert!(api(400).is_malformed_listen());
        for status in [401, 403, 404, 429, 500, 503] {
            assert!(!api(status).is_malformed_listen(), "{status}");
        }
        assert!(!Error::RateLimited { reset_in: None }.is_malformed_listen());
    }

    #[test]
    fn feedback_json() {
        let page: FeedbackPage = serde_json::from_str(
            r#"{"feedback": [{"user_id": "rob", "recording_msid": null,
                "recording_mbid": "8f3471b5-7e6a-48da-86a9-c1c07a0f47ae", "score": 1, "created": 1700000000},
               {"user_id": "rob", "recording_msid": "d23f4719-9212-49f0-ad08-ddbfbfc50d6f",
                "recording_mbid": null, "score": -1}],
               "count": 2, "total_count": 2, "offset": 0}"#,
        )
        .unwrap();
        assert_eq!(page.feedback[0].score, Score::Love);
        assert_eq!(page.feedback[1].score, Score::Hate);
        assert_eq!(page.feedback[1].created, None);
        assert!(serde_json::from_str::<Score>("2").is_err());
        let json = |score| serde_json::to_string(&score).unwrap();
        assert_eq!(
            [json(Score::Love), json(Score::Hate), json(Score::Clear)],
            ["1", "-1", "0"]
        );
    }
}
