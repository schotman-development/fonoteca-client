//! The listens a user has sent, newest first, a page at a time. ListenBrainz
//! pages them by when they were heard, not by an offset.

use serde::{Deserialize, Serialize};

use crate::playlist::Credit;
use crate::{Client, PlaylistTrack, Result, send};

/// Which listens a page holds: the newest, or those just before or just
/// after a time, in seconds since 1970. A listen at that very time is left
/// out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Around {
    Newest,
    Before(i64),
    After(i64),
}

/// The most listens ListenBrainz gives in one page.
pub const MAX_LISTENS_PER_PAGE: u32 = 1000;

/// A track the user listened to, and when they began, in seconds since 1970.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listened {
    pub listened_at: i64,
    pub track: PlaylistTrack,
}

/// One page of [`Client::listens`], newest first, and when the user heard
/// the newest and the oldest of all their listens, in seconds since 1970;
/// none without any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListensPage {
    pub listens: Vec<Listened>,
    pub newest_at: Option<i64>,
    pub oldest_at: Option<i64>,
}

#[derive(Deserialize)]
struct Answer {
    payload: Payload,
}

#[derive(Deserialize)]
struct CountAnswer {
    payload: Count,
}

#[derive(Deserialize)]
struct Count {
    count: u64,
}

/// The page, and the times of the user's newest and oldest listen of all.
#[derive(Deserialize)]
struct Payload {
    listens: Vec<Sent>,
    latest_listen_ts: Option<i64>,
    oldest_listen_ts: Option<i64>,
}

#[derive(Deserialize)]
struct Sent {
    listened_at: i64,
    track_metadata: Metadata,
}

#[derive(Deserialize)]
struct Metadata {
    track_name: String,
    artist_name: String,
    release_name: Option<String>,
    additional_info: Option<Info>,
    mbid_mapping: Option<Mapping>,
}

/// What the program that sent the listen said of the track, from the tags
/// of the file it played.
#[derive(Deserialize)]
struct Info {
    recording_mbid: Option<String>,
    artist_names: Option<Vec<String>>,
}

/// The recording ListenBrainz took the listen for.
#[derive(Deserialize)]
struct Mapping {
    recording_mbid: Option<String>,
    #[serde(default)]
    artists: Vec<Credit>,
}

impl From<Sent> for Listened {
    /// The track as the program that sent it named it, since that names it
    /// as the library does, else as ListenBrainz took it.
    fn from(sent: Sent) -> Self {
        let metadata = sent.track_metadata;
        let (sent_mbid, sent_artists) = match metadata.additional_info {
            Some(info) => (info.recording_mbid, info.artist_names.unwrap_or_default()),
            None => (None, Vec::new()),
        };
        let (mapped_mbid, credits) = match metadata.mbid_mapping {
            Some(mapping) => (mapping.recording_mbid, mapping.artists),
            None => (None, Vec::new()),
        };
        let artists = if !sent_artists.is_empty() {
            sent_artists
        } else if !credits.is_empty() {
            credits.into_iter().map(|c| c.artist_credit_name).collect()
        } else {
            vec![metadata.artist_name.clone()]
        };
        let artist_mbids = vec![None; artists.len()];
        Listened {
            listened_at: sent.listened_at,
            track: PlaylistTrack {
                recording_mbid: sent_mbid.or(mapped_mbid),
                title: metadata.track_name,
                artist: metadata.artist_name,
                artists,
                artist_mbids,
                album: metadata.release_name,
                duration_ms: None,
            },
        }
    }
}

impl From<Payload> for ListensPage {
    fn from(payload: Payload) -> Self {
        ListensPage {
            listens: payload.listens.into_iter().map(Listened::from).collect(),
            newest_at: payload.latest_listen_ts,
            oldest_at: payload.oldest_listen_ts,
        }
    }
}

impl Client {
    /// A page of `user`'s listens, at most `count` of them, and no more
    /// than [`MAX_LISTENS_PER_PAGE`].
    pub async fn listens(&self, user: &str, around: Around, count: u32) -> Result<ListensPage> {
        let mut query = vec![("count", count.to_string())];
        match around {
            Around::Newest => {}
            Around::Before(at) => query.push(("max_ts", at.to_string())),
            Around::After(at) => query.push(("min_ts", at.to_string())),
        }
        let url = self.url(&["user", user, "listens"]);
        let answer: Answer = send(self.authorized(self.http.get(url).query(&query))).await?;
        Ok(answer.payload.into())
    }

    /// How many listens `user` has.
    pub async fn listen_count(&self, user: &str) -> Result<u64> {
        let url = self.url(&["user", user, "listen-count"]);
        let answer: CountAnswer = send(self.authorized(self.http.get(url))).await?;
        Ok(answer.payload.count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two listens as ListenBrainz answers: one sent with its tags and
    /// matched, one sent with only its names and not matched.
    const PAGE: &str = r#"{"payload":{"count":2,"user_id":"rob","latest_listen_ts":1771414109,"oldest_listen_ts":1160599019,"listens":[
      {"inserted_at":1771414107,"listened_at":1771413706,"recording_msid":"e0f1","user_name":"rob",
       "track_metadata":{"artist_name":"Röyksopp feat. Susanne Sundfør","track_name":"Tell Him","release_name":"Profound Mysteries II",
         "additional_info":{"artist_names":["Röyksopp","Susanne Sundfør"],"duration_ms":326560,"tracknumber":9,
           "recording_mbid":"46f0b53f-6731-457c-93e7-57cff0e5b0c2"},
         "mbid_mapping":{"recording_mbid":"0000-mapped","recording_name":"Tell Him",
           "artists":[{"artist_credit_name":"Royksopp","join_phrase":" feat. "},{"artist_credit_name":"Susanne Sundfor","join_phrase":""}]}}},
      {"listened_at":1771413380,"track_metadata":{"artist_name":"21 Hertz","track_name":"Bare","additional_info":{}}}]}}"#;

    fn page(json: &str) -> ListensPage {
        serde_json::from_str::<Answer>(json).unwrap().payload.into()
    }

    #[test]
    fn a_listen_is_named_as_it_was_sent_else_as_listenbrainz_took_it() {
        let listens = page(PAGE).listens;
        let sent = &listens[0];
        assert_eq!(sent.listened_at, 1771413706);
        assert_eq!(
            sent.track.recording_mbid.as_deref(),
            Some("46f0b53f-6731-457c-93e7-57cff0e5b0c2")
        );
        assert_eq!(sent.track.title, "Tell Him");
        assert_eq!(sent.track.artist, "Röyksopp feat. Susanne Sundfør");
        assert_eq!(sent.track.artists, ["Röyksopp", "Susanne Sundfør"]);
        assert_eq!(sent.track.album.as_deref(), Some("Profound Mysteries II"));
        let bare = &listens[1];
        assert_eq!(bare.track.recording_mbid, None);
        assert_eq!(bare.track.artists, ["21 Hertz"]);
        assert_eq!(bare.track.album, None);
    }

    #[test]
    fn a_matched_listen_sent_without_tags_takes_listenbrainz_s_recording_and_credits() {
        let json = r#"{"payload":{"latest_listen_ts":5,"oldest_listen_ts":5,"listens":[
          {"listened_at":5,"track_metadata":{"artist_name":"A feat. B","track_name":"T","additional_info":null,
            "mbid_mapping":{"recording_mbid":"r1","artists":[{"artist_credit_name":"A"},{"artist_credit_name":"B"}]}}}]}}"#;
        let track = &page(json).listens[0].track;
        assert_eq!(track.recording_mbid.as_deref(), Some("r1"));
        assert_eq!(track.artists, ["A", "B"]);
    }

    #[test]
    fn a_count_is_read_from_its_payload() {
        let answer: CountAnswer = serde_json::from_str(r#"{"payload":{"count":220287}}"#).unwrap();
        assert_eq!(answer.payload.count, 220287);
    }

    #[test]
    fn a_page_says_when_the_newest_and_oldest_of_all_listens_were_heard() {
        let between = page(PAGE);
        assert_eq!(
            (between.newest_at, between.oldest_at),
            (Some(1771414109), Some(1160599019))
        );
        let empty = page(r#"{"payload":{"count":0,"listens":[]}}"#);
        assert_eq!(empty, ListensPage::default());
    }
}
