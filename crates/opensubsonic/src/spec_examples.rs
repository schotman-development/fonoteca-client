//! Parses every JSON example on the spec pages of the calls and types this
//! crate covers. `spec-examples/` holds them, copied from
//! <https://github.com/opensubsonic/open-subsonic-api> at be131fb44a48 under
//! its Apache-2.0 license. Eleven examples had JSON typos (trailing or missing
//! commas, an extra brace); their copies fix only those.
//!
//! Left out: the getIndexes and indexes examples, which give numbers and
//! booleans as strings and a one-item list as an object, against their own
//! field tables; three partial lyrics snippets on the getLyricsBySongId page;
//! the tokenInfo type example, which repeats the endpoint's; and the getUser,
//! getUsers, user and users examples, which give booleans (and in the type
//! examples folder IDs) as strings and leave out the required
//! videoConversionRole.

use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::*;

fn endpoint(name: &str, body: &[u8]) -> Result<()> {
    let response = envelope(body)?;
    fn check<T: DeserializeOwned>(
        response: Map<String, Value>,
        name: &str,
        key: &str,
    ) -> Result<()> {
        take::<T>(response, name, key).map(drop)
    }
    match name {
        "ping" => serde_json::from_value::<ServerInfo>(Value::Object(response))
            .map(drop)
            .map_err(Into::into),
        "getLicense" => check::<License>(response, name, "license"),
        "getOpenSubsonicExtensions" => {
            check::<Vec<OpenSubsonicExtension>>(response, name, "openSubsonicExtensions")
        }
        "tokenInfo" => check::<TokenInfo>(response, name, "tokenInfo"),
        "getMusicFolders" => check::<MusicFolders>(response, name, "musicFolders"),
        "getIndexes" => check::<Indexes>(response, name, "indexes"),
        "getMusicDirectory" => check::<Directory>(response, name, "directory"),
        "getGenres" => check::<Genres>(response, name, "genres"),
        "getArtists" => check::<ArtistsID3>(response, name, "artists"),
        "getArtist" => check::<ArtistWithAlbumsID3>(response, name, "artist"),
        "getAlbum" => check::<AlbumID3WithSongs>(response, name, "album"),
        "getSong" => check::<Child>(response, name, "song"),
        "getArtistInfo" => check::<ArtistInfo>(response, name, "artistInfo"),
        "getArtistInfo2" => check::<ArtistInfo2>(response, name, "artistInfo2"),
        "getAlbumInfo" | "getAlbumInfo2" => check::<AlbumInfo>(response, name, "albumInfo"),
        "getSimilarSongs" => check::<SimilarSongs>(response, name, "similarSongs"),
        "getSimilarSongs2" => check::<SimilarSongs2>(response, name, "similarSongs2"),
        "getTopSongs" => check::<TopSongs>(response, name, "topSongs"),
        "getAlbumList" => check::<AlbumList>(response, name, "albumList"),
        "getAlbumList2" => check::<AlbumList2>(response, name, "albumList2"),
        "getRandomSongs" => check::<Songs>(response, name, "randomSongs"),
        "getSongsByGenre" => check::<Songs>(response, name, "songsByGenre"),
        "getNowPlaying" => check::<NowPlaying>(response, name, "nowPlaying"),
        "getStarred" => check::<Starred>(response, name, "starred"),
        "getStarred2" => check::<Starred2>(response, name, "starred2"),
        "search2" => check::<SearchResult2>(response, name, "searchResult2"),
        "search3" => check::<SearchResult3>(response, name, "searchResult3"),
        "getPlaylists" => check::<Playlists>(response, name, "playlists"),
        "getPlaylist" => check::<PlaylistWithSongs>(response, name, "playlist"),
        "createPlaylist" => check::<Option<PlaylistWithSongs>>(response, name, "playlist"),
        "getLyrics" => check::<Lyrics>(response, name, "lyrics"),
        "getLyricsBySongId" => check::<LyricsList>(response, name, "lyricsList"),
        "getSonicSimilarTracks" | "findSonicPath" => {
            check::<Vec<SonicMatch>>(response, name, "sonicMatch")
        }
        "getScanStatus" | "startScan" => check::<ScanStatus>(response, name, "scanStatus"),
        "getInternetRadioStations" => {
            check::<InternetRadioStations>(response, name, "internetRadioStations")
        }
        "updatePlaylist"
        | "deletePlaylist"
        | "star"
        | "unstar"
        | "setRating"
        | "createInternetRadioStation"
        | "updateInternetRadioStation"
        | "deleteInternetRadioStation"
        | "createUser"
        | "updateUser"
        | "deleteUser"
        | "changePassword" => Ok(()),
        _ => panic!("no check for endpoint {name}"),
    }
}

fn response(name: &str, json: &[u8]) -> Result<()> {
    fn parse<T: DeserializeOwned>(json: &[u8]) -> Result<()> {
        let result = if json.trim_ascii_start().starts_with(b"[") {
            serde_json::from_slice::<Vec<T>>(json).map(drop)
        } else {
            serde_json::from_slice::<T>(json).map(drop)
        };
        result.map_err(Into::into)
    }
    match name {
        "Agent" => parse::<Agent>(json),
        "AlbumID3" => parse::<AlbumID3>(json),
        "AlbumID3WithSongs" => parse::<AlbumID3WithSongs>(json),
        "AlbumInfo" => parse::<AlbumInfo>(json),
        "AlbumList" => parse::<AlbumList>(json),
        "AlbumList2" => parse::<AlbumList2>(json),
        "ApiError" => parse::<ApiError>(json),
        "Artist" => parse::<Artist>(json),
        "ArtistID3" => parse::<ArtistID3>(json),
        "ArtistInfo" => parse::<ArtistInfo>(json),
        "ArtistInfo2" => parse::<ArtistInfo2>(json),
        "ArtistWithAlbumsID3" => parse::<ArtistWithAlbumsID3>(json),
        "ArtistsID3" => parse::<ArtistsID3>(json),
        "Child" => parse::<Child>(json),
        "Contributor" => parse::<Contributor>(json),
        "Cue" => parse::<Cue>(json),
        "CueLine" => parse::<CueLine>(json),
        "Directory" => parse::<Directory>(json),
        "DiscTitle" => parse::<DiscTitle>(json),
        "Genre" => parse::<Genre>(json),
        "Genres" => parse::<Genres>(json),
        "Index" => parse::<Index>(json),
        "IndexID3" => parse::<IndexID3>(json),
        "InternetRadioStation" => parse::<InternetRadioStation>(json),
        "InternetRadioStations" => parse::<InternetRadioStations>(json),
        "Indexes" => parse::<Indexes>(json),
        "ItemDate" => parse::<ItemDate>(json),
        "ItemGenre" => parse::<ItemGenre>(json),
        "License" => parse::<License>(json),
        "Line" => parse::<Line>(json),
        "Lyrics" => parse::<Lyrics>(json),
        "LyricsList" => parse::<LyricsList>(json),
        "Movement" => parse::<Movement>(json),
        "MusicFolder" => parse::<MusicFolder>(json),
        "MusicFolders" => parse::<MusicFolders>(json),
        "NowPlaying" => parse::<NowPlaying>(json),
        "NowPlayingEntry" => parse::<NowPlayingEntry>(json),
        "OpenSubsonicExtension" => parse::<OpenSubsonicExtension>(json),
        "Playlist" => parse::<Playlist>(json),
        "PlaylistWithSongs" => parse::<PlaylistWithSongs>(json),
        "Playlists" => parse::<Playlists>(json),
        "RecordLabel" => parse::<RecordLabel>(json),
        "ReplayGain" => parse::<ReplayGain>(json),
        "ScanStatus" => parse::<ScanStatus>(json),
        "SearchResult2" => parse::<SearchResult2>(json),
        "SearchResult3" => parse::<SearchResult3>(json),
        "SimilarSongs" => parse::<SimilarSongs>(json),
        "SimilarSongs2" => parse::<SimilarSongs2>(json),
        "Songs" => parse::<Songs>(json),
        "SonicMatch" => parse::<SonicMatch>(json),
        "Starred" => parse::<Starred>(json),
        "Starred2" => parse::<Starred2>(json),
        "StructuredLyrics" => parse::<StructuredLyrics>(json),
        "TokenInfo" => parse::<TokenInfo>(json),
        "TopSongs" => parse::<TopSongs>(json),
        "Work" => parse::<Work>(json),
        _ => panic!("no check for type {name}"),
    }
}

#[test]
fn spec_examples_parse() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("spec-examples");
    let (mut count, mut failures) = (0, Vec::new());
    for kind in ["endpoints", "responses"] {
        for entry in fs::read_dir(dir.join(kind)).unwrap() {
            let path = entry.unwrap().path();
            let file = path.file_name().unwrap().to_str().unwrap().to_owned();
            let name = file.split('.').next().unwrap();
            let json = fs::read(&path).unwrap();
            let result = if kind == "endpoints" {
                endpoint(name, &json)
            } else {
                response(name, &json)
            };
            count += 1;
            if let Err(e) = result {
                failures.push(format!("{kind}/{file}: {e}"));
            }
        }
    }
    failures.sort();
    assert!(
        failures.is_empty(),
        "{} of {count} examples failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
