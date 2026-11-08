//! Finds the library's song for a track named by ListenBrainz: the playlists
//! made for the user (#45) and the recordings the user loves (#47).
//!
//! A track is the library's when a song has its MusicBrainz recording ID,
//! else when a song has its title and one of its artists, however they are
//! cased or accented. OpenSubsonic can't search by recording ID (Fonoteca's
//! search3 finds nothing for one), so the songs are looked up by title and
//! artist, and the ID is checked among those.

use listenbrainz as lb;

use super::*;

/// Songs the library search returns for one query.
const SEARCH_RESULTS: u32 = 30;

/// What the library found for tracks already looked up: the song of a track
/// by its key, or none when the library has no song of it. Kept for the
/// session, so Home looks up the tracks of its playlists once and a loved
/// track is not searched for twice; a playlist's page asks again.
#[derive(Clone, Default)]
pub(crate) struct Found(Rc<RefCell<HashMap<String, Option<Child>>>>);

/// The library's song for `track` among `songs`, or none: the one with its
/// recording ID, else the one with its title and an artist of it.
pub(super) fn pick<'a>(track: &lb::PlaylistTrack, songs: &'a [Child]) -> Option<&'a Child> {
    let recording = (track.recording_mbid.as_deref())
        .and_then(|mbid| songs.iter().find(|song| is_recording(song, mbid)));
    recording.or_else(|| {
        let title = fold(&track.title);
        songs
            .iter()
            .find(|song| fold(&song.title) == title && shares_artist(song, track))
    })
}

fn is_recording(song: &Child, mbid: &str) -> bool {
    (song.music_brainz_id.as_deref()).is_some_and(|own| own.eq_ignore_ascii_case(mbid))
}

fn shares_artist(song: &Child, track: &lb::PlaylistTrack) -> bool {
    let wanted: Vec<String> = (track.artists.iter())
        .chain([&track.artist])
        .map(|name| fold(name))
        .filter(|name| !name.is_empty())
        .collect();
    let own = (song.artist.iter())
        .chain(song.display_artist.iter())
        .chain(song.artists.iter().map(|artist| &artist.name));
    own.map(|name| fold(name))
        .any(|name| wanted.contains(&name))
}

/// Letters with their accents and their case taken off, and nothing else:
/// "Beyoncé's Halo" and "beyonces halo" are the same. Spaces and
/// punctuation go too, so a title differing only in them is the same.
pub(super) fn fold(text: &str) -> String {
    const ACCENTED: [(&str, &str); 19] = [
        ("àáâãäåāăą", "a"),
        ("çćĉċč", "c"),
        ("ďđ", "d"),
        ("èéêëēĕėęě", "e"),
        ("ĝğġģ", "g"),
        ("ĥħ", "h"),
        ("ìíîïĩīĭįı", "i"),
        ("ĵ", "j"),
        ("ķ", "k"),
        ("ĺļľŀł", "l"),
        ("ñńņňŉ", "n"),
        ("òóôõöøōŏő", "o"),
        ("ŕŗř", "r"),
        ("śŝşš", "s"),
        ("ţťŧ", "t"),
        ("ùúûüũūŭůűų", "u"),
        ("ŵ", "w"),
        ("ýÿŷ", "y"),
        ("źżž", "z"),
    ];
    let mut folded = String::new();
    for letter in text.chars().flat_map(char::to_lowercase) {
        match letter {
            'ß' => folded.push_str("ss"),
            'æ' => folded.push_str("ae"),
            'œ' => folded.push_str("oe"),
            letter => match ACCENTED
                .iter()
                .find(|(accented, _)| accented.contains(letter))
            {
                Some((_, plain)) => folded.push_str(plain),
                None if letter.is_alphanumeric() => folded.push(letter),
                None => {}
            },
        }
    }
    folded
}

/// What names a track in [`Found`]: its recording, else its title and
/// artist.
fn key(track: &lb::PlaylistTrack) -> String {
    match &track.recording_mbid {
        Some(mbid) => mbid.to_lowercase(),
        None => format!("{}|{}", fold(&track.title), fold(&track.artist)),
    }
}

/// The library's song of `track`, from what was found before in this
/// session or else by searching.
pub(crate) async fn lookup(
    cache: &Found,
    client: &Client,
    track: &lb::PlaylistTrack,
) -> opensubsonic::Result<Option<Child>> {
    if let Some(found) = cache.0.borrow().get(&key(track)) {
        return Ok(found.clone());
    }
    search_anew(cache, client, track).await
}

/// The library's song of `track` as it has it now, searched for whatever
/// was found before, and noted in `cache` for the session.
pub(crate) async fn search_anew(
    cache: &Found,
    client: &Client,
    track: &lb::PlaylistTrack,
) -> opensubsonic::Result<Option<Child>> {
    let found = search(client, track).await?;
    cache.0.borrow_mut().insert(key(track), found.clone());
    Ok(found)
}

/// What to ask the library for a track, the likeliest first: its title and
/// first artist, so a title many songs share doesn't crowd it out, then its
/// title alone, for a library that names the artist otherwise.
fn search_queries(track: &lb::PlaylistTrack) -> Vec<String> {
    match track.artists.first().filter(|artist| !artist.is_empty()) {
        Some(artist) => vec![format!("{} {artist}", track.title), track.title.clone()],
        None => vec![track.title.clone()],
    }
}

/// One search after another until a song is the track's.
async fn search(client: &Client, track: &lb::PlaylistTrack) -> opensubsonic::Result<Option<Child>> {
    let options = SearchOptions {
        song_count: Some(SEARCH_RESULTS),
        artist_count: Some(0),
        album_count: Some(0),
        ..SearchOptions::default()
    };
    for query in search_queries(track) {
        let result = client.search3(&query, &options).await?;
        if let Some(song) = pick(track, &result.song) {
            return Ok(Some(song.clone()));
        }
    }
    Ok(None)
}

#[cfg(test)]
pub(super) mod fixtures {
    use super::*;

    pub(in crate::browse) fn wanted(
        title: &str,
        artist: &str,
        mbid: Option<&str>,
    ) -> lb::PlaylistTrack {
        lb::PlaylistTrack {
            recording_mbid: mbid.map(str::to_owned),
            title: title.into(),
            artist: artist.into(),
            artists: vec![artist.into()],
            artist_mbids: vec![None],
            album: None,
            duration_ms: None,
        }
    }

    pub(in crate::browse) fn song(
        id: &str,
        title: &str,
        artist: &str,
        mbid: Option<&str>,
    ) -> Child {
        Child {
            id: id.into(),
            title: title.into(),
            artist: Some(artist.into()),
            music_brainz_id: mbid.map(str::to_owned),
            ..Child::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{song, wanted};
    use super::*;

    fn picked<'a>(track: &lb::PlaylistTrack, songs: &'a [Child]) -> Option<&'a str> {
        pick(track, songs).map(|song| song.id.as_str())
    }

    #[test]
    fn folding_ignores_case_accents_and_punctuation() {
        assert_eq!(fold("Beyoncé's Halo"), fold("BEYONCES halo"));
        assert_eq!(fold("Sigur Rós"), "sigurros");
        assert_eq!(fold("e\u{301}"), "e");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("Motörhead – Ace of Spades!"), "motorheadaceofspades");
        assert_ne!(fold("Familiar"), fold("Familiars"));
        assert_eq!(fold("東京"), "東京");
    }

    #[test]
    fn a_recording_id_picks_the_song_whatever_its_name() {
        let songs = [song("a", "Other", "Someone", Some("R1"))];
        let track = wanted("Familiar", "Nils Frahm", Some("r1"));
        assert_eq!(picked(&track, &songs), Some("a"));
    }

    #[test]
    fn a_recording_id_beats_a_song_that_only_has_the_title() {
        let songs = [
            song("title", "Familiar", "Nils Frahm", None),
            song("id", "Familiar (Live)", "Nils Frahm", Some("r1")),
        ];
        let track = wanted("Familiar", "Nils Frahm", Some("r1"));
        assert_eq!(picked(&track, &songs), Some("id"));
    }

    #[test]
    fn without_a_recording_id_the_title_and_an_artist_pick_it() {
        let songs = [
            song("other", "Familiar", "Someone Else", None),
            song("right", "FAMILIAR", "nils frahm", None),
        ];
        assert_eq!(
            picked(&wanted("Familiar", "Nils Frahm", None), &songs),
            Some("right")
        );
        let elsewhere = wanted("Familiar", "Nils Frahm", Some("not-in-the-library"));
        assert_eq!(picked(&elsewhere, &songs), Some("right"));
    }

    #[test]
    fn the_title_must_be_the_same_and_the_artist_one_of_them() {
        let mut together = song("a", "Ritual", "Jon Hopkins", None);
        together.artists =
            vec![serde_json::from_value(serde_json::json!({"id": "x", "name": "Ishq"})).unwrap()];
        let mut track = wanted("Ritual", "Jon Hopkins, 7RAYS & Ishq", None);
        track.artists = vec!["Jon Hopkins".into(), "7RAYS".into(), "Ishq".into()];
        assert_eq!(picked(&track, std::slice::from_ref(&together)), Some("a"));
        let other_title = wanted("Rituals", "Jon Hopkins", None);
        assert_eq!(picked(&other_title, std::slice::from_ref(&together)), None);
        let other_artist = wanted("Ritual", "Someone Else", None);
        assert_eq!(picked(&other_artist, std::slice::from_ref(&together)), None);
        assert_eq!(picked(&track, &[]), None);
    }

    #[test]
    fn tracks_are_cached_by_recording_or_else_by_title_and_artist() {
        assert_eq!(
            key(&wanted("A", "B", Some("R1"))),
            key(&wanted("x", "y", Some("r1")))
        );
        assert_eq!(
            key(&wanted("Café", "B", None)),
            key(&wanted("cafe", "b", None))
        );
        assert_ne!(key(&wanted("A", "B", None)), key(&wanted("A", "C", None)));
    }

    #[test]
    fn a_track_is_asked_by_title_and_first_artist_then_by_title() {
        let mut track = wanted("Ritual", "Jon Hopkins, 7RAYS", None);
        track.artists = vec!["Jon Hopkins".into(), "7RAYS".into()];
        assert_eq!(search_queries(&track), ["Ritual Jon Hopkins", "Ritual"]);
        track.artists = vec![String::new()];
        assert_eq!(search_queries(&track), ["Ritual"]);
    }
}
