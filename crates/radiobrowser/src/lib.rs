//! The [radio-browser.info](https://api.radio-browser.info/) directory of
//! internet radio stations: the most voted ones, of a genre, a country or a
//! genre in a language, and the names of the countries and languages. It
//! needs no account or key.
//!
//! radio-browser asks for a descriptive User-Agent, and for a click to be
//! counted only when a station starts playing. Nothing here counts clicks or
//! votes.

use std::fmt;

use reqwest::{RequestBuilder, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;

/// Every radio-browser server under one name, which DNS hands out in turn.
// ponytail: no retry on another server when one fails; pick from
// /json/servers and retry when one being down matters.
pub const API_ROOT: &str = "https://all.api.radio-browser.info/json/";

/// The largest logo fetched, in bytes; a bigger one is left out.
const LOGO_BYTES: usize = 512 * 1024;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A station, as far as the directory knows it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Station {
    pub stationuuid: String,
    pub name: String,
    /// The stream as the station gave it, which may be a playlist file.
    pub url: String,
    /// The stream itself, with playlists and redirects followed.
    pub url_resolved: String,
    pub homepage: String,
    /// A picture of the station, PNG or JPEG.
    pub favicon: String,
    /// Genres and other words, split by commas.
    pub tags: String,
    /// ISO 3166-1 alpha-2, in capitals.
    pub countrycode: String,
    pub votes: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Country {
    pub name: String,
    pub iso_3166_1: String,
}

/// A language as stations name it, in lower case, such as "dutch"; only
/// those with an ISO 639 code are languages proper.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Language {
    pub name: String,
    pub iso_639: Option<String>,
    pub stationcount: u32,
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

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.root.clone();
        url.path_segments_mut()
            .expect("the root is an http URL")
            .pop_if_empty()
            .extend(segments);
        url
    }

    /// Working stations with the most votes, `limit` of them from `offset`.
    pub async fn top_voted(&self, offset: u32, limit: u32) -> Result<Vec<Station>> {
        let url = self.url(&["stations", "topvote"]);
        let query = [
            ("hidebroken", "true".to_owned()),
            ("offset", offset.to_string()),
            ("limit", limit.to_string()),
        ];
        get(self.http.get(url).query(&query)).await
    }

    /// Working stations tagged `tag`, most votes first, `limit` of them from
    /// `offset`.
    pub async fn by_tag(&self, tag: &str, offset: u32, limit: u32) -> Result<Vec<Station>> {
        let url = self.url(&["stations", "bytagexact", tag]);
        let query = [
            ("order", "votes".to_owned()),
            ("reverse", "true".to_owned()),
            ("hidebroken", "true".to_owned()),
            ("offset", offset.to_string()),
            ("limit", limit.to_string()),
        ];
        get(self.http.get(url).query(&query)).await
    }

    /// Working stations tagged `tag` that speak `language` among others, most
    /// votes first, `limit` of them from `offset`.
    pub async fn by_tag_in(
        &self,
        tag: &str,
        language: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<Station>> {
        let url = self.url(&["stations", "search"]);
        let query = [
            ("tag", tag.to_owned()),
            ("tagExact", "true".to_owned()),
            ("language", language.to_owned()),
            ("languageExact", "true".to_owned()),
            ("order", "votes".to_owned()),
            ("reverse", "true".to_owned()),
            ("hidebroken", "true".to_owned()),
            ("offset", offset.to_string()),
            ("limit", limit.to_string()),
        ];
        get(self.http.get(url).query(&query)).await
    }

    /// Working stations of the country with the ISO 3166-1 code `code`, most
    /// votes first, `limit` of them from `offset`.
    pub async fn by_country(&self, code: &str, offset: u32, limit: u32) -> Result<Vec<Station>> {
        let url = self.url(&["stations", "bycountrycodeexact", code]);
        let query = [
            ("order", "votes".to_owned()),
            ("reverse", "true".to_owned()),
            ("hidebroken", "true".to_owned()),
            ("offset", offset.to_string()),
            ("limit", limit.to_string()),
        ];
        get(self.http.get(url).query(&query)).await
    }

    /// The stations named exactly `name`, most votes first, at most 20.
    pub async fn named(&self, name: &str) -> Result<Vec<Station>> {
        let url = self.url(&["stations", "search"]);
        let query = [
            ("name", name),
            ("nameExact", "true"),
            ("order", "votes"),
            ("reverse", "true"),
            ("limit", "20"),
        ];
        get(self.http.get(url).query(&query)).await
    }

    /// Every country with stations, with its name and code.
    pub async fn countries(&self) -> Result<Vec<Country>> {
        get(self.http.get(self.url(&["countries"]))).await
    }

    /// Every language working stations speak.
    pub async fn languages(&self) -> Result<Vec<Language>> {
        let url = self.url(&["languages"]);
        get(self.http.get(url).query(&[("hidebroken", "true")])).await
    }

    /// A station's picture from `address`, or `None` when it is larger than
    /// half a megabyte.
    pub async fn logo(&self, address: &str) -> Result<Option<Vec<u8>>> {
        let response = self.http.get(address).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|n| n > LOGO_BYTES as u64)
        {
            return Ok(None);
        }
        let bytes = response.bytes().await?;
        Ok((bytes.len() <= LOGO_BYTES).then(|| bytes.to_vec()))
    }
}

async fn get<T: DeserializeOwned>(request: RequestBuilder) -> Result<T> {
    Ok(request.send().await?.error_for_status()?.json().await?)
}

#[derive(Debug)]
pub enum Error {
    /// The request failed, the server refused it, or the JSON was unexpected.
    Http(reqwest::Error),
    /// The server's address isn't a URL.
    Address(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "{e}"),
            Error::Address(e) => write!(f, "not a server address: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            Error::Address(_) => None,
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
    fn tags_are_escaped_in_paths() {
        let client = Client::new("test/1").unwrap();
        let url = client.url(&["stations", "bytagexact", "hip hop/r&b"]);
        assert_eq!(
            url.as_str(),
            "https://all.api.radio-browser.info/json/stations/bytagexact/hip%20hop%2Fr&b"
        );
    }

    #[test]
    fn station_json() {
        let stations: Vec<Station> = serde_json::from_str(
            r#"[{"changeuuid":"x","stationuuid":"d1a54d2e","name":"Classic Vinyl HD",
                "url":"https://icecast.walmradio.com:8443/classic",
                "url_resolved":"https://icecast.walmradio.com:8443/classic",
                "homepage":"https://walmradio.com/classic",
                "favicon":"https://icecast.walmradio.com:8443/classic.jpg",
                "tags":"jazz,lounge","countrycode":"US","country":"The United States Of America",
                "votes":308442,"lastcheckok":1,"geo_lat":null},
               {"stationuuid":"e2","name":"Bare"}]"#,
        )
        .unwrap();
        assert_eq!(stations[0].countrycode, "US");
        assert_eq!(stations[0].votes, 308442);
        assert_eq!(stations[1].url_resolved, "");
    }
}
