//! A user's statistics: their top artists, albums and genres over a range of
//! time. ListenBrainz works them out a few times a day, and answers 204 for a
//! range it hasn't worked out yet.

use std::cmp::Reverse;
use std::collections::HashMap;

use reqwest::Url;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{Client, Result, exchange, send_checked};

/// Where the cover art of releases is kept, which ListenBrainz's own
/// statistics point to by release and image ID.
pub const COVER_ART_ROOT: &str = "https://coverartarchive.org/";

/// The width in pixels of the cover art thumbnails asked for; the archive
/// keeps 250, 500 and 1200.
const COVER_ART_WIDTH: u32 = 250;

/// The ranges of time ListenBrainz sums listens over that this app offers.
/// The `this_` ones are the calendar week, month and year so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    ThisWeek,
    ThisMonth,
    ThisYear,
    AllTime,
}

impl Range {
    fn as_str(self) -> &'static str {
        match self {
            Range::ThisWeek => "this_week",
            Range::ThisMonth => "this_month",
            Range::ThisYear => "this_year",
            Range::AllTime => "all_time",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TopArtist {
    pub artist_name: String,
    /// The MusicBrainz ID, which ListenBrainz may not know.
    #[serde(default)]
    pub artist_mbid: Option<String>,
    pub listen_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TopAlbum {
    pub release_group_name: String,
    pub artist_name: String,
    #[serde(default)]
    pub release_group_mbid: Option<String>,
    /// The cover art archive's image of the release `caa_release_mbid`, when
    /// it has one.
    #[serde(default)]
    pub caa_id: Option<u64>,
    #[serde(default)]
    pub caa_release_mbid: Option<String>,
    pub listen_count: u64,
}

/// How often a genre was heard in one hour of the day.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GenreActivity {
    pub genre: String,
    pub hour: u8,
    pub listen_count: u64,
}

/// ListenBrainz has no top genres of a user: its genre statistic is the
/// listens of each genre for each hour of the day, which `top_genres` adds up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopGenre {
    pub genre: String,
    pub listen_count: u64,
}

/// The `count` genres heard most, most first, and by name among equals.
fn top_genres_of(activity: Vec<GenreActivity>, count: usize) -> Vec<TopGenre> {
    let mut heard: HashMap<String, u64> = HashMap::new();
    for hour in activity {
        *heard.entry(hour.genre).or_default() += hour.listen_count;
    }
    let mut genres: Vec<TopGenre> = (heard.into_iter())
        .map(|(genre, listen_count)| TopGenre {
            genre,
            listen_count,
        })
        .collect();
    genres.sort_by(|a, b| {
        (Reverse(a.listen_count), &a.genre).cmp(&(Reverse(b.listen_count), &b.genre))
    });
    genres.truncate(count);
    genres
}

#[derive(Deserialize)]
struct Envelope<T> {
    payload: T,
}

#[derive(Deserialize)]
struct Artists {
    #[serde(default)]
    artists: Vec<TopArtist>,
}

#[derive(Deserialize)]
struct ReleaseGroups {
    #[serde(default)]
    release_groups: Vec<TopAlbum>,
}

#[derive(Deserialize)]
struct GenreHours {
    #[serde(default)]
    genre_activity: Vec<GenreActivity>,
}

impl Client {
    /// The `count` artists `user` played most in `range`, or `None` when
    /// ListenBrainz hasn't worked the statistic out yet.
    pub async fn top_artists(
        &self,
        user: &str,
        range: Range,
        count: u32,
    ) -> Result<Option<Vec<TopArtist>>> {
        let artists: Option<Artists> = self.statistic(user, "artists", range, Some(count)).await?;
        Ok(artists.map(|artists| artists.artists))
    }

    /// The `count` albums, as release groups, `user` played most in `range`,
    /// or `None` when ListenBrainz hasn't worked the statistic out yet.
    pub async fn top_albums(
        &self,
        user: &str,
        range: Range,
        count: u32,
    ) -> Result<Option<Vec<TopAlbum>>> {
        let albums: Option<ReleaseGroups> = self
            .statistic(user, "release-groups", range, Some(count))
            .await?;
        Ok(albums.map(|albums| albums.release_groups))
    }

    /// The `count` genres `user` played most in `range`, or `None` when
    /// ListenBrainz hasn't worked the statistic out yet.
    pub async fn top_genres(
        &self,
        user: &str,
        range: Range,
        count: usize,
    ) -> Result<Option<Vec<TopGenre>>> {
        let hours: Option<GenreHours> = self.statistic(user, "genre-activity", range, None).await?;
        Ok(hours.map(|hours| top_genres_of(hours.genre_activity, count)))
    }

    async fn statistic<T: DeserializeOwned>(
        &self,
        user: &str,
        kind: &str,
        range: Range,
        count: Option<u32>,
    ) -> Result<Option<T>> {
        let mut query = vec![("range", range.as_str().to_owned())];
        query.extend(count.map(|count| ("count", count.to_string())));
        let request = self
            .http
            .get(self.url(&["stats", "user", user, kind]))
            .query(&query);
        let response = send_checked(self.authorized(request)).await?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        Ok(Some(response.json::<Envelope<T>>().await?.payload))
    }

    /// The thumbnail of image `caa_id` of the release `release_mbid` from the
    /// cover art archive at `root`. The token stays out of this call.
    pub async fn cover_art(&self, root: &str, release_mbid: &str, caa_id: u64) -> Result<Vec<u8>> {
        let mut url = Url::parse(root).expect("the root is a valid URL");
        url.path_segments_mut()
            .expect("the root is an http URL")
            .pop_if_empty()
            .extend([
                "release",
                release_mbid,
                &format!("{caa_id}-{COVER_ART_WIDTH}.jpg"),
            ]);
        let response = exchange(self.http.get(url)).await?.error_for_status()?;
        Ok(response.bytes().await?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload<T: DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).unwrap().payload
    }

    #[test]
    fn top_artists_json_with_and_without_a_musicbrainz_id() {
        let Artists { artists } = payload(
            r#"{"payload":{"artists":[
                {"artist_mbid":"8229a8f1","artist_name":"Carbon Based Lifeforms","listen_count":1569},
                {"artist_name":"No Id","listen_count":3}],
                "count":2,"range":"year","user_id":"rob","total_artist_count":9}}"#,
        );
        assert_eq!(artists[0].artist_name, "Carbon Based Lifeforms");
        assert_eq!(artists[0].artist_mbid.as_deref(), Some("8229a8f1"));
        assert_eq!(artists[0].listen_count, 1569);
        assert_eq!(artists[1].artist_mbid, None);
    }

    #[test]
    fn top_albums_json_with_and_without_cover_art() {
        let ReleaseGroups { release_groups } = payload(
            r#"{"payload":{"release_groups":[
                {"artist_mbids":["3c2e"],"artist_name":"Cell","artists":[],"caa_id":32199768280,
                 "caa_release_mbid":"7c45fd16","listen_count":480,
                 "release_group_mbid":"c08736e7","release_group_name":"Onwards System"},
                {"artist_name":"Cell","listen_count":1,"release_group_name":"Bare"}],
                "count":2,"range":"all_time"}}"#,
        );
        assert_eq!(release_groups[0].release_group_name, "Onwards System");
        assert_eq!(release_groups[0].caa_id, Some(32199768280));
        assert_eq!(
            release_groups[0].caa_release_mbid.as_deref(),
            Some("7c45fd16")
        );
        assert_eq!(
            release_groups[0].release_group_mbid.as_deref(),
            Some("c08736e7")
        );
        assert_eq!(release_groups[1].caa_id, None);
        assert_eq!(release_groups[1].release_group_mbid, None);
    }

    #[test]
    fn a_range_without_listens_has_no_entries() {
        let Artists { artists } = payload(r#"{"payload":{"count":0,"range":"this_week"}}"#);
        assert!(artists.is_empty());
    }

    #[test]
    fn genre_activity_json_is_added_up() {
        let GenreHours { genre_activity } = payload(
            r#"{"payload":{"genre_activity":[
                {"genre":"ambient","hour":9,"listen_count":22},
                {"genre":"ambient","hour":10,"listen_count":9},
                {"genre":"rock","hour":1,"listen_count":3}],"range":"all_time"}}"#,
        );
        let genres = top_genres_of(genre_activity, 10);
        assert_eq!(genres[0].genre, "ambient");
        assert_eq!(genres[0].listen_count, 31);
        assert_eq!(genres[1].listen_count, 3);
    }

    #[test]
    fn genres_are_the_hours_added_up_most_first_and_cut_at_count() {
        let hour = |genre: &str, hour, listen_count| GenreActivity {
            genre: genre.into(),
            hour,
            listen_count,
        };
        let activity = vec![
            hour("rock", 9, 5),
            hour("ambient", 9, 4),
            hour("rock", 22, 1),
            hour("jazz", 3, 6),
            hour("blues", 3, 2),
        ];
        let top = |count| {
            top_genres_of(activity.clone(), count)
                .into_iter()
                .map(|genre| (genre.genre, genre.listen_count))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            top(3),
            [
                ("jazz".into(), 6),
                ("rock".into(), 6),
                ("ambient".into(), 4)
            ],
            "rock and jazz tie at 6 and the name settles it"
        );
        assert_eq!(top(10).len(), 4);
        assert!(top_genres_of(Vec::new(), 3).is_empty());
    }

    #[test]
    fn the_ranges_are_listenbrainzs_own_names() {
        let names = [
            Range::ThisWeek,
            Range::ThisMonth,
            Range::ThisYear,
            Range::AllTime,
        ]
        .map(Range::as_str);
        assert_eq!(names, ["this_week", "this_month", "this_year", "all_time"]);
    }
}
