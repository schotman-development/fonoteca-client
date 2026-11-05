//! The playlists ListenBrainz makes for a user, read as JSPF.

use serde::Deserialize;

use crate::{Client, Result, send};

/// What ListenBrainz calls the algorithm behind the weekly playlists of new
/// music, which it lists beside the others it creates for a user.
const EXPLORATION_PATCH: &str = "weekly-exploration";

/// A playlist, without its tracks when it comes from a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub mbid: String,
    pub title: String,
    /// What ListenBrainz says the playlist is, as HTML.
    pub annotation: Option<String>,
    /// The algorithm that made it, when ListenBrainz made it.
    pub source_patch: Option<String>,
    /// When it was made, as ISO 8601.
    pub date: Option<String>,
    pub tracks: Vec<PlaylistTrack>,
}

impl Playlist {
    /// Whether it is one of the weekly playlists of music the user hasn't
    /// heard.
    pub fn is_exploration(&self) -> bool {
        self.source_patch.as_deref() == Some(EXPLORATION_PATCH)
    }

    /// The day it was made, counted from 1970-01-01, to tell how many days
    /// apart two playlists are.
    pub fn day_number(&self) -> Option<i64> {
        let mut parts = self.date.as_deref()?.get(..10)?.split('-');
        let mut next = || parts.next()?.parse::<i64>().ok();
        let (year, month, day) = (next()?, next()?, next()?);
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        // Counts the days of whole years from March, when the leap day ends the year.
        let year = if month <= 2 { year - 1 } else { year };
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        Some(era * 146_097 + day_of_era - 719_468)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistTrack {
    pub recording_mbid: Option<String>,
    pub title: String,
    /// The artists as one credit, as ListenBrainz joins them.
    pub artist: String,
    /// The names in that credit, one for each artist.
    pub artists: Vec<String>,
    /// The MusicBrainz ID of each of `artists`, where ListenBrainz has it.
    pub artist_mbids: Vec<Option<String>>,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Deserialize)]
struct Jspf {
    playlist: JspfPlaylist,
}

#[derive(Deserialize)]
struct JspfList {
    playlists: Vec<Jspf>,
}

#[derive(Deserialize)]
struct JspfPlaylist {
    title: String,
    identifier: String,
    annotation: Option<String>,
    date: Option<String>,
    #[serde(default)]
    extension: Extension,
    #[serde(default)]
    track: Vec<JspfTrack>,
}

#[derive(Deserialize, Default)]
struct Extension {
    #[serde(default, rename = "https://musicbrainz.org/doc/jspf#playlist")]
    playlist: PlaylistMetadata,
}

#[derive(Deserialize, Default)]
struct PlaylistMetadata {
    #[serde(default)]
    additional_metadata: AlgorithmParent,
}

#[derive(Deserialize, Default)]
struct AlgorithmParent {
    #[serde(default)]
    algorithm_metadata: Algorithm,
}

#[derive(Deserialize, Default)]
struct Algorithm {
    source_patch: Option<String>,
}

#[derive(Deserialize)]
struct JspfTrack {
    title: String,
    creator: Option<String>,
    album: Option<String>,
    duration: Option<u64>,
    #[serde(default)]
    identifier: Vec<String>,
    #[serde(default)]
    extension: TrackExtension,
}

#[derive(Deserialize, Default)]
struct TrackExtension {
    #[serde(default, rename = "https://musicbrainz.org/doc/jspf#track")]
    track: TrackMetadata,
}

#[derive(Deserialize, Default)]
struct TrackMetadata {
    #[serde(default)]
    additional_metadata: Credits,
}

#[derive(Deserialize, Default)]
struct Credits {
    #[serde(default)]
    artists: Vec<Credit>,
}

#[derive(Deserialize)]
pub(crate) struct Credit {
    pub(crate) artist_credit_name: String,
    artist_mbid: Option<String>,
}

/// The last part of an address such as
/// `https://musicbrainz.org/recording/<mbid>`.
fn mbid_of(address: &str) -> Option<String> {
    address
        .rsplit('/')
        .next()
        .filter(|mbid| !mbid.is_empty())
        .map(str::to_owned)
}

impl From<Jspf> for Playlist {
    fn from(jspf: Jspf) -> Self {
        let jspf = jspf.playlist;
        let tracks = jspf.track.into_iter().map(PlaylistTrack::from).collect();
        Playlist {
            mbid: mbid_of(&jspf.identifier).unwrap_or_default(),
            title: jspf.title,
            annotation: jspf.annotation,
            date: jspf.date,
            source_patch: jspf
                .extension
                .playlist
                .additional_metadata
                .algorithm_metadata
                .source_patch,
            tracks,
        }
    }
}

impl From<JspfTrack> for PlaylistTrack {
    fn from(track: JspfTrack) -> Self {
        let credits = track.extension.track.additional_metadata.artists;
        let artist = track.creator.unwrap_or_default();
        let (artists, artist_mbids) = if credits.is_empty() {
            (vec![artist.clone()], vec![None])
        } else {
            (credits.into_iter())
                .map(|c| (c.artist_credit_name, c.artist_mbid))
                .unzip()
        };
        PlaylistTrack {
            recording_mbid: track.identifier.first().and_then(|id| mbid_of(id)),
            title: track.title,
            artist,
            artists,
            artist_mbids,
            album: track.album,
            duration_ms: track.duration,
        }
    }
}

impl Client {
    /// The playlists ListenBrainz made for `user`, newest first and without
    /// their tracks; <https://listenbrainz.readthedocs.io/en/latest/users/api/playlist.html>
    /// says they are all public.
    pub async fn playlists_created_for(&self, user: &str) -> Result<Vec<Playlist>> {
        let url = self.url(&["user", user, "playlists", "createdfor"]);
        let list: JspfList = send(self.authorized(self.http.get(url))).await?;
        Ok(list.playlists.into_iter().map(Playlist::from).collect())
    }

    /// The playlist with this ID and its tracks.
    pub async fn playlist(&self, mbid: &str) -> Result<Playlist> {
        let url = self.url(&["playlist", mbid]);
        let jspf: Jspf = send(self.authorized(self.http.get(url))).await?;
        Ok(jspf.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"{"count":2,"offset":0,"playlist_count":2,"playlists":[
      {"playlist":{"title":"Daily Jams for rob","identifier":"https://listenbrainz.org/playlist/a9b9f76f-7e1f-427b-9244-68dcf26cb43f",
        "annotation":"<p>Made for you</p>","date":"2026-11-04T23:02:06.186375+00:00","track":[],
        "extension":{"https://musicbrainz.org/doc/jspf#playlist":{"created_for":"rob",
          "additional_metadata":{"algorithm_metadata":{"source_patch":"daily-jams"}}}}}},
      {"playlist":{"title":"Weekly Exploration for rob","identifier":"https://listenbrainz.org/playlist/55f2a01f-3974-46ca-8461-a8a84886eba4",
        "extension":{"https://musicbrainz.org/doc/jspf#playlist":{
          "additional_metadata":{"algorithm_metadata":{"source_patch":"weekly-exploration"}}}}}}]}"#;

    #[test]
    fn a_list_of_playlists_is_read_with_the_algorithm_behind_each() {
        let list: JspfList = serde_json::from_str(LIST).unwrap();
        let playlists: Vec<Playlist> = list.playlists.into_iter().map(Playlist::from).collect();
        assert_eq!(playlists[0].mbid, "a9b9f76f-7e1f-427b-9244-68dcf26cb43f");
        assert_eq!(playlists[0].title, "Daily Jams for rob");
        assert_eq!(
            playlists[0].annotation.as_deref(),
            Some("<p>Made for you</p>")
        );
        assert_eq!(playlists[0].source_patch.as_deref(), Some("daily-jams"));
        assert_eq!(
            playlists[0].date.as_deref(),
            Some("2026-11-04T23:02:06.186375+00:00")
        );
        assert_eq!(playlists[1].date, None);
        assert_eq!(playlists[1].annotation, None);
        assert!(playlists[1].tracks.is_empty());
    }

    #[test]
    fn only_the_weekly_exploration_is_exploration() {
        let list: JspfList = serde_json::from_str(LIST).unwrap();
        let exploration: Vec<bool> = (list.playlists.into_iter())
            .map(|jspf| Playlist::from(jspf).is_exploration())
            .collect();
        assert_eq!(exploration, [false, true]);
        let without_metadata: Jspf = serde_json::from_str(
            r#"{"playlist":{"title":"Mine","identifier":"https://listenbrainz.org/playlist/x"}}"#,
        )
        .unwrap();
        assert!(!Playlist::from(without_metadata).is_exploration());
    }

    #[test]
    fn a_playlist_has_its_tracks_with_recording_and_artist_credits() {
        let jspf: Jspf = serde_json::from_str(
            r#"{"playlist":{"title":"Jams","identifier":"https://listenbrainz.org/playlist/p",
              "track":[{"title":"RITUAL","creator":"Jon Hopkins, 7RAYS & Ishq","album":"RITUAL","duration":194880,
                "identifier":["https://musicbrainz.org/recording/31dad9e2-25fc-48aa-8c01-72c3729f53a1"],
                "extension":{"https://musicbrainz.org/doc/jspf#track":{"additional_metadata":{"artists":[
                  {"artist_credit_name":"Jon Hopkins","artist_mbid":"a1","join_phrase":", "},{"artist_credit_name":"7RAYS"},{"artist_credit_name":"Ishq"}]}}}},
               {"title":"Bare","creator":"Someone"}]}}"#,
        )
        .unwrap();
        let tracks = Playlist::from(jspf).tracks;
        assert_eq!(
            tracks[0].recording_mbid.as_deref(),
            Some("31dad9e2-25fc-48aa-8c01-72c3729f53a1")
        );
        assert_eq!(tracks[0].artist, "Jon Hopkins, 7RAYS & Ishq");
        assert_eq!(tracks[0].artists, ["Jon Hopkins", "7RAYS", "Ishq"]);
        assert_eq!(tracks[0].artist_mbids, [Some("a1".to_owned()), None, None]);
        assert_eq!(tracks[0].duration_ms, Some(194880));
        assert_eq!(tracks[1].recording_mbid, None);
        assert_eq!(tracks[1].artists, ["Someone"]);
        assert_eq!(tracks[1].artist_mbids, [None]);
    }

    #[test]
    fn the_playlist_urls_escape_the_user_name() {
        let client = Client::new("t").unwrap();
        let url = client.url(&["user", "a b", "playlists", "createdfor"]);
        assert_eq!(
            url.as_str(),
            "https://api.listenbrainz.org/1/user/a%20b/playlists/createdfor"
        );
    }
}
