//! The [ListenBrainz](https://listenbrainz.readthedocs.io/en/latest/users/api/)
//! feedback calls: love, hate or clear a recording, and read a user's
//! feedback back.
//!
//! Recordings are identified by their MusicBrainz recording ID, which
//! OpenSubsonic servers such as Navidrome put in a song's `musicBrainzId`.
//!
//! ListenBrainz asks every client to make at most one call per second.
// ponytail: no pacing in here; the caller spaces calls with a timer that works
// natively and in the browser, since std's clock doesn't exist on wasm.

use std::fmt;

use reqwest::header::AUTHORIZATION;
use reqwest::{RequestBuilder, Url};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};

pub const API_ROOT: &str = "https://api.listenbrainz.org/1/";

pub type Result<T, E = Error> = std::result::Result<T, E>;

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
        Ok(Self {
            http: reqwest::Client::builder().build()?,
            root: Url::parse(API_ROOT).expect("API_ROOT is a valid URL"),
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

async fn send<T: DeserializeOwned>(request: RequestBuilder) -> Result<T> {
    let response = request.send().await?;
    let status = response.status();
    if status.is_success() {
        return Ok(response.json().await?);
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
