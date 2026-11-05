//! The playlists ListenBrainz makes for the user: a shelf on the home page
//! and, for each, a page that lists every track in ListenBrainz's order.
//! The tracks the library has can be played; the others stay greyed. Save
//! makes a playlist of the first on the server. [`matching`] says which
//! songs are the library's.

use listenbrainz as lb;

use super::matching::lookup;
use super::*;

/// What the tiles say made the playlists.
const MADE_BY: &str = "ListenBrainz";

/// The playlist on screen.
#[derive(Default)]
pub(super) struct State {
    shown: Option<Shown>,
}

struct Shown {
    name: String,
    tracks: Vec<lb::PlaylistTrack>,
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
pub(crate) async fn account() -> Option<(lb::Client, String)> {
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
    let cache = browser.borrow().found.clone();
    for (row, track) in tracks.iter().enumerate() {
        let found = match lookup(&cache, client, track).await {
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

/// "38 of 50 tracks are in your library".
fn facts(b: &Browser, total: usize) -> String {
    let found = b.songs.iter().filter(|song| !is_missing(song)).count();
    format!(
        "{found} of {} in your library",
        count(total, "track", "tracks")
    )
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
    use super::super::matching::fixtures::{song, wanted};
    use super::*;

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
