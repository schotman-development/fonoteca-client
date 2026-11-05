//! Live radio stations over the app's own HTTPS client: a plain stream
//! (Icecast, Shoutcast: MP3, AAC or Ogg) is passed on as it arrives, and an
//! HLS stream has its playlist read and refetched and its segments fetched
//! in order and passed on one after the other. The bytes go to a [`Sink`]
//! for FFmpeg to decode; nothing here touches FFmpeg, so FFmpeg needs no
//! network code or TLS library.

mod hls;
mod icy;

use std::fmt;
use std::time::Duration;

use reqwest::{Response, Url};

use hls::{Media, Playlist, Segment, Sequencer};

/// Where the bytes of a station go.
pub trait Sink: Send + Sync {
    /// Takes the next bytes; false once nothing wants them any more.
    fn accept(&mut self, bytes: &[u8]) -> bool;

    /// Whether anything still wants bytes, asked while there are none to give.
    fn wanted(&self) -> bool;
}

/// How many times in a row a station is tried again before it is given up.
const RETRIES: u32 = 3;

/// The wait before the first retry, doubled for each after.
const FIRST_RETRY_AFTER: Duration = Duration::from_secs(1);

/// How long a connection takes to open, and a body to go on arriving, before
/// it counts as dropped.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// How often a wait looks whether the sink still wants the station, so
/// stopping one doesn't leave a fetch behind for the length of a segment.
const WANTED_CHECK: Duration = Duration::from_millis(100);

/// The shortest and longest wait, in seconds, before a playlist is read again.
const MIN_REFRESH: f64 = 0.5;
const MAX_REFRESH: f64 = 60.0;

const PLAYLIST_LIMIT: usize = 1 << 20;
const SEGMENT_LIMIT: usize = 16 << 20;

/// What the HTTP client says of a reply whose status line it can't read,
/// such as ICY's.
const UNKNOWN_STATUS_LINE: &str = "invalid HTTP version";

/// Playlist address endings and content types, as stations serve them.
const PLAYLIST_EXTENSION: &str = ".m3u8";
const PLAYLIST_CONTENT_TYPE: &str = "mpegurl";

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a step stopped: it failed, or nothing wants the station any more.
enum Stop {
    Failed(Error),
    Unwanted,
}

impl From<Error> for Stop {
    fn from(e: Error) -> Self {
        Stop::Failed(e)
    }
}

impl From<reqwest::Error> for Stop {
    fn from(e: reqwest::Error) -> Self {
        Stop::Failed(e.into())
    }
}

type Flow<T> = std::result::Result<T, Stop>;

#[derive(Debug)]
pub enum Error {
    Address(String),
    Http(reqwest::Error),
    /// The station answered with this HTTP status.
    Status(u16),
    /// A connection of the station's own, in ICY, failed.
    Io(std::io::Error),
    Playlist(String),
    Unsupported(String),
    TooBig,
    /// The server ended a stream that goes on for ever.
    Closed,
    /// The station kept failing; this was the last failure.
    Dropped(Box<Error>),
}

impl Error {
    /// Whether trying again could help.
    fn is_temporary(&self) -> bool {
        match self {
            Error::Http(e) => !(e.is_builder() || e.is_redirect()),
            Error::Status(status) => *status >= 500,
            Error::Io(_) => true,
            Error::Closed => true,
            _ => false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Address(why) => write!(f, "The station's address is no good: {why}"),
            Error::Http(e) => write!(f, "{e}"),
            Error::Status(status) => write!(f, "The station answered with HTTP {status}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Playlist(why) => write!(f, "The station's playlist can't be read: {why}"),
            Error::Unsupported(why) => write!(f, "The station can't be played: {why}"),
            Error::Closed => write!(f, "The station ended its stream"),
            Error::TooBig => write!(f, "The station sent more than a playlist or segment holds"),
            Error::Dropped(last) => write!(f, "The station stopped answering: {last}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// What a station answered with: HTTP, or ICY where HTTP's client refused it.
enum Incoming {
    Http(Response),
    Icy(icy::Connection),
}

impl Incoming {
    /// The next bytes of the body; none at its end.
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        match self {
            Incoming::Http(response) => Ok(response.chunk().await?.map(|bytes| bytes.to_vec())),
            Incoming::Icy(connection) => connection.chunk().await,
        }
    }
}

pub struct Radio {
    http: reqwest::Client,
    user_agent: String,
    retry_after: Duration,
}

impl Radio {
    /// `user_agent` names the app and its version, such as "app/1.2". On
    /// Android, as everywhere in the app, a plain http address is refused,
    /// and so is a redirect to one.
    pub fn new(user_agent: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .https_only(cfg!(target_os = "android"))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            user_agent: user_agent.to_owned(),
            retry_after: FIRST_RETRY_AFTER,
        })
    }

    /// The same with another wait before the first retry, such as a test's.
    pub fn retrying_after(mut self, wait: Duration) -> Self {
        self.retry_after = wait;
        self
    }

    /// Passes the station at `address` on to `sink` until nothing wants it
    /// any more (`Ok`), or an HLS stream ends, or it fails more times in a
    /// row than it is retried.
    pub async fn play(&self, address: &str, sink: &mut dyn Sink) -> Result<()> {
        let url = Url::parse(address).map_err(|e| Error::Address(e.to_string()))?;
        match self.open(url, sink).await {
            Ok(()) | Err(Stop::Unwanted) => Ok(()),
            Err(Stop::Failed(e)) => Err(e),
        }
    }

    async fn open(&self, url: Url, sink: &mut dyn Sink) -> Flow<()> {
        let mut retries = Retries::new(self.retry_after);
        loop {
            let response = match self.get(&url).await {
                Ok(response) => response,
                Err(e) => {
                    retries.spend(e, sink).await?;
                    continue;
                }
            };
            if !is_playlist(&url, &response) {
                return self.stream(response, &url, sink, &mut retries).await;
            }
            let text = body(response, PLAYLIST_LIMIT).await;
            match text.and_then(|text| text_of(&text)) {
                Ok(text) => return self.live(&url, text, sink, &mut retries).await,
                Err(e) => retries.spend(e, sink).await?,
            }
        }
    }

    async fn get(&self, url: &Url) -> Result<Incoming> {
        let response = match self.http.get(url.clone()).send().await {
            Ok(response) => response,
            Err(refused) => return self.icy_instead(url, refused).await,
        };
        match response.status() {
            status if status.is_success() => Ok(Incoming::Http(response)),
            status => Err(Error::Status(status.as_u16())),
        }
    }

    /// The answer to `url` when the HTTP client `refused` it as having no
    /// status line it knows: an ICY server's, if it is one. Only on plain
    /// http, which Android doesn't allow.
    async fn icy_instead(&self, url: &Url, refused: reqwest::Error) -> Result<Incoming> {
        let plain_http = url.scheme() == "http" && !cfg!(target_os = "android");
        if !(plain_http && has_unknown_status_line(&refused)) {
            return Err(refused.into());
        }
        match icy::get(url, &self.user_agent).await? {
            Some(connection) => Ok(Incoming::Icy(connection)),
            None => Err(refused.into()),
        }
    }

    /// A plain stream: every chunk is passed on, and a connection that drops
    /// is opened again.
    async fn stream(
        &self,
        mut response: Incoming,
        url: &Url,
        sink: &mut dyn Sink,
        retries: &mut Retries,
    ) -> Flow<()> {
        loop {
            match response.chunk().await {
                Ok(Some(bytes)) => {
                    retries.reset();
                    if !sink.accept(&bytes) {
                        return Err(Stop::Unwanted);
                    }
                    continue;
                }
                Ok(None) => retries.spend(Error::Closed, sink).await?,
                Err(e) => retries.spend(e, sink).await?,
            }
            response = loop {
                match self.get(url).await {
                    Ok(response) => break response,
                    Err(e) => retries.spend(e, sink).await?,
                }
            };
        }
    }

    /// An HLS stream, whose first playlist (`text`, from `url`) may name
    /// variants: the best of them is followed, its playlist read again each
    /// time it should have grown, and its new segments passed on in order.
    async fn live(
        &self,
        url: &Url,
        text: String,
        sink: &mut dyn Sink,
        retries: &mut Retries,
    ) -> Flow<()> {
        let (mut url, mut playlist) = (url.clone(), Playlist::parse(&text, url)?);
        let mut sequencer = Sequencer::default();
        let mut init_sent: Option<Url> = None;
        loop {
            let media = match playlist {
                Playlist::Master(variants) => {
                    let variant = Playlist::best_variant(&variants)
                        .ok_or_else(|| Error::Playlist("it names no variant".into()))?;
                    url = variant.url.clone();
                    playlist = self.playlist(&url, sink, retries).await?;
                    continue;
                }
                Playlist::Media(media) => media,
            };
            let new = sequencer.new_segments(&media);
            for segment in new {
                self.pass_on(segment, &mut init_sent, sink, retries).await?;
            }
            if media.ended {
                return Ok(());
            }
            wait(refresh_after(&media, !new.is_empty()), sink).await?;
            playlist = self.playlist(&url, sink, retries).await?;
        }
    }

    /// The playlist at `url`, read again after a failure.
    async fn playlist(
        &self,
        url: &Url,
        sink: &mut dyn Sink,
        retries: &mut Retries,
    ) -> Flow<Playlist> {
        loop {
            let fetched = async {
                let text = body(self.get(url).await?, PLAYLIST_LIMIT).await?;
                Playlist::parse(&text_of(&text)?, url)
            };
            match fetched.await {
                Ok(playlist) => return Ok(playlist),
                Err(e) => retries.spend(e, sink).await?,
            }
        }
    }

    /// Gives `sink` the segment, after its init segment when that changed.
    async fn pass_on(
        &self,
        segment: &Segment,
        init_sent: &mut Option<Url>,
        sink: &mut dyn Sink,
        retries: &mut Retries,
    ) -> Flow<()> {
        if segment.init != *init_sent || segment.discontinuity {
            if let Some(init) = &segment.init {
                self.fetch_into(init, sink, retries).await?;
            }
            init_sent.clone_from(&segment.init);
        }
        self.fetch_into(&segment.url, sink, retries).await
    }

    /// Fetches `url` whole, and gives it to `sink`.
    async fn fetch_into(&self, url: &Url, sink: &mut dyn Sink, retries: &mut Retries) -> Flow<()> {
        loop {
            let fetched = async { body(self.get(url).await?, SEGMENT_LIMIT).await };
            match fetched.await {
                // A segment the server no longer has is behind the live edge
                // already, and the next one is not.
                Err(Error::Status(404 | 410)) => return Ok(()),
                Ok(bytes) => {
                    retries.reset();
                    return if sink.accept(&bytes) {
                        Ok(())
                    } else {
                        Err(Stop::Unwanted)
                    };
                }
                Err(e) => retries.spend(e, sink).await?,
            }
        }
    }
}

/// hyper has no kind for this but its message, which a test holds to.
fn has_unknown_status_line(error: &reqwest::Error) -> bool {
    let mut cause = std::error::Error::source(error);
    while let Some(inner) = cause {
        if inner.to_string().contains(UNKNOWN_STATUS_LINE) {
            return true;
        }
        cause = inner.source();
    }
    false
}

fn is_playlist(url: &Url, incoming: &Incoming) -> bool {
    let Incoming::Http(response) = incoming else {
        return false;
    };
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    content_type.contains(PLAYLIST_CONTENT_TYPE)
        || url
            .path()
            .to_ascii_lowercase()
            .ends_with(PLAYLIST_EXTENSION)
}

fn text_of(bytes: &[u8]) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| Error::Playlist("it is not text".into()))
}

/// All of a response, unless it is longer than `limit`.
async fn body(mut response: Incoming, limit: usize) -> Result<Vec<u8>> {
    let mut all = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if all.len() + chunk.len() > limit {
            return Err(Error::TooBig);
        }
        all.extend(chunk);
    }
    Ok(all)
}

/// How long to wait before reading a live playlist again: its target
/// duration once it has grown, else half of that, as the standard says. A
/// playlist is read at least this often, and at most as often as a
/// misbehaving one (a huge or not-a-number target) is allowed to put it off.
fn refresh_after(media: &Media, grew: bool) -> Duration {
    let seconds = if grew {
        media.target_seconds
    } else {
        media.target_seconds / 2.0
    };
    // `clamp` would hand a NaN on.
    let seconds = if seconds.is_nan() {
        MIN_REFRESH
    } else {
        seconds.clamp(MIN_REFRESH, MAX_REFRESH)
    };
    Duration::from_secs_f64(seconds)
}

/// Waits `duration`, or ends the station as soon as the sink wants no more.
async fn wait(duration: Duration, sink: &dyn Sink) -> Flow<()> {
    let mut left = duration;
    while !left.is_zero() {
        let step = left.min(WANTED_CHECK);
        tokio::time::sleep(step).await;
        left -= step;
        if !sink.wanted() {
            return Err(Stop::Unwanted);
        }
    }
    Ok(())
}

/// The failures allowed in a row.
struct Retries {
    left: u32,
    next_wait: Duration,
    first_wait: Duration,
}

impl Retries {
    fn new(first_wait: Duration) -> Self {
        Self {
            left: RETRIES,
            next_wait: first_wait,
            first_wait,
        }
    }

    /// Something arrived, so the next failure is the first again.
    fn reset(&mut self) {
        *self = Self::new(self.first_wait);
    }

    /// Waits to try again after `error`, or gives it back, as a dropped
    /// station when retries are used up and as it is when trying again
    /// can't help.
    async fn spend(&mut self, error: Error, sink: &dyn Sink) -> Flow<()> {
        if !error.is_temporary() {
            return Err(error.into());
        }
        if self.left == 0 {
            return Err(Error::Dropped(Box::new(error)).into());
        }
        self.left -= 1;
        let wait_for = self.next_wait;
        self.next_wait *= 2;
        wait(wait_for, sink).await
    }
}

#[cfg(test)]
mod tests;
