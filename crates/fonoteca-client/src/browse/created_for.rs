//! The playlists ListenBrainz makes for the user: a shelf on the home page
//! and, for each, a page that lists every track in ListenBrainz's order.
//! The tracks the library has can be played; the others stay greyed. Save
//! makes a playlist of the first on the server.
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
/// What the tiles say made the playlists.
const MADE_BY: &str = "ListenBrainz";

/// What the library found for tracks already looked up, and the playlist on
/// screen.
#[derive(Default)]
pub(super) struct State {
    /// The song of a track by its key, or none when the library has no
    /// song of it, kept for the session so a playlist opens again at once.
    found: HashMap<String, Option<Child>>,
    shown: Option<Shown>,
}

struct Shown {
    name: String,
    tracks: Vec<lb::PlaylistTrack>,
}

/// The library's song for `track` among `songs`, or none: the one with its
/// recording ID, else the one with its title and an artist of it.
fn pick<'a>(track: &lb::PlaylistTrack, songs: &'a [Child]) -> Option<&'a Child> {
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
fn fold(text: &str) -> String {
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

/// What names a track in `State::found`: its recording, else its title and
/// artist.
fn key(track: &lb::PlaylistTrack) -> String {
    match &track.recording_mbid {
        Some(mbid) => mbid.to_lowercase(),
        None => format!("{}|{}", fold(&track.title), fold(&track.artist)),
    }
}

/// The playlists to offer: all but the weekly ones of music the user
/// hasn't heard, which the app can't play yet.
fn without_exploration(playlists: Vec<lb::Playlist>) -> Vec<lb::Playlist> {
    playlists
        .into_iter()
        .filter(|playlist| !playlist.is_exploration())
        .collect()
}

/// Whether the library already has a playlist of this name.
fn is_taken(playlists: &[opensubsonic::Playlist], name: &str) -> bool {
    playlists.iter().any(|playlist| playlist.name == name)
}

/// A song standing for a track the library doesn't have; it has no ID.
fn missing_song(track: &lb::PlaylistTrack) -> Child {
    Child {
        title: track.title.clone(),
        artist: Some(track.artist.clone()),
        ..Child::default()
    }
}

pub(super) fn is_missing(song: &Child) -> bool {
    song.id.is_empty()
}

/// Takes the songs the library lacks out of the selection, which a range
/// picked across their rows put there.
pub(super) fn forget_missing(b: &mut Browser) {
    let missing = song_link(&Child::default());
    b.selected.retain(|link| *link != missing);
}

/// The row of a track as listed, before and unless the library has it.
fn missing_row(track: &lb::PlaylistTrack) -> Track {
    let credits = [Some(track.artist.clone()), track.album.clone()];
    Track {
        title: track.title.as_str().into(),
        split: 1,
        names: names(credits.into_iter().flatten().map(unlinked).collect()),
        duration: (track.duration_ms)
            .map(|ms| duration((ms / 1000) as i64))
            .unwrap_or_default()
            .into(),
        unavailable: true,
        ..Track::default()
    }
}

/// The client for the saved ListenBrainz token and the user it belongs to,
/// when there is a token.
async fn account() -> Option<(lb::Client, String)> {
    let saved = crate::store::load_listenbrainz().await?;
    // Tests must never reach the real ListenBrainz, which is someone's account.
    #[cfg(not(test))]
    let client = lb::Client::new(saved.token);
    #[cfg(test)]
    let client = lb::Client::with_root(saved.token, &testing::root());
    client
        .inspect_err(|e| eprintln!("ListenBrainz: no client: {e}"))
        .ok()
        .map(|client| (client, saved.user))
}

/// Shows the shelf on the home page: the playlists made for the user, or
/// none without a token. Says so when ListenBrainz can't be asked.
pub(super) fn shelf(ui: &AppWindow) {
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let playlists = playlists().await;
        let Some(ui) = weak.upgrade() else { return };
        match playlists {
            Ok(playlists) => ui.set_created_for_you(tiles(playlists)),
            Err(e) => {
                ui.set_created_for_you(ModelRc::default());
                if ui.get_page() == Page::Home {
                    ui.set_page_problem(format!("Created for you couldn't be fetched: {e}").into());
                }
            }
        }
    })
    .expect("the event loop is running");
}

async fn playlists() -> Result<Vec<lb::Playlist>, lb::Error> {
    let Some((client, user)) = account().await else {
        return Ok(Vec::new());
    };
    client
        .playlists_created_for(&user)
        .await
        .map(without_exploration)
}

fn tiles(playlists: Vec<lb::Playlist>) -> ModelRc<AlbumTile> {
    let tiles: Vec<AlbumTile> = (playlists.into_iter())
        .map(|playlist| AlbumTile {
            title: playlist.title.into(),
            names: names(vec![unlinked(MADE_BY.into())]),
            link: format!("createdfor:{}", playlist.mbid).into(),
            ..AlbumTile::default()
        })
        .collect();
    ModelRc::new(VecModel::from(tiles))
}

/// Shows the page of the playlist with this ID: every track at once, greyed,
/// then each the library has as it is found, one search after another so a
/// playlist of fifty is not fifty searches at once.
pub(super) fn open_page(ui: &AppWindow, browser: &Shared, mbid: String) {
    let visit = browser.borrow().visit;
    ui.set_loading(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let fetched = fetch(&mbid).await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().visit != visit {
            return;
        }
        ui.set_loading(false);
        match fetched {
            Ok(playlist) => {
                show(&ui, &browser, playlist);
                resume(&ui, &browser);
            }
            Err(e) => ui.set_page_problem(e.into()),
        }
    })
    .expect("the event loop is running");
}

async fn fetch(mbid: &str) -> Result<lb::Playlist, String> {
    let Some((client, _)) = account().await else {
        return Err("Set your ListenBrainz token in the settings to see this playlist.".into());
    };
    (client.playlist(mbid).await).map_err(|e| format!("The playlist couldn't be fetched: {e}"))
}

fn show(ui: &AppWindow, browser: &Shared, playlist: lb::Playlist) {
    ui.set_header_title(playlist.title.as_str().into());
    ui.set_header_facts(count(playlist.tracks.len(), "track", "tracks").into());
    ui.set_created_for(true);
    let songs = playlist.tracks.iter().map(missing_song).collect();
    let rows = playlist.tracks.iter().map(missing_row).collect();
    let mut b = browser.borrow_mut();
    show_songs(&mut b, songs, rows);
    b.created_for.shown = Some(Shown {
        name: playlist.title,
        tracks: playlist.tracks,
    });
}

/// Looks for the library's songs of the playlist on screen, from the first
/// track on; what was found before comes from the cache, so after a failure
/// it goes on where that stopped. Save stays off until it has all been
/// looked up.
pub(super) fn resume(ui: &AppWindow, browser: &Shared) {
    let (client, visit, tracks) = {
        let b = browser.borrow();
        let (Some(client), Some(shown)) = (b.client.clone(), &b.created_for.shown) else {
            return;
        };
        (client, b.visit, shown.tracks.clone())
    };
    ui.set_created_for_matching(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        find_songs(&weak, &browser, &client, &tracks, visit).await;
    })
    .expect("the event loop is running");
}

async fn find_songs(
    weak: &slint::Weak<AppWindow>,
    browser: &Shared,
    client: &Client,
    tracks: &[lb::PlaylistTrack],
    visit: i32,
) {
    for (row, track) in tracks.iter().enumerate() {
        let found = match lookup(browser, client, track).await {
            Ok(found) => found,
            Err(e) => {
                let Some(ui) = weak.upgrade() else { return };
                if browser.borrow().visit == visit {
                    ui.set_page_problem(format!("Your server didn't answer: {e}").into());
                }
                return;
            }
        };
        let Some(ui) = weak.upgrade() else { return };
        let mut b = browser.borrow_mut();
        if b.visit != visit {
            return;
        }
        if let Some(song) = found {
            b.tracks.set_row_data(row, song_track(&song));
            b.songs[row] = song;
        }
        ui.set_header_facts(facts(&b, tracks.len()).into());
    }
    if let Some(ui) = weak.upgrade()
        && browser.borrow().visit == visit
    {
        ui.set_created_for_matching(false);
        show_downloads(&ui, browser);
    }
}

/// The library's song of `track`, from what was found before in this
/// session or else by searching.
async fn lookup(
    browser: &Shared,
    client: &Client,
    track: &lb::PlaylistTrack,
) -> opensubsonic::Result<Option<Child>> {
    if let Some(found) = browser.borrow().created_for.found.get(&key(track)) {
        return Ok(found.clone());
    }
    let found = search(client, track).await?;
    (browser.borrow_mut().created_for.found).insert(key(track), found.clone());
    Ok(found)
}

/// "38 of 50 tracks are in your library".
fn facts(b: &Browser, total: usize) -> String {
    let found = b.songs.iter().filter(|song| !is_missing(song)).count();
    format!(
        "{found} of {} in your library",
        count(total, "track", "tracks")
    )
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

/// Makes a playlist on the server of the songs the library has, in
/// ListenBrainz's order and under ListenBrainz's name. A playlist already
/// of that name is left as it is and nothing is made, since making another
/// of the same name would leave two the user can't tell apart.
pub(super) fn save(ui: &AppWindow, browser: &Shared) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    if ui.get_created_for_matching() {
        return;
    }
    let (name, tracks, ids) = {
        let b = browser.borrow();
        let Some(shown) = &b.created_for.shown else {
            return;
        };
        let ids: Vec<String> = (b.songs.iter())
            .filter(|song| !is_missing(song))
            .map(|song| song.id.clone())
            .collect();
        (shown.name.clone(), shown.tracks.len(), ids)
    };
    if ids.is_empty() {
        return notify(
            ui,
            browser,
            "None of its tracks are in your library.".into(),
        );
    }
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let saved = save_new(&client, &name, &ids).await;
        let Some(ui) = weak.upgrade() else { return };
        match saved {
            Ok(false) => notify(
                &ui,
                &browser,
                format!("You already have a playlist named {name}, so nothing was saved."),
            ),
            Ok(true) => {
                load_playlists(&ui, &browser);
                let saved = format!("{} of {tracks}", ids.len());
                notify(&ui, &browser, format!("Saved {name} with {saved} tracks."));
            }
            Err(e) => ui.set_page_problem(format!("Your server didn't take that: {e}").into()),
        }
    })
    .expect("the event loop is running");
}

/// Makes the playlist unless the name is taken; whether it did.
async fn save_new(client: &Client, name: &str, ids: &[String]) -> opensubsonic::Result<bool> {
    if is_taken(&client.get_playlists(None).await?.playlist, name) {
        return Ok(false);
    }
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    client.create_playlist(name, &ids).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(title: &str, artist: &str, mbid: Option<&str>) -> lb::PlaylistTrack {
        lb::PlaylistTrack {
            recording_mbid: mbid.map(str::to_owned),
            title: title.into(),
            artist: artist.into(),
            artists: vec![artist.into()],
            album: None,
            duration_ms: None,
        }
    }

    fn song(id: &str, title: &str, artist: &str, mbid: Option<&str>) -> Child {
        Child {
            id: id.into(),
            title: title.into(),
            artist: Some(artist.into()),
            music_brainz_id: mbid.map(str::to_owned),
            ..Child::default()
        }
    }

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

    #[test]
    fn a_playlist_made_for_you_lights_home_in_the_sidebar_and_the_bar() {
        let b = Browser {
            history: vec!["createdfor:x".into()],
            ..Browser::default()
        };
        let home = sections_of(&b).iter().position(|s| s.0 == "home");
        assert_eq!(home, Some(0));
        assert_eq!((section(&b), bar_section(&b)), (0, 0));
    }

    #[test]
    fn exploration_playlists_are_not_offered() {
        let playlist = |title: &str, patch: Option<&str>| lb::Playlist {
            mbid: title.into(),
            title: title.into(),
            annotation: None,
            source_patch: patch.map(str::to_owned),
            tracks: Vec::new(),
        };
        let offered = without_exploration(vec![
            playlist("Daily Jams", Some("daily-jams")),
            playlist("Weekly Exploration", Some("weekly-exploration")),
            playlist("Hand made", None),
        ]);
        let titles: Vec<&str> = offered.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Daily Jams", "Hand made"]);
    }

    #[test]
    fn a_name_is_taken_when_a_playlist_has_exactly_it() {
        let playlists: Vec<opensubsonic::Playlist> = ["Road Trip", "Mix"]
            .into_iter()
            .map(|name| {
                serde_json::from_value(serde_json::json!({
                    "id": name, "name": name, "songCount": 0, "duration": 0,
                    "created": "2024-01-01T00:00:00Z", "changed": "2024-01-01T00:00:00Z"
                }))
                .unwrap()
            })
            .collect();
        assert!(is_taken(&playlists, "Mix"));
        assert!(!is_taken(&playlists, "mix"));
        assert!(!is_taken(&[], "Mix"));
    }

    #[test]
    fn playing_from_a_row_leaves_out_the_tracks_the_library_lacks() {
        let browser = Shared::default();
        let missing = missing_song(&wanted("Gone", "Nobody", None));
        browser.borrow_mut().songs = vec![
            missing.clone(),
            song("a", "A", "X", None),
            missing,
            song("b", "B", "X", None),
        ];
        let ids = |songs: Vec<Child>| -> Vec<String> { songs.into_iter().map(|s| s.id).collect() };
        let (songs, start) = super::songs(&browser, Page::Playlist, 3);
        assert_eq!(
            (ids(songs), start),
            (vec!["a".to_owned(), "b".to_owned()], 1)
        );
        let (_, start) = super::songs(&browser, Page::Playlist, 0);
        assert_eq!(start, 0);
    }

    #[test]
    fn a_track_the_library_lacks_is_a_greyed_row_and_cannot_be_selected() {
        let track = wanted("Familiar", "Nils Frahm", None);
        let missing = missing_song(&track);
        assert!(is_missing(&missing));
        assert!(!is_missing(&song("a", "Familiar", "Nils Frahm", None)));
        let row = missing_row(&track);
        assert!(row.unavailable);
        assert_eq!(row.title, "Familiar");
        let mut b = Browser {
            selected: vec!["song:a".into(), song_link(&missing), "song:b".into()],
            ..Browser::default()
        };
        forget_missing(&mut b);
        assert_eq!(b.selected, ["song:a", "song:b"]);
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::Mutex;

    static ROOT: Mutex<Option<String>> = Mutex::new(None);

    /// Points the calls at a stand-in for ListenBrainz, `root` ending in
    /// the API version.
    pub(crate) fn serve_from(root: &str) {
        *ROOT.lock().unwrap() = Some(root.to_owned());
    }

    /// Where the calls go: the stand-in, or an address nothing answers.
    pub(super) fn root() -> String {
        (ROOT.lock().unwrap().clone()).unwrap_or_else(|| "http://127.0.0.1:9/1/".to_owned())
    }
}
