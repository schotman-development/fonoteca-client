//! Client for the [OpenSubsonic API](https://opensubsonic.netlify.app): the
//! calls for browsing a music library, searching it, starring and rating, and
//! managing playlists. Responses are requested as JSON.
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
        })
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
        client.get_open_subsonic_extensions().await.unwrap();
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
}
