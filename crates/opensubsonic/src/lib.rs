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

mod api;
mod model;
#[cfg(test)]
mod spec_examples;

pub use api::*;
pub use model::*;

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};

use md5::{Digest, Md5};
use reqwest::Url;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// The Subsonic API version this client implements.
pub const PROTOCOL_VERSION: &str = "1.16.1";
const CLIENT_NAME: &str = "fonoteca-client";

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
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
    extensions: Mutex<Vec<String>>,
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
        Ok(Self {
            http: http.build()?,
            rest,
            auth,
            password_method: AtomicU8::new(TOKEN),
            form_post: AtomicBool::new(false),
            extensions: Mutex::default(),
        })
    }

    /// Whether the server listed `extension` the last time
    /// [`Client::get_open_subsonic_extensions`] was called.
    pub fn supports(&self, extension: &str) -> bool {
        self.extensions
            .lock()
            .is_ok_and(|names| names.iter().any(|name| name == extension))
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
        loop {
            let (request, method) = self.request(endpoint, params)?;
            let response = request.send().await?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let body = response.bytes().await?;
            if content_type.contains("json") {
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
            return Ok(Body::Binary(body.to_vec()));
        }
    }

    async fn json(&self, endpoint: &str, params: &Params) -> Result<Map<String, Value>> {
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
        match self.send(endpoint, &params).await? {
            Body::Binary(data) => Ok(data),
            Body::Json(_) => Err(Error::Unexpected(format!(
                "{endpoint} answered with JSON instead of data"
            ))),
        }
    }
}

enum Body {
    Json(Map<String, Value>),
    Binary(Vec<u8>),
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
    async fn redirects_are_not_followed() {
        let (server, log) = serve(|_| reply("307 Temporary Redirect", "Location: /x\r\n", ""));
        let client = Client::new(&server, Auth::ApiKey("SECRET".into())).unwrap();
        assert!(matches!(client.ping().await, Err(Error::Status(307))));
        assert_eq!(log.lock().unwrap().len(), 1);
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
