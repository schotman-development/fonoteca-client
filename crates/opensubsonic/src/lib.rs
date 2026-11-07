//! Client for the [OpenSubsonic API](https://opensubsonic.netlify.app): the
//! calls for browsing a music library, searching it, starring and rating,
//! managing playlists, internet radio stations and users. Responses are
//! requested as JSON.
//!
//! Call [`Client::get_open_subsonic_extensions`] before anything else. It
//! never sends credentials, it shows whether `apiKeyAuthentication` is offered
//! before the user is asked how to sign in, and on servers with `formPost` it
//! switches the client to form POST before credentials go anywhere, so they
//! never appear in a URL. Servers without `formPost` only take them in the URL.
//!
//! Errors never contain the request URL or the address entered, addresses
//! with a login in them are refused, and redirects are not followed, so
//! credentials only go to that address. The browser build
//! is the exception: the browser follows redirects itself, so a 307 or 308 to
//! another host gets the form body.
//!
//! A client can keep the answers pages read in [`Answers`], and hand them
//! out from there while they are fresh. One made to wait for a [`SignIn`]
//! hands them out before the sign-in is over.

mod answers;
mod api;
mod model;
#[cfg(test)]
mod spec_examples;

pub use answers::{Answers, FRESH_FOR};
pub use api::*;
pub use model::*;

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Poll, Waker};

use md5::{Digest, Md5};
use reqwest::Url;
use reqwest::header::CONTENT_TYPE;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The Subsonic API version this client implements.
pub const PROTOCOL_VERSION: &str = "1.16.1";
const CLIENT_NAME: &str = "fonoteca-client";
/// How long a server may take to accept a connection, and to send the next
/// bytes of an answer, before the call fails.
#[cfg(not(target_family = "wasm"))]
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
#[cfg(not(target_family = "wasm"))]
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// How the client signs in.
#[derive(Clone)]
pub enum Auth {
    /// No credentials. Servers must answer `getOpenSubsonicExtensions`
    /// without any.
    None,
    /// An API key from the server (the `apiKeyAuthentication` extension).
    ApiKey(String),
    /// A username and password. Sent as a salted MD5 token first. When an
    /// https server refuses that with error 41 or 42, as the hex-encoded
    /// password, then as the plain password; over http it stays the token.
    Password { username: String, password: String },
}

// Password methods, in the order they are tried.
const TOKEN: u8 = 0;
const HEX: u8 = 1;
const PLAIN: u8 = 2;

/// What every response says about the server, returned by [`Client::ping`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// The Subsonic API version the server supports.
    pub version: String,
    /// The server's name, e.g. Navidrome. OpenSubsonic servers only.
    pub r#type: Option<String>,
    /// The server's own version. OpenSubsonic servers only.
    pub server_version: Option<String>,
    /// True on servers that implement OpenSubsonic.
    #[serde(default)]
    pub open_subsonic: bool,
}

pub struct Client {
    http: reqwest::Client,
    rest: Url,
    auth: Auth,
    password_method: AtomicU8,
    form_post: AtomicBool,
    extensions: Mutex<Vec<model::OpenSubsonicExtension>>,
    /// Where the answers pages read are kept, if anywhere.
    answers: Option<Arc<Answers>>,
    /// Which kept answers are handed out, and whether an aged one was.
    hands_out: HandsOut,
    gave_aged: AtomicBool,
    /// Whether an answer the server gave differed from the one kept.
    heard_news: AtomicBool,
    /// The sign-in this client waits for before it sends anything.
    signing_in: Option<Arc<SignIn>>,
}

impl Client {
    /// `server` is the address the user enters, with or without a path
    /// prefix: `https://music.example.org` or `https://example.org/navidrome`.
    /// An address with a `user:password@` in it is refused. An address that
    /// redirects fails with a 3xx [`Error::Status`]; the address it redirects
    /// to is the one to enter.
    pub fn new(server: &str, auth: Auth) -> Result<Self> {
        let invalid = |reason: &str| Error::InvalidUrl(reason.to_owned());
        let mut url = Url::parse(server).map_err(|e| invalid(&e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(invalid("not an http or https address"));
        }
        // Android's own setting against plain http only covers its Java
        // networking, not these sockets.
        #[cfg(target_os = "android")]
        if url.scheme() == "http" {
            return Err(invalid("the app only connects to https:// addresses"));
        }
        // The browser quotes the whole URL, credentials included, when it
        // refuses one with a login in it.
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid("the address contains a username or password"));
        }
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        let rest = url.join("rest/").map_err(|e| invalid(&e.to_string()))?;
        let http = reqwest::Client::builder();
        // A redirect would take the credentials along, in the URL, the Referer
        // or a re-sent form body. reqwest has no such setting in the browser.
        #[cfg(not(target_family = "wasm"))]
        let http = http.redirect(reqwest::redirect::Policy::none());
        // A server that stops answering, or a song that stops arriving
        // midway, fails the call instead of leaving the app waiting for good.
        // reqwest has no such settings in the browser either.
        #[cfg(not(target_family = "wasm"))]
        let http = http
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT);
        Ok(Self {
            http: http.build()?,
            rest,
            auth,
            password_method: AtomicU8::new(TOKEN),
            form_post: AtomicBool::new(false),
            extensions: Mutex::default(),
            answers: None,
            hands_out: HandsOut::Fresh,
            gave_aged: AtomicBool::new(false),
            heard_news: AtomicBool::new(false),
            signing_in: None,
        })
    }

    /// The server's address, which with a user name tells accounts apart.
    pub fn address(&self) -> &str {
        self.rest.as_str()
    }

    /// Keeps the answers pages read in `answers`, and hands them out from
    /// there while they are fresh.
    pub fn keeping_answers_in(mut self, answers: Arc<Answers>) -> Self {
        self.answers = Some(answers);
        self
    }

    /// This client, sending nothing until `sign_in` is over and then sending
    /// through the client that signed in: meanwhile it hands out kept
    /// answers, aged ones too where it shows them, for pages shown while the
    /// server is asked. The readers made from it wait for it too.
    pub fn waiting_for(mut self, sign_in: Arc<SignIn>) -> Self {
        self.signing_in = Some(sign_in);
        self
    }

    /// Whether the sign-in this client waits for failed, so that it sends
    /// nothing.
    pub fn sign_in_failed(&self) -> bool {
        (self.signing_in.as_ref()).is_some_and(|sign_in| sign_in.failed())
    }

    /// A client like this one, keeping answers in the same place, with notes
    /// of its own about what it hands out. With `shows_aged` it hands out aged
    /// answers too, for a page that shows them at once and then asks again
    /// with a client without it, which asks the server for them.
    pub fn reader(&self, shows_aged: bool) -> Self {
        let hands_out = match shows_aged {
            true => HandsOut::AgedToo,
            false => HandsOut::Fresh,
        };
        self.handing_out(hands_out)
    }

    /// A client like this one that asks the server for every answer and
    /// keeps it, for a change that must go by what the server has now.
    pub fn asking_the_server(&self) -> Self {
        self.handing_out(HandsOut::Nothing)
    }

    fn handing_out(&self, hands_out: HandsOut) -> Self {
        let extensions = self.extensions.lock().map(|listed| listed.clone());
        Self {
            http: self.http.clone(),
            rest: self.rest.clone(),
            auth: self.auth.clone(),
            password_method: AtomicU8::new(self.password_method.load(Relaxed)),
            form_post: AtomicBool::new(self.form_post.load(Relaxed)),
            extensions: Mutex::new(extensions.unwrap_or_default()),
            answers: self.answers.clone(),
            hands_out,
            gave_aged: AtomicBool::new(false),
            heard_news: AtomicBool::new(false),
            signing_in: self.signing_in.clone(),
        }
    }

    /// Whether this client handed out an aged answer.
    pub fn gave_aged(&self) -> bool {
        self.gave_aged.load(Relaxed)
    }

    /// Whether an answer this client asked the server for said something else
    /// than the one kept.
    pub fn heard_news(&self) -> bool {
        self.heard_news.load(Relaxed)
    }

    /// The account this client signs in to, as a name for what is kept for
    /// it: the same for the same server, login and `salt`, and saying none
    /// of them. Without the salt, a [`random_salt`] the app keeps, a guessed
    /// login can't be checked against the name.
    pub fn account(&self, salt: &str) -> String {
        let login = match &self.auth {
            Auth::None => "",
            Auth::ApiKey(key) => key,
            Auth::Password { username, .. } => username,
        };
        hex(&Md5::digest(format!("{salt}\n{}\n{login}", self.rest)))
    }

    /// Whether the server listed `extension` the last time
    /// [`Client::get_open_subsonic_extensions`] was called.
    pub fn supports(&self, extension: &str) -> bool {
        self.extensions()
            .lock()
            .is_ok_and(|listed| listed.iter().any(|listed| listed.name == extension))
    }

    /// Whether the server listed `version` of `extension`.
    pub fn supports_version(&self, extension: &str, version: i64) -> bool {
        self.extensions().lock().is_ok_and(|listed| {
            (listed.iter())
                .any(|listed| listed.name == extension && listed.versions.contains(&version))
        })
    }

    /// The extensions the server listed, as the client that signed in for
    /// this one heard them, where it waited for a sign-in.
    fn extensions(&self) -> &Mutex<Vec<model::OpenSubsonicExtension>> {
        match self
            .signing_in
            .as_ref()
            .and_then(|sign_in| sign_in.client())
        {
            Some(signed_in) => &signed_in.extensions,
            None => &self.extensions,
        }
    }

    fn request(&self, endpoint: &str, params: &Params) -> Result<(reqwest::RequestBuilder, u8)> {
        let method = self.password_method.load(Relaxed);
        let mut query = vec![
            ("v", PROTOCOL_VERSION.to_owned()),
            ("c", CLIENT_NAME.to_owned()),
            ("f", "json".to_owned()),
        ];
        // Public by spec, and the call that tells whether credentials can go in
        // a POST body.
        let auth = if endpoint == "getOpenSubsonicExtensions" {
            &Auth::None
        } else {
            &self.auth
        };
        match auth {
            Auth::None => {}
            Auth::ApiKey(key) => query.push(("apiKey", key.clone())),
            Auth::Password { username, password } => {
                query.push(("u", username.clone()));
                match method {
                    TOKEN => {
                        let salt = format!("{:016x}", getrandom::u64()?);
                        query.push(("t", token(password, &salt)));
                        query.push(("s", salt));
                    }
                    HEX => query.push(("p", format!("enc:{}", hex(password.as_bytes())))),
                    _ => query.push(("p", password.clone())),
                }
            }
        }
        query.extend(params.0.iter().cloned());
        let url = self
            .rest
            .join(endpoint)
            .map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let request = if self.form_post.load(Relaxed) {
            self.http.post(url).form(&query)
        } else {
            self.http.get(url).query(&query)
        };
        Ok((request, method))
    }

    /// Moves on to the next password method after `used` was refused. False
    /// when there is none left to try, and always over http, where the next
    /// methods would send the password readable.
    fn fall_back(&self, used: u8) -> bool {
        if !matches!(self.auth, Auth::Password { .. })
            || used >= PLAIN
            || self.rest.scheme() != "https"
        {
            return false;
        }
        let _ = self
            .password_method
            .compare_exchange(used, used + 1, Relaxed, Relaxed);
        true
    }

    /// Refuses before a call that carries a password when the server is
    /// neither https nor on this machine, where the password would cross the
    /// network readable.
    fn may_send_password(&self) -> Result<()> {
        let host = self.rest.host_str().unwrap_or_default();
        let local = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if self.rest.scheme() == "https" || local {
            Ok(())
        } else {
            Err(Error::PlainHttp)
        }
    }

    async fn send(&self, endpoint: &str, params: &Params) -> Result<Body> {
        if let Some(sign_in) = &self.signing_in {
            let signed_in = sign_in.over().await.ok_or(Error::NotSignedIn)?;
            return Box::pin(signed_in.send(endpoint, params)).await;
        }
        loop {
            let (request, method) = self.request(endpoint, params)?;
            let response = exchange(request).await?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            if content_type.contains("json") {
                let body = response.bytes().await?;
                match envelope(&body) {
                    Err(Error::Api(e)) if matches!(e.code, 41 | 42) && self.fall_back(method) => {
                        continue;
                    }
                    Err(Error::Json(_)) if !status.is_success() => {
                        return Err(Error::Status(status.as_u16()));
                    }
                    result => return result.map(Body::Json),
                }
            }
            if !status.is_success() {
                return Err(Error::Status(status.as_u16()));
            }
            if content_type.contains("xml") {
                return Err(Error::Unexpected(format!(
                    "{endpoint} answered with XML instead of JSON"
                )));
            }
            return Ok(Body::Binary(response));
        }
    }

    /// Asks for `endpoint`, or hands out the answer kept for it while that is
    /// fresh, or aged where this client shows aged ones. A call that changes
    /// the server then forgets the kept answers it makes out of date.
    async fn json(&self, endpoint: &str, params: &Params) -> Result<Map<String, Value>> {
        let Some(answers) = &self.answers else {
            return self.ask(endpoint, params).await;
        };
        if !answers.is_loaded() {
            let loading = answers.clone();
            in_background(move || loading.wait_until_loaded()).await;
        }
        let Some(read) = answers::read_key(endpoint, &params.0) else {
            let answer = self.ask(endpoint, params).await;
            if let Some(search) = answers.called(endpoint, &params.0) {
                let outdated = in_background(move || search.run()).await;
                answers.forget_outdated(outdated);
            }
            return answer;
        };
        let found = match self.hands_out {
            HandsOut::Nothing => None,
            HandsOut::Fresh | HandsOut::AgedToo => answers.find(&read),
        };
        match found {
            Some(found) if !found.aged => return Ok(found.answer),
            Some(found) if self.hands_out == HandsOut::AgedToo => {
                self.gave_aged.store(true, Relaxed);
                return Ok(found.answer);
            }
            _ => {}
        }
        let writes = answers.writes();
        let answer = self.ask(endpoint, params).await?;
        if answers.keep(read, &answer, writes) {
            self.heard_news.store(true, Relaxed);
        }
        Ok(answer)
    }

    async fn ask(&self, endpoint: &str, params: &Params) -> Result<Map<String, Value>> {
        match self.send(endpoint, params).await? {
            Body::Json(response) => Ok(response),
            Body::Binary(_) => Err(Error::Unexpected(format!(
                "{endpoint} answered with data instead of JSON"
            ))),
        }
    }

    /// Calls `endpoint` and returns the element `key` of its response.
    async fn call<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        params: Params,
        key: &str,
    ) -> Result<T> {
        take(self.json(endpoint, &params).await?, endpoint, key)
    }

    /// Calls an endpoint whose response carries nothing but its status.
    async fn call_unit(&self, endpoint: &str, params: Params) -> Result<()> {
        self.json(endpoint, &params).await.map(drop)
    }

    /// Calls an endpoint that answers with binary data.
    async fn bytes(&self, endpoint: &str, params: Params) -> Result<Vec<u8>> {
        Ok(self.data(endpoint, params).await?.bytes().await?.to_vec())
    }

    /// Calls an endpoint that answers with binary data, and leaves it unread.
    async fn data(&self, endpoint: &str, params: Params) -> Result<reqwest::Response> {
        match self.send(endpoint, &params).await? {
            Body::Binary(response) => Ok(response),
            Body::Json(_) => Err(Error::Unexpected(format!(
                "{endpoint} answered with JSON instead of data"
            ))),
        }
    }
}

/// A sign-in under way, which a client made to wait for it waits for: until
/// it is over that client sends nothing, so no credentials go out before the
/// server's extensions say how they may, and then it sends through the
/// client that signed in.
#[derive(Default)]
pub struct SignIn {
    signed_in: OnceLock<Option<Arc<Client>>>,
    waiting: Mutex<Vec<Waker>>,
}

impl SignIn {
    /// Ends the sign-in, with the client that signed in, or `None` when it
    /// failed, and wakes the calls that wait for it. Only the first end
    /// counts.
    pub fn end(&self, signed_in: Option<Arc<Client>>) {
        let _ = self.signed_in.set(signed_in);
        let waiting = self
            .waiting
            .lock()
            .map(|mut waiting| std::mem::take(&mut *waiting));
        waiting
            .unwrap_or_default()
            .into_iter()
            .for_each(Waker::wake);
    }

    /// The client that signed in, once one has.
    fn client(&self) -> Option<&Arc<Client>> {
        self.signed_in.get().and_then(Option::as_ref)
    }

    /// Whether it ended without a client that signed in.
    fn failed(&self) -> bool {
        matches!(self.signed_in.get(), Some(None))
    }

    /// Waits until the sign-in is over, for the client that signed in.
    async fn over(&self) -> Option<&Arc<Client>> {
        std::future::poll_fn(|context| {
            if let Some(signed_in) = self.signed_in.get() {
                return Poll::Ready(signed_in.as_ref());
            }
            if let Ok(mut waiting) = self.waiting.lock() {
                waiting.push(context.waker().clone());
            }
            // It may have ended in between, with this call not yet waiting.
            match self.signed_in.get() {
                Some(signed_in) => Poll::Ready(signed_in.as_ref()),
                None => Poll::Pending,
            }
        })
        .await
    }
}

/// Which kept answers a client hands out instead of asking the server.
#[derive(Clone, Copy, PartialEq)]
enum HandsOut {
    Fresh,
    /// Aged ones too, for a page that shows them and then asks again.
    AgedToo,
    Nothing,
}

/// Runs `work`, which takes a while, on the runtime's blocking threads, so
/// that the caller's, the app's UI thread, stays free.
#[cfg(not(target_family = "wasm"))]
async fn in_background<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .expect("the work doesn't panic")
}

/// The browser has no threads to spare, so `work` runs at once.
#[cfg(target_family = "wasm")]
async fn in_background<T>(work: impl FnOnce() -> T) -> T {
    work()
}

/// Sends `request` from the runtime's threads. Connecting checks the
/// server's certificate, which on Android can go online to see whether it
/// was revoked, and the app awaits its calls on its UI thread.
#[cfg(not(target_family = "wasm"))]
async fn exchange(request: reqwest::RequestBuilder) -> reqwest::Result<reqwest::Response> {
    tokio::spawn(request.send())
        .await
        .expect("the runtime outlives the app's calls")
}

#[cfg(target_family = "wasm")]
async fn exchange(request: reqwest::RequestBuilder) -> reqwest::Result<reqwest::Response> {
    request.send().await
}

enum Body {
    Json(Map<String, Value>),
    /// Data, still to be read.
    Binary(reqwest::Response),
}

/// A song's file as it arrives, from [`Client::stream`] or
/// [`Client::download`].
#[cfg(not(target_family = "wasm"))]
pub struct Stream(reqwest::Response);

#[cfg(not(target_family = "wasm"))]
impl Stream {
    /// The file's size in bytes, when the server says.
    pub fn size(&self) -> Option<u64> {
        self.0.content_length()
    }

    /// The media type the server named for the file, without parameters.
    pub fn content_type(&self) -> Option<String> {
        let value = self.0.headers().get(CONTENT_TYPE)?.to_str().ok()?;
        let media_type = value.split(';').next()?.trim();
        (!media_type.is_empty()).then(|| media_type.to_ascii_lowercase())
    }

    /// The next bytes, or none once all have come.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.0.chunk().await?.map(|bytes| bytes.to_vec()))
    }
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: Map<String, Value>,
}

/// The `subsonic-response` object of a successful response, or its error.
fn envelope(body: &[u8]) -> Result<Map<String, Value>> {
    let mut response = serde_json::from_slice::<Envelope>(body)?.response;
    if response.get("status").and_then(Value::as_str) == Some("ok") {
        return Ok(response);
    }
    let error = response.remove("error").unwrap_or(Value::Null);
    Err(Error::Api(serde_json::from_value(error)?))
}

/// Deserializes element `key` of a response. A missing element is `None`
/// when `T` is an `Option`, and an error otherwise.
fn take<T: DeserializeOwned>(
    mut response: Map<String, Value>,
    endpoint: &str,
    key: &str,
) -> Result<T> {
    match response.remove(key) {
        Some(value) => Ok(serde_json::from_value(value)?),
        None => serde_json::from_value(Value::Null)
            .map_err(|_| Error::Unexpected(format!("{endpoint} answered without `{key}`"))),
    }
}

fn token(password: &str, salt: &str) -> String {
    hex(&Md5::digest(format!("{password}{salt}")))
}

/// 128 random bits, as hex.
pub fn random_salt() -> Result<String> {
    Ok(format!(
        "{:016x}{:016x}",
        getrandom::u64()?,
        getrandom::u64()?
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Query parameters of one call, in order. Repeated names are allowed.
#[derive(Default)]
struct Params(Vec<(&'static str, String)>);

impl Params {
    fn new() -> Self {
        Self::default()
    }

    fn req(mut self, name: &'static str, value: impl ToString) -> Self {
        self.0.push((name, value.to_string()));
        self
    }

    fn opt(self, name: &'static str, value: Option<impl ToString>) -> Self {
        match value {
            Some(value) => self.req(name, value),
            None => self,
        }
    }

    fn list(mut self, name: &'static str, values: &[impl ToString]) -> Self {
        self.0.extend(values.iter().map(|v| (name, v.to_string())));
        self
    }
}

#[derive(Debug)]
pub enum Error {
    InvalidUrl(String),
    /// The request failed or the connection broke.
    Http(reqwest::Error),
    /// The server answered with this HTTP status and no Subsonic error.
    Status(u16),
    /// The server answered with a Subsonic error. Codes:
    /// <https://opensubsonic.netlify.app/docs/responses/error/>
    Api(ApiError),
    /// The JSON doesn't match the spec.
    Json(serde_json::Error),
    /// No secure random source for the token salt.
    Random(getrandom::Error),
    /// A password would have gone to a plain http server elsewhere on the
    /// network; nothing was sent.
    PlainHttp,
    /// The sign-in the client waited for failed; nothing was sent.
    NotSignedIn,
    Unexpected(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidUrl(reason) => write!(f, "invalid server address: {reason}"),
            Error::Status(status @ 300..=399) => {
                write!(
                    f,
                    "server address redirects (HTTP {status}); enter the address it leads to"
                )
            }
            Error::Http(e) => write!(f, "{e}"),
            Error::Status(status) => write!(f, "server answered HTTP {status}"),
            Error::Api(e) => write!(
                f,
                "server error {}: {}",
                e.code,
                e.message.as_deref().unwrap_or("no message")
            ),
            Error::Json(e) => write!(f, "unexpected response: {e}"),
            Error::Random(e) => write!(f, "no random source: {e}"),
            Error::PlainHttp => write!(f, "a password is only sent to an https address"),
            Error::NotSignedIn => write!(f, "not signed in: the server didn't answer"),
            Error::Unexpected(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            Error::Json(e) => Some(e),
            Error::Random(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        // The URL carries the credentials.
        Error::Http(e.without_url())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

impl From<getrandom::Error> for Error {
    fn from(e: getrandom::Error) -> Self {
        Error::Random(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn password() -> Auth {
        Auth::Password {
            username: "joe".into(),
            password: "sesame".into(),
        }
    }

    fn query(client: &Client) -> Vec<(String, String)> {
        let (request, _) = client
            .request("getAlbum", &Params::new().req("id", "7"))
            .unwrap();
        request
            .build()
            .unwrap()
            .url()
            .query_pairs()
            .into_owned()
            .collect()
    }

    fn get<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn token_matches_the_spec_example() {
        assert_eq!(
            token("sesame", "c19b2d"),
            "26719a1196d2a940705a59634eb18eab"
        );
    }

    #[test]
    fn server_address_keeps_its_path() {
        let rest = |s| Client::new(s, Auth::None).map(|c| c.rest.to_string());
        assert_eq!(rest("https://h").unwrap(), "https://h/rest/");
        assert_eq!(
            rest("https://h/navidrome").unwrap(),
            "https://h/navidrome/rest/"
        );
        assert_eq!(rest("http://h:4533/x/").unwrap(), "http://h:4533/x/rest/");
        assert!(rest("ftp://h").is_err());
        assert!(rest("h").is_err());
        assert!(rest("https://admin:hunter2@h").is_err());
        assert!(rest("https://admin@h").is_err());
    }

    #[test]
    fn password_methods_in_order() {
        let client = Client::new("https://h", password()).unwrap();
        let pairs = query(&client);
        let salt = get(&pairs, "s").unwrap();
        assert!(salt.len() >= 6);
        assert_eq!(get(&pairs, "t"), Some(token("sesame", salt).as_str()));
        assert_eq!(get(&pairs, "u"), Some("joe"));
        assert_eq!(get(&pairs, "id"), Some("7"));
        assert_eq!(get(&pairs, "f"), Some("json"));
        assert_ne!(
            get(&query(&client), "s"),
            Some(salt),
            "a fresh salt per request"
        );

        assert!(client.fall_back(TOKEN));
        assert_eq!(get(&query(&client), "p"), Some("enc:736573616d65"));
        assert!(client.fall_back(HEX));
        assert_eq!(get(&query(&client), "p"), Some("sesame"));
        assert!(!client.fall_back(PLAIN));

        let http = Client::new("http://h", password()).unwrap();
        assert!(!http.fall_back(TOKEN), "no readable password over http");
    }

    #[test]
    fn api_key_sends_no_username() {
        let client = Client::new("https://h", Auth::ApiKey("k".into())).unwrap();
        let pairs = query(&client);
        assert_eq!(get(&pairs, "apiKey"), Some("k"));
        assert_eq!(get(&pairs, "u"), None);
        assert!(!client.fall_back(TOKEN));
    }

    #[test]
    fn form_post_moves_parameters_into_the_body() {
        let client = Client::new("https://h", password()).unwrap();
        client.form_post.store(true, Relaxed);
        let (request, _) = client
            .request("star", &Params::new().list("id", &["1", "2"]))
            .unwrap();
        let request = request.build().unwrap();
        assert_eq!(request.method(), "POST");
        assert_eq!(request.url().as_str(), "https://h/rest/star");
        let body = std::str::from_utf8(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert!(
            body.contains("u=joe") && body.contains("id=1&id=2"),
            "{body}"
        );
    }

    #[test]
    fn errors_and_missing_elements() {
        let failed = br#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#;
        assert!(matches!(
            envelope(failed),
            Err(Error::Api(ApiError { code: 40, .. }))
        ));

        let ok = br#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
        let indexes: Option<Indexes> =
            take(envelope(ok).unwrap(), "getIndexes", "indexes").unwrap();
        assert_eq!(indexes, None);
        assert!(matches!(
            take::<Indexes>(envelope(ok).unwrap(), "getIndexes", "indexes"),
            Err(Error::Unexpected(_))
        ));
    }

    #[test]
    fn errors_never_show_the_request_url() {
        let error = reqwest::Client::new()
            .get("data:x?apiKey=SECRET&u=joe&t=TOKEN&s=SALT")
            .build()
            .unwrap_err();
        assert!(error.url().is_some());
        let error = Error::from(error);
        let shown = format!("{error} {error:?}");
        assert!(
            !["SECRET", "TOKEN", "SALT"]
                .iter()
                .any(|s| shown.contains(s)),
            "{shown}"
        );

        for address in [
            "https://admin:hunter2@h:99999",
            "ftp://admin:hunter2@h",
            "admin:hunter2@h",
        ] {
            let error = Client::new(address, Auth::None).err().unwrap();
            let shown = format!("{error} {error:?}");
            assert!(!shown.contains("hunter2"), "{shown}");
        }
    }

    #[test]
    fn extensions_are_asked_without_credentials() {
        for auth in [password(), Auth::ApiKey("k".into())] {
            let client = Client::new("https://h", auth).unwrap();
            let (request, _) = client
                .request("getOpenSubsonicExtensions", &Params::new())
                .unwrap();
            let pairs: Vec<_> = request
                .build()
                .unwrap()
                .url()
                .query_pairs()
                .into_owned()
                .collect();
            for name in ["u", "t", "s", "p", "apiKey"] {
                assert_eq!(get(&pairs, name), None, "{name}");
            }
        }
    }

    type Log = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A local HTTP server that answers every request with `answer(request)`
    /// and logs each request, head and body, as text.
    fn serve(answer: fn(&str) -> String) -> (String, Log) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let log = Log::default();
        let requests = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = BufReader::new(stream.unwrap());
                let (mut request, mut length) = (String::new(), 0);
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap();
                    }
                    request.push_str(&line);
                }
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                request = format!("{request}\r\n{}", String::from_utf8(body).unwrap());
                let response = answer(&request);
                requests.lock().unwrap().push(request);
                let _ = stream.get_mut().write_all(response.as_bytes());
            }
        });
        (address, log)
    }

    fn reply(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    fn json(body: &str) -> String {
        reply("200 OK", "Content-Type: application/json\r\n", body)
    }

    #[tokio::test]
    async fn a_song_streams_in_pieces_as_the_server_keeps_it() {
        let (server, log) = serve(|request| {
            if request.contains("id=gone") {
                json(
                    r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":70,"message":"Song not found"}}}"#,
                )
            } else {
                reply(
                    "200 OK",
                    "Content-Type: audio/flac\r\n",
                    "fLaC and the rest",
                )
            }
        });
        let client = Client::new(&server, Auth::ApiKey("SECRET".into())).unwrap();
        let mut stream = client.stream("tr-1", "raw", 0, None).await.unwrap();
        assert_eq!(stream.size(), Some(17));
        let mut all = Vec::new();
        while let Some(chunk) = stream.chunk().await.unwrap() {
            all.extend(chunk);
        }
        assert_eq!(all, b"fLaC and the rest");
        assert!(matches!(
            client.stream("gone", "raw", 0, None).await,
            Err(Error::Api(ApiError { code: 70, .. }))
        ));
        {
            let log = log.lock().unwrap();
            assert!(
                log[0].contains("id=tr-1") && log[0].contains("format=raw"),
                "{}",
                log[0]
            );
            assert!(!log[0].contains("maxBitRate"), "{}", log[0]);
            assert!(!log[0].contains("timeOffset"), "{}", log[0]);
        }
        client.stream("tr-1", "mp3", 192, Some(61.5)).await.unwrap();
        let log = log.lock().unwrap();
        assert!(
            log[2].contains("format=mp3")
                && log[2].contains("maxBitRate=192")
                && log[2].contains("timeOffset=61.5"),
            "{}",
            log[2]
        );
    }

    #[tokio::test]
    async fn a_song_downloads_as_the_original_and_says_its_type() {
        let (server, log) = serve(|request| {
            if request.contains("id=gone") {
                reply("403 Forbidden", "", "")
            } else {
                reply(
                    "200 OK",
                    "Content-Type: Audio/FLAC; charset=binary\r\n",
                    "fLaC and the rest",
                )
            }
        });
        let client = Client::new(&server, Auth::ApiKey("SECRET".into())).unwrap();
        let mut stream = client.download("tr-1").await.unwrap();
        assert_eq!(stream.content_type().as_deref(), Some("audio/flac"));
        assert_eq!(stream.size(), Some(17));
        let mut all = Vec::new();
        while let Some(chunk) = stream.chunk().await.unwrap() {
            all.extend(chunk);
        }
        assert_eq!(all, b"fLaC and the rest");
        assert!(matches!(
            client.download("gone").await,
            Err(Error::Status(403))
        ));
        let log = log.lock().unwrap();
        assert!(
            log[0].contains("/rest/download?") && log[0].contains("id=tr-1"),
            "{}",
            log[0]
        );
        assert!(!log[0].contains("format="), "{}", log[0]);
    }

    #[tokio::test]
    async fn kept_answers_are_handed_out_while_fresh() {
        use std::sync::atomic::AtomicU64;
        static NOW: AtomicU64 = AtomicU64::new(1000);
        static NAME: Mutex<&str> = Mutex::new("Night Harbour");
        let (server, log) = serve(|request| {
            let name = *NAME.lock().unwrap();
            json(&if request.contains("/rest/getAlbum?") {
                format!(
                    r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","album":{{"id":"a1","name":"{name}","songCount":1,"duration":1,"created":"2021-01-01T00:00:00Z","song":[{{"id":"s1","isDir":false,"title":"Song"}}]}}}}}}"#
                )
            } else {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#.to_owned()
            })
        });
        let asked = || log.lock().unwrap().len();
        let answers = Arc::new(Answers::new(|| NOW.load(Relaxed)));
        let client = Client::new(&server, Auth::ApiKey("SECRET".into()))
            .unwrap()
            .keeping_answers_in(answers);

        assert_eq!(
            client.get_album("a1").await.unwrap().album.name,
            "Night Harbour"
        );
        assert_eq!(asked(), 1);
        client.get_album("a1").await.unwrap();
        assert_eq!(asked(), 1, "a fresh answer is handed out");
        client.get_cover_art("al-a1", None).await.unwrap_err();
        client.get_cover_art("al-a1", None).await.unwrap_err();
        assert_eq!(asked(), 3, "covers aren't kept");

        NOW.store(1000 + FRESH_FOR, Relaxed);
        *NAME.lock().unwrap() = "Day Harbour";
        let reader = client.reader(true);
        assert_eq!(
            reader.get_album("a1").await.unwrap().album.name,
            "Night Harbour"
        );
        assert!(reader.gave_aged());
        assert_eq!(asked(), 3, "an aged answer shows at once");
        let again = client.reader(false);
        assert_eq!(
            again.get_album("a1").await.unwrap().album.name,
            "Day Harbour"
        );
        assert!(!again.gave_aged());
        assert!(again.heard_news());
        assert_eq!(asked(), 4);

        let same = client.reader(false);
        same.get_album("a1").await.unwrap();
        assert!(!same.heard_news(), "fresh again");
        client.star(&["s1"], &[], &[]).await.unwrap();
        assert_eq!(asked(), 5);
        client.get_album("a1").await.unwrap();
        assert_eq!(asked(), 6, "a heart outdates the album it is on");
        // One asked for anew is, though another is kept and fresh.
        client.asking_the_server().get_album("a1").await.unwrap();
        assert_eq!(asked(), 7);
    }

    #[tokio::test]
    async fn calls_wait_for_the_answers_saved_to_be_read_back() {
        let (server, log) = serve(|_| {
            json(
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","album":{"id":"a1","name":"From the server","songCount":0,"duration":0,"created":"2021-01-01T00:00:00Z"}}}"#,
            )
        });
        let first = Arc::new(Answers::new(|| 1000));
        let client = Client::new(&server, Auth::None).unwrap();
        let reading = client.keeping_answers_in(first.clone());
        reading.get_album("a1").await.unwrap();
        let saved = first
            .unsaved()
            .unwrap()
            .json
            .replace("From the server", "Kept");

        let answers = Arc::new(Answers::loading(|| 1000));
        let client = Client::new(&server, Auth::None).unwrap();
        let client = client.keeping_answers_in(answers.clone());
        let reader = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            answers.load(Some(&saved));
        });
        assert_eq!(client.get_album("a1").await.unwrap().album.name, "Kept");
        assert_eq!(log.lock().unwrap().len(), 1, "asked the first time only");
        reader.join().unwrap();
    }

    #[tokio::test]
    async fn a_client_waiting_for_a_sign_in_hands_out_what_is_kept_and_sends_through_it() {
        let (server, log) = serve(|request| {
            json(if request.contains("getOpenSubsonicExtensions") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","openSubsonicExtensions":[{"name":"formPost","versions":[1]}]}}"#
            } else if request.contains("getAlbum") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","album":{"id":"a1","name":"Kept","songCount":0,"duration":0,"created":"2021-01-01T00:00:00Z"}}}"#
            } else {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#
            })
        });
        let asked = || log.lock().unwrap().len();
        let answers = Arc::new(Answers::new(|| 1000));
        let keeping = Client::new(&server, Auth::None).unwrap();
        keeping
            .keeping_answers_in(answers.clone())
            .get_album("a1")
            .await
            .unwrap();
        let waiting_for = |sign_in: &Arc<SignIn>| {
            let client = Client::new(&server, password()).unwrap();
            client
                .keeping_answers_in(answers.clone())
                .waiting_for(sign_in.clone())
        };

        let sign_in = Arc::new(SignIn::default());
        let reader = waiting_for(&sign_in).reader(true);
        assert_eq!(reader.get_album("a1").await.unwrap().album.name, "Kept");
        assert!(!reader.supports("formPost"));
        assert!(!reader.sign_in_failed());
        let (pinged, ()) = tokio::join!(reader.ping(), async {
            assert_eq!(asked(), 1, "nothing is sent while signing in");
            let signed_in = Client::new(&server, password()).unwrap();
            signed_in.get_open_subsonic_extensions().await.unwrap();
            sign_in.end(Some(Arc::new(signed_in)));
        });
        pinged.unwrap();
        assert!(reader.supports("formPost") && !reader.sign_in_failed());
        let ping = log.lock().unwrap()[2].clone();
        let (head, body) = ping.split_once("\r\n\r\n").unwrap();
        assert!(
            head.starts_with("POST /rest/ping") && !head.contains("joe"),
            "{head}"
        );
        assert!(body.contains("u=joe"), "{body}");

        let failed = Arc::new(SignIn::default());
        failed.end(None);
        let reader = waiting_for(&failed).reader(true);
        assert_eq!(reader.get_album("a1").await.unwrap().album.name, "Kept");
        assert!(matches!(reader.ping().await, Err(Error::NotSignedIn)));
        assert_eq!(asked(), 3, "nothing is sent after a failed sign-in");
        assert!(reader.sign_in_failed());
    }

    /// Wakes the call it was handed to, by noting it.
    struct Woken(AtomicBool);

    impl std::task::Wake for Woken {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Relaxed);
        }
    }

    #[test]
    fn the_end_of_a_sign_in_wakes_the_calls_waiting_for_it() {
        let sign_in = SignIn::default();
        let woken = Arc::new(Woken(AtomicBool::new(false)));
        let waker = Waker::from(woken.clone());
        let mut context = std::task::Context::from_waker(&waker);
        let mut over = std::pin::pin!(sign_in.over());
        assert!(over.as_mut().poll(&mut context).is_pending());
        sign_in.end(None);
        assert!(woken.0.load(Relaxed), "nothing else polls it again");
        assert!(matches!(over.poll(&mut context), Poll::Ready(None)));
    }

    #[test]
    fn accounts_are_named_by_server_login_and_salt_without_saying_them() {
        let salt = random_salt().unwrap();
        assert_eq!(salt.len(), 32);
        let salted =
            |server: &str, auth: Auth, salt: &str| Client::new(server, auth).unwrap().account(salt);
        let account = |server: &str, auth: Auth| salted(server, auth, &salt);
        let joe = account("https://music.example.org", password());
        assert_eq!(joe, account("https://music.example.org/", password()));
        assert_ne!(joe, account("https://other.example.org", password()));
        let key = account("https://music.example.org", Auth::ApiKey("SECRET".into()));
        assert_ne!(joe, key);
        assert!(!key.contains("SECRET") && !joe.contains("joe"));
        let other_install = random_salt().unwrap();
        assert_ne!(
            joe,
            salted("https://music.example.org", password(), &other_install)
        );
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let (server, log) = serve(|_| reply("307 Temporary Redirect", "Location: /x\r\n", ""));
        let client = Client::new(&server, Auth::ApiKey("SECRET".into())).unwrap();
        assert!(matches!(client.ping().await, Err(Error::Status(307))));
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    /// The app awaits its calls on its UI thread, and on Android checking the
    /// server's certificate can itself go online. So a call connects and
    /// sends on the runtime's threads, not on its caller's.
    #[test]
    fn a_call_goes_out_while_its_caller_is_busy() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _inside = runtime.handle().enter();
        let (server, log) =
            serve(|_| json(r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#));
        let client = Client::new(&server, Auth::ApiKey("SECRET".into())).unwrap();
        std::thread::spawn(move || runtime.block_on(std::future::pending::<()>()));
        let mut ping = Box::pin(client.ping());
        let mut caller = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(ping.as_mut().poll(&mut caller).is_pending());
        std::thread::sleep(std::time::Duration::from_secs(1));
        assert_eq!(log.lock().unwrap().len(), 1, "the server got no call");
    }

    #[tokio::test]
    async fn credentials_go_in_the_body_once_form_post_is_known() {
        let (server, log) = serve(|request| {
            json(if request.contains("getOpenSubsonicExtensions") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","openSubsonicExtensions":[{"name":"formPost","versions":[1]}]}}"#
            } else {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#
            })
        });
        let client = Client::new(&server, Auth::ApiKey("SECRET".into())).unwrap();
        assert!(!client.supports("formPost"));
        client.get_open_subsonic_extensions().await.unwrap();
        assert!(client.supports("formPost") && !client.supports("songLyrics"));
        assert!(client.supports_version("formPost", 1) && !client.supports_version("formPost", 2));
        client.ping().await.unwrap();
        let log = log.lock().unwrap();
        assert!(!log[0].contains("SECRET"), "{}", log[0]);
        let (head, body) = log[1].split_once("\r\n\r\n").unwrap();
        assert!(
            head.starts_with("POST ") && !head.contains("SECRET"),
            "{head}"
        );
        assert!(body.contains("apiKey=SECRET"), "{body}");
    }

    #[tokio::test]
    async fn http_never_falls_back_to_the_password() {
        let (server, log) = serve(|_| {
            json(
                r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":41,"message":"Token authentication not supported"}}}"#,
            )
        });
        let client = Client::new(&server, password()).unwrap();
        assert!(matches!(
            client.ping().await,
            Err(Error::Api(ApiError { code: 41, .. }))
        ));
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert!(!log[0].contains("sesame"), "{}", log[0]);
    }

    #[tokio::test]
    async fn waveform_in_fonotecas_shape() {
        let (server, log) = serve(|request| {
            json(if request.contains("id=tr-1") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","waveform":{"level":[0.0123,0.2,0]}}}"#
            } else {
                r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":70,"message":"No such waveform."}}}"#
            })
        });
        let client = Client::new(&server, Auth::ApiKey("K".into())).unwrap();
        let waveform = client.get_waveform("tr-1").await.unwrap();
        assert_eq!(waveform.level, [0.0123, 0.2, 0.0]);
        assert!(matches!(
            client.get_waveform("tr-2").await,
            Err(Error::Api(ApiError { code: 70, .. }))
        ));
        assert!(log.lock().unwrap()[0].contains("/rest/getWaveform"));
    }

    #[tokio::test]
    async fn composers_and_compositions_in_fonotecas_shape() {
        let (server, log) = serve(|request| {
            json(if request.contains("getComposers") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","composers":{"index":[{"name":"B","composer":[{"id":"ar-1","name":"Ludwig van Beethoven","sortName":"Beethoven, Ludwig van","musicBrainzId":"1f9df192","coverArt":"ar-1","compositionCount":2}]}]}}}"#
            } else if request.contains("id=wk-5") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","compositions":{"composition":[{"id":"wk-5","name":"Symphony No. 5","composers":[{"id":"ar-1","name":"Ludwig van Beethoven"}],"composed":{"from":{"year":1804},"to":{"year":1808}},"albumCount":1,"coverArt":"al-9","movement":[{"name":"Allegro con brio","number":1}],"song":[{"id":"tr-1","isDir":false,"title":"Symphony No. 5: I. Allegro con brio","albumId":"al-9"}]}]}}}"#
            } else {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","compositions":{"composition":[{"id":"wk-5","name":"Symphony No. 5","composers":[{"id":"ar-1","name":"Ludwig van Beethoven"}],"albumCount":1,"form":"Symphony","key":"C minor","period":"Classical","movementCount":4,"duration":1840,"lyricists":[{"id":"","name":"Nobody"}],"languages":["zxx"],"starred":"2026-10-19T08:00:00Z"}]}}}"#
            })
        });
        let client = Client::new(&server, Auth::ApiKey("K".into())).unwrap();
        let composers = client.get_composers().await.unwrap();
        assert_eq!(composers.index[0].composer[0].composition_count, Some(2));
        let list = client.get_compositions(Some("ar-1")).await.unwrap();
        assert_eq!(
            list.composition[0].composers[0].name,
            "Ludwig van Beethoven"
        );
        let first = &list.composition[0];
        assert!(first.song.is_empty());
        assert_eq!(first.form.as_deref(), Some("Symphony"));
        assert_eq!(first.key.as_deref(), Some("C minor"));
        assert_eq!(first.period.as_deref(), Some("Classical"));
        assert_eq!(
            (first.movement_count, first.duration),
            (Some(4), Some(1840))
        );
        assert_eq!(first.lyricists[0].id, "");
        assert_eq!(first.languages, ["zxx"]);
        assert!(first.starred.is_some());
        let piece = client.get_composition("wk-5").await.unwrap();
        assert_eq!(piece.movement[0].number, Some(1));
        assert_eq!(piece.song[0].album_id.as_deref(), Some("al-9"));
        let years = piece.composed.unwrap();
        assert_eq!(years.to.and_then(|to| to.year), Some(1808));
        let log = log.lock().unwrap();
        assert!(log[1].contains("composerId=ar-1"), "{}", log[1]);
    }

    #[tokio::test]
    async fn playlist_folders_as_proposed() {
        let (server, log) = serve(|request| {
            json(if request.contains("getPlaylistFolders") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlistFolders":{"playlistFolder":[{"id":"folder-dj","name":"DJ Sets","sortOrder":1},{"id":"folder-2026","name":"2026","parentId":"folder-dj","sortOrder":1}]}}}"#
            } else if request.contains("createPlaylistFolder") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlistFolder":{"id":"folder-2026","name":"2026","parentId":"folder-dj","sortOrder":2}}}"#
            } else if request.contains("getPlaylists") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlists":{"playlist":[{"id":"800000075","name":"testcreate","songCount":1,"duration":304,"created":"2023-03-16T03:18:41+00:00","changed":"2023-03-16T03:18:41+00:00","playlistFolderId":"folder-dj","playlistFolderSortOrder":2}]}}}"#
            } else {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#
            })
        });
        let client = Client::new(&server, Auth::ApiKey("K".into())).unwrap();
        let folders = client.get_playlist_folders(None).await.unwrap();
        let inner = &folders.playlist_folder[1];
        assert_eq!(
            (inner.parent_id.as_deref(), inner.sort_order),
            (Some("folder-dj"), Some(1))
        );
        assert_eq!(folders.playlist_folder[0].parent_id, None);
        let made = client
            .create_playlist_folder("2026", Some("folder-dj"), None)
            .await
            .unwrap();
        assert_eq!(made.sort_order, Some(2));
        let playlist = &client.get_playlists(None).await.unwrap().playlist[0];
        assert_eq!(
            (
                playlist.playlist_folder_id.as_deref(),
                playlist.playlist_folder_sort_order
            ),
            (Some("folder-dj"), Some(2))
        );
        let to_root = PlaylistFolderUpdate {
            parent_id: Some(""),
            ..PlaylistFolderUpdate::default()
        };
        client
            .update_playlist_folder("folder-2026", &to_root)
            .await
            .unwrap();
        client
            .move_playlists(&["a", "b"], Some("folder-dj"), &[1, 2])
            .await
            .unwrap();
        client.move_playlists(&["a"], Some(""), &[]).await.unwrap();
        client.delete_playlist_folder("folder-2026").await.unwrap();
        let log = log.lock().unwrap();
        assert!(
            log[1].contains("name=2026&parentId=folder-dj") && !log[1].contains("sortOrder"),
            "{}",
            log[1]
        );
        assert!(
            log[3].contains("playlistFolderId=folder-2026&parentId= "),
            "{}",
            log[3]
        );
        assert!(
            log[4].contains(
                "playlistId=a&playlistId=b&playlistFolderId=folder-dj&sortOrder=1&sortOrder=2"
            ),
            "{}",
            log[4]
        );
        assert!(
            log[5].contains("playlistId=a&playlistFolderId= "),
            "{}",
            log[5]
        );
        assert!(
            log[6].contains("playlistFolderId=folder-2026"),
            "{}",
            log[6]
        );
    }

    #[tokio::test]
    async fn tags_in_fonotecas_shape() {
        let (server, log) = serve(|request| {
            json(if request.contains("getTags") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","tags":{"tag":[{"id":"tg-1","name":"Mood","songCount":3},{"id":"tg-2","name":"Calm","parentId":"tg-1","songCount":1}]}}}"#
            } else if request.contains("getTag") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","tag":{"id":"tg-1","name":"Mood","songCount":1,"song":[{"id":"tr-1","isDir":false,"title":"Calm Song","tagIds":["tg-2"]}]}}}"#
            } else if request.contains("createTag") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","tag":{"id":"tg-3","name":"Piano","songCount":0}}}"#
            } else {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#
            })
        });
        let client = Client::new(&server, Auth::ApiKey("K".into())).unwrap();
        let tags = client.get_tags().await.unwrap().tag;
        assert_eq!(
            (tags[1].parent_id.as_deref(), tags[1].song_count),
            (Some("tg-1"), 1)
        );
        let mood = client.get_tag("tg-1").await.unwrap();
        assert_eq!(mood.tag.name, "Mood");
        assert_eq!(mood.song[0].tag_ids, ["tg-2"]);
        assert_eq!(client.create_tag("Piano", None).await.unwrap().id, "tg-3");
        let update = TagUpdate {
            parent_id: Some(""),
            song_ids_to_add: &["tr-1", "tr-2"],
            song_ids_to_remove: &["tr-3"],
            ..TagUpdate::default()
        };
        client.update_tag("tg-3", &update).await.unwrap();
        client.delete_tag("tg-3").await.unwrap();
        let log = log.lock().unwrap();
        assert!(log[1].contains("id=tg-1"), "{}", log[1]);
        assert!(
            log[2].contains("name=Piano") && !log[2].contains("parentId"),
            "{}",
            log[2]
        );
        assert!(
            log[3].contains(
                "tagId=tg-3&parentId=&songIdToAdd=tr-1&songIdToAdd=tr-2&songIdToRemove=tr-3"
            ),
            "{}",
            log[3]
        );
        assert!(
            log[4].contains("deleteTag?") && log[4].contains("tagId=tg-3"),
            "{}",
            log[4]
        );
    }

    #[test]
    fn passwords_go_only_over_https_or_to_this_machine() {
        let may = |server: &str| {
            Client::new(server, Auth::ApiKey("K".into()))
                .unwrap()
                .may_send_password()
                .is_ok()
        };
        for server in [
            "https://music.example.org",
            "http://localhost:4533",
            "http://127.0.0.1:4533",
            "http://[::1]:4533",
        ] {
            assert!(may(server), "{server}");
        }
        for server in [
            "http://music.example.org",
            "http://192.168.1.10:4533",
            "http://localhost.example.org",
        ] {
            assert!(!may(server), "{server}");
        }
    }

    #[tokio::test]
    async fn no_password_call_goes_to_plain_http() {
        let client = Client::new("http://music.example.org", Auth::ApiKey("K".into())).unwrap();
        let refused = |result: Result<()>| matches!(result, Err(Error::PlainHttp));
        assert!(refused(client.change_password("joe", "new").await));
        assert!(refused(
            client
                .create_user("ann", "new", "a@example.org", &Roles::default(), &[])
                .await
        ));
        let update = UserUpdate {
            password: Some("new"),
            ..UserUpdate::default()
        };
        assert!(refused(client.update_user("ann", &update).await));
    }

    #[tokio::test]
    async fn users_in_navidromes_shape() {
        let (server, log) = serve(|request| {
            json(if request.contains("getOpenSubsonicExtensions") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","openSubsonicExtensions":[{"name":"formPost","versions":[1]}]}}"#
            } else if request.contains("getUsers") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","users":{"user":[{"username":"admin","scrobblingEnabled":true,"adminRole":true,"settingsRole":false,"downloadRole":true,"uploadRole":false,"playlistRole":false,"coverArtRole":true,"commentRole":false,"podcastRole":false,"streamRole":true,"jukeboxRole":false,"shareRole":true,"videoConversionRole":false,"folder":[1]}]}}}"#
            } else if request.contains("getUser") {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","user":{"username":"admin","scrobblingEnabled":true,"adminRole":true,"settingsRole":false,"downloadRole":true,"uploadRole":false,"playlistRole":false,"coverArtRole":true,"commentRole":false,"podcastRole":false,"streamRole":true,"jukeboxRole":false,"shareRole":true,"videoConversionRole":false,"folder":[1]}}}"#
            } else {
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#
            })
        });
        let client = Client::new(&server, Auth::ApiKey("K".into())).unwrap();
        client.get_open_subsonic_extensions().await.unwrap();
        let admin = client.get_user("admin").await.unwrap();
        assert!(admin.admin_role && !admin.settings_role && admin.share_role);
        assert_eq!((admin.folder, admin.email), (vec![1], None));
        assert_eq!(client.get_users().await.unwrap().user[0].username, "admin");
        let roles = Roles {
            admin: Some(false),
            download: Some(true),
            ..Roles::default()
        };
        client
            .create_user("ann", "s3cret&new", "ann@example.org", &roles, &[1, 3])
            .await
            .unwrap();
        let update = UserUpdate {
            email: Some("a@example.org"),
            ..UserUpdate::default()
        };
        client.update_user("ann", &update).await.unwrap();
        client.change_password("ann", "other").await.unwrap();
        client.delete_user("ann").await.unwrap();
        // Each role under its own name.
        let names = [
            "adminRole",
            "settingsRole",
            "streamRole",
            "downloadRole",
            "uploadRole",
            "coverArtRole",
            "commentRole",
            "podcastRole",
            "shareRole",
            "jukeboxRole",
            "videoConversionRole",
        ];
        for i in 0..names.len() {
            let on = |j: usize| (i == j).then_some(true);
            let roles = Roles {
                admin: on(0),
                settings: on(1),
                stream: on(2),
                download: on(3),
                upload: on(4),
                cover_art: on(5),
                comment: on(6),
                podcast: on(7),
                share: on(8),
                jukebox: on(9),
                video_conversion: on(10),
            };
            let update = UserUpdate {
                roles,
                ..UserUpdate::default()
            };
            client.update_user("ann", &update).await.unwrap();
        }
        let log = log.lock().unwrap();
        for (name, request) in names.iter().zip(&log[7..]) {
            let body = request.split_once("\r\n\r\n").unwrap().1;
            assert!(
                body.contains(&format!("{name}=true")) && body.matches("Role=").count() == 1,
                "{name} in {body}"
            );
        }
        assert!(log[1].contains("username=admin"), "{}", log[1]);
        let (head, create) = log[3].split_once("\r\n\r\n").unwrap();
        assert!(
            head.starts_with("POST ") && !head.contains("s3cret"),
            "{head}"
        );
        for part in [
            "username=ann",
            "password=s3cret%26new",
            "email=ann%40example.org",
            "adminRole=false",
            "downloadRole=true",
            "musicFolderId=1&musicFolderId=3",
        ] {
            assert!(create.contains(part), "{part} in {create}");
        }
        assert!(!create.contains("settingsRole"), "{create}");
        let update = log[4].split_once("\r\n\r\n").unwrap().1;
        assert!(update.contains("email=a%40example.org"), "{update}");
        assert!(
            !update.contains("password") && !update.contains("Role"),
            "{update}"
        );
        assert!(log[5].contains("password=other"), "{}", log[5]);
        assert!(log[6].contains("deleteUser"), "{}", log[6]);
    }

    #[tokio::test]
    async fn radio_catalogue_in_fonotecas_shape() {
        let (server, log) = serve(|_| {
            json(
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","radioCatalogue":{"internetRadioStation":[{"id":"rc-npo.xspf","name":"NPO","streamUrl":"https://example.org/npo","homePageUrl":"https://npo.nl","coverArt":"rc-npo.xspf","countryCode":"NL","languageCodes":["nl"],"genres":["Pop"]},{"id":"rc-old.xspf","name":"Old","streamUrl":"https://example.org/old","coverArt":"rc-old.xspf"}]}}}"#,
            )
        });
        let client = Client::new(&server, Auth::ApiKey("K".into())).unwrap();
        let list = client
            .get_radio_catalogue()
            .await
            .unwrap()
            .internet_radio_station;
        assert_eq!(list[0].country_code.as_deref(), Some("NL"));
        assert_eq!((list[0].language_codes.len(), list[0].genres.len()), (1, 1));
        assert_eq!(list[1].country_code, None);
        assert!(list[1].genres.is_empty());
        assert!(log.lock().unwrap()[0].contains("/rest/getRadioCatalogue"));
    }

    #[tokio::test]
    async fn radio_stations_in_navidromes_shape() {
        let (server, log) = serve(|request| {
            json(
                if request.contains("getInternetRadioStations") && request.contains("first") {
                    r#"{"subsonic-response":{"status":"ok","version":"1.16.1","internetRadioStations":{}}}"#
                } else if request.contains("getInternetRadioStations") {
                    r#"{"subsonic-response":{"status":"ok","version":"1.16.1","internetRadioStations":{"internetRadioStation":[{"id":"1QTg","name":"Probe","streamUrl":"http://example.org/s.mp3","coverArt":""}]}}}"#
                } else {
                    r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#
                },
            )
        });
        let client = Client::new(&server, Auth::ApiKey("first".into())).unwrap();
        assert!(
            client
                .get_internet_radio_stations()
                .await
                .unwrap()
                .internet_radio_station
                .is_empty()
        );
        let client = Client::new(&server, Auth::ApiKey("K".into())).unwrap();
        let station = &client
            .get_internet_radio_stations()
            .await
            .unwrap()
            .internet_radio_station[0];
        assert_eq!(
            (station.name.as_str(), station.home_page_url.as_deref()),
            ("Probe", None)
        );
        let probe = RadioStation {
            name: "Probe",
            stream_url: "http://example.org/s.mp3",
            homepage_url: Some("https://example.org/"),
        };
        client.create_internet_radio_station(&probe).await.unwrap();
        client
            .update_internet_radio_station("1QTg", &probe)
            .await
            .unwrap();
        client.delete_internet_radio_station("1QTg").await.unwrap();
        let log = log.lock().unwrap();
        assert!(
            log[2].contains("name=Probe&streamUrl=http%3A%2F%2Fexample.org%2Fs.mp3&homepageUrl=https%3A%2F%2Fexample.org%2F"),
            "{}",
            log[2]
        );
        assert!(log[3].contains("id=1QTg&name=Probe"), "{}", log[3]);
        assert!(
            log[4].contains("deleteInternetRadioStation?") && log[4].contains("id=1QTg"),
            "{}",
            log[4]
        );
    }
}
