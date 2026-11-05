//! The playlists ListenBrainz makes for the user: a shelf on the home page
//! and, for each, a page that lists every track in ListenBrainz's order.
//! The tracks the library has can be played; the others stay greyed. Save
//! makes a playlist of the first on the server. [`matching`] says which
//! songs are the library's.

use listenbrainz as lb;

use super::matching::{fold, lookup};
use super::*;
use crate::KindTint;

/// The width the banners behind the tiles are asked for, in points: the
/// widest a wide tile gets, in a shelf one tile across that is about to
/// hold two (two tiles of its least width and the gap).
const BANNER_WIDTH: f32 = 640.0;
/// What ListenBrainz calls the algorithms behind the playlists with a name
/// of their own.
const WEEKLY_PATCH: &str = "weekly-jams";
const DAILY_PATCH: &str = "daily-jams";
/// The yearly ones by the start of their algorithm, and what a tile calls
/// them. The `-of-` ones end in the year; the `-for-year` ones don't, and
/// have it in their title.
const YEARLY_PATCHES: [(&str, &str); 5] = [
    ("top-discoveries-of-", "Top Discoveries"),
    ("top-missed-recordings-of-", "Missed Tracks"),
    ("top-discoveries-for-year", "Top Discoveries"),
    ("top-recordings-for-year", "Top Recordings"),
    ("top-new-recordings-for-year", "Top New Releases"),
];
/// An old yearly playlist ListenBrainz made without saying which, known by
/// how it starts.
const UNLABELLED_MISSED_TITLE: &str = "Top Missed Recordings of ";
/// How many days before the newest weekly playlist the one of the week
/// before it can be made, a day either way.
const LAST_WEEK_DAYS: std::ops::RangeInclusive<i64> = 6..=8;

/// The playlist on screen.
#[derive(Default)]
pub(super) struct State {
    shown: Option<Shown>,
    /// The playlists on the home page's shelf, newest first, and the tiles
    /// that show them.
    shelf: Vec<lb::Playlist>,
    tiles: Rc<VecModel<AlbumTile>>,
    /// The tracks of the shelf's playlists by ID, kept for the session. A
    /// playlist whose tracks couldn't be fetched has none kept, so the next
    /// visit to the home page asks again.
    shelf_tracks: HashMap<String, Vec<lb::PlaylistTrack>>,
    /// The banner each tile was given, by row.
    banners: Vec<Option<String>>,
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
pub(super) fn missing_song(track: &lb::PlaylistTrack) -> Child {
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
pub(super) fn missing_row(track: &lb::PlaylistTrack) -> Track {
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
/// none without a token, and then their banners. Says so when ListenBrainz
/// can't be asked.
pub(super) fn shelf(ui: &AppWindow, browser: &Shared) {
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let playlists = playlists().await;
        let Some(ui) = weak.upgrade() else { return };
        match playlists {
            Ok(playlists) => {
                let model = Rc::new(VecModel::from(tiles(&playlists)));
                ui.set_created_for_you(ModelRc::from(model.clone()));
                {
                    let mut b = browser.borrow_mut();
                    b.created_for.banners = vec![None; playlists.len()];
                    b.created_for.shelf = playlists;
                    b.created_for.tiles = model.clone();
                }
                // The playlists whose tracks an earlier visit kept show their
                // banners at once, on this visit's new tiles.
                banners(&ui, &browser);
                fetch_shelf_tracks(&ui, &browser, &model).await;
            }
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

/// What a tile calls its playlist and the colour it has: by the algorithm
/// that made it, else under the playlist's own title.
fn kind_of(playlist: &lb::Playlist, is_last_week: bool) -> (String, KindTint) {
    match playlist.source_patch.as_deref() {
        Some(WEEKLY_PATCH) if is_last_week => ("Last Week's Jams".into(), KindTint::Green),
        Some(WEEKLY_PATCH) => ("Weekly Jams".into(), KindTint::Green),
        Some(DAILY_PATCH) => ("Daily Jams".into(), KindTint::Blue),
        _ => match yearly_name(playlist) {
            Some(name) => (name, KindTint::Red),
            None => (playlist.title.clone(), KindTint::Blue),
        },
    }
}

/// "2025 Top Discoveries" for a yearly playlist, without the year where
/// there is none to be found; none for the others.
fn yearly_name(playlist: &lb::Playlist) -> Option<String> {
    let patch = playlist.source_patch.as_deref();
    let (name, patch_end) = match patch {
        Some(patch) => YEARLY_PATCHES
            .iter()
            .find_map(|(start, name)| Some((*name, patch.strip_prefix(start)?)))?,
        None if playlist.title.starts_with(UNLABELLED_MISSED_TITLE) => ("Missed Tracks", ""),
        None => return None,
    };
    let year = [patch_end, &playlist.title]
        .into_iter()
        .find_map(year_in)
        .map(str::to_owned);
    Some(year.map_or_else(|| name.to_owned(), |year| format!("{year} {name}")))
}

/// The first four digits on their own in `text`.
fn year_in(text: &str) -> Option<&str> {
    text.split(|letter: char| !letter.is_ascii_digit())
        .find(|digits| digits.len() == 4)
}

/// The tiles of `playlists`, newest first. The newest weekly playlist is
/// this week's; the one made a week before it is last week's; any older
/// is a weekly playlist like it, told apart by its name beneath.
fn tiles(playlists: &[lb::Playlist]) -> Vec<AlbumTile> {
    let is_weekly =
        |playlist: &lb::Playlist| playlist.source_patch.as_deref() == Some(WEEKLY_PATCH);
    let newest_weekly_day = (playlists.iter())
        .find(|playlist| is_weekly(playlist))
        .and_then(lb::Playlist::day_number);
    (playlists.iter())
        .map(|playlist| {
            let is_last_week = match (newest_weekly_day, playlist.day_number()) {
                (Some(newest), Some(day)) => LAST_WEEK_DAYS.contains(&(newest - day)),
                _ => false,
            };
            let (title, tint) = kind_of(playlist, is_last_week);
            // The playlist's own name, with its date, under a name for its kind.
            let own_name = (playlist.title != title).then(|| unlinked(playlist.title.clone()));
            AlbumTile {
                title: title.into(),
                names: names(own_name.into_iter().collect()),
                link: format!("createdfor:{}", playlist.mbid).into(),
                tint,
                ..AlbumTile::default()
            }
        })
        .collect()
}

/// The IDs of the shelf's playlists whose tracks aren't kept, in shelf
/// order.
fn playlists_without_tracks(state: &State) -> Vec<String> {
    (state.shelf.iter())
        .map(|playlist| playlist.mbid.clone())
        .filter(|mbid| !state.shelf_tracks.contains_key(mbid))
        .collect()
}

/// Keeps the tracks of a playlist that was fetched. One that couldn't be
/// keeps nothing, so that the next round asks for it again.
fn keep_tracks(state: &mut State, mbid: String, fetched: Result<lb::Playlist, lb::Error>) {
    match fetched {
        Ok(playlist) => {
            state.shelf_tracks.insert(mbid, playlist.tracks);
        }
        Err(e) => eprintln!("ListenBrainz: no tracks for a banner: {e}"),
    }
}

/// Whether ListenBrainz answered that the rate limit is reached, which ends
/// a round of fetches.
fn is_rate_limited(fetched: &Result<lb::Playlist, lb::Error>) -> bool {
    matches!(fetched, Err(lb::Error::RateLimited { .. }))
}

/// Fetches the tracks of the shelf's playlists that aren't kept yet, in
/// shelf order, and gives each tile its banner as its tracks arrive. The
/// playlists the list gives have none, and they say which artists each has.
/// ListenBrainz makes the calls wait their turn. Stops when it says the rate
/// limit is reached, or when the shelf is not the one it began with.
async fn fetch_shelf_tracks(ui: &AppWindow, browser: &Shared, model: &Rc<VecModel<AlbumTile>>) {
    let wanted = playlists_without_tracks(&browser.borrow().created_for);
    let Some((client, _)) = account().await else {
        return;
    };
    for mbid in wanted {
        let fetched = client.playlist(&mbid).await;
        if !Rc::ptr_eq(&browser.borrow().created_for.tiles, model) {
            return;
        }
        let limited = is_rate_limited(&fetched);
        keep_tracks(&mut browser.borrow_mut().created_for, mbid, fetched);
        banners(ui, browser);
        if limited {
            return;
        }
    }
}

/// The library's artists in `tracks`, the one with most tracks first, those
/// with as many in the order of their first track. An artist is matched by
/// its MusicBrainz ID, else by name.
fn artists_in(tracks: &[lb::PlaylistTrack], artists: &[ArtistID3]) -> Vec<(Name, usize)> {
    let by_mbid: HashMap<String, &ArtistID3> = (artists.iter())
        .filter_map(|artist| Some((artist.music_brainz_id.as_ref()?.to_lowercase(), artist)))
        .collect();
    let by_name: HashMap<String, &ArtistID3> = artists
        .iter()
        .map(|artist| (fold(&artist.name), artist))
        .collect();
    let mut counted: Vec<(&ArtistID3, usize)> = Vec::new();
    for track in tracks {
        let mut in_track: Vec<&ArtistID3> = Vec::new();
        for (credit, name) in track.artists.iter().enumerate() {
            let mbid = track.artist_mbids.get(credit).and_then(Option::as_deref);
            let found = (mbid.and_then(|mbid| by_mbid.get(&mbid.to_lowercase())))
                .or_else(|| by_name.get(&fold(name)));
            if let Some(&artist) = found
                && !in_track.iter().any(|seen| seen.id == artist.id)
            {
                in_track.push(artist);
            }
        }
        for artist in in_track {
            match counted.iter_mut().find(|(seen, _)| seen.id == artist.id) {
                Some((_, tracks)) => *tracks += 1,
                None => counted.push((artist, 1)),
            }
        }
    }
    counted.sort_by_key(|&(_, tracks)| std::cmp::Reverse(tracks));
    (counted.into_iter())
        .map(|(artist, tracks)| {
            let name = Name {
                label: artist.name.as_str().into(),
                link: format!("artist:{}", artist.id).into(),
            };
            (name, tracks)
        })
        .collect()
}

/// The banners the shelf's playlists get from the artists the library has
/// and the server's banners, if it offers them.
fn banners_of(b: &Browser) -> Vec<Option<String>> {
    shelf_banners(
        &b.created_for.shelf,
        &b.created_for.shelf_tracks,
        &b.artists,
        offers(b, ARTIST_BANNER),
    )
}

/// Each playlist's banner, those first in line choosing first: the banner
/// of the artist with most tracks in it that has one no playlist before it
/// shows.
fn shelf_banners(
    playlists: &[lb::Playlist],
    tracks: &HashMap<String, Vec<lb::PlaylistTrack>>,
    artists: &[ArtistID3],
    offered: bool,
) -> Vec<Option<String>> {
    let banners = artist_banners(artists, offered);
    let known = playlists.iter().map(|playlist| {
        let tracks = tracks.get(&playlist.mbid).map_or(&[][..], Vec::as_slice);
        artists_in(tracks, artists)
    });
    distinct_banners(known, &banners)
}

/// Puts each tile's banner behind it as the tracks of its playlist arrive,
/// once the library's artists are in. The tiles show their tint and name
/// until then, and a banner already shown stays where none is found.
pub(super) fn banners(ui: &AppWindow, browser: &Shared) {
    let size = pixels(ui, BANNER_WIDTH);
    let (tiles, wanted) = {
        let mut b = browser.borrow_mut();
        if !b.artists_loaded {
            return;
        }
        let assigned = banners_of(&b);
        let state = &b.created_for;
        let wanted: Vec<(usize, String)> = (assigned.iter().enumerate())
            .filter(|&(row, banner)| *banner != state.banners[row])
            .filter_map(|(row, banner)| Some((row, banner.clone()?)))
            .collect();
        let tiles = state.tiles.clone();
        b.created_for.banners = assigned;
        (tiles, wanted)
    };
    if wanted.is_empty() {
        return;
    }
    let browser = browser.clone();
    slint::spawn_local(async move {
        for (row, banner) in wanted {
            if !Rc::ptr_eq(&browser.borrow().created_for.tiles, &tiles) {
                return;
            }
            let image = cover_art(&browser, None, banner, size).await;
            if let (Some(Some(image)), Some(mut tile)) = (image, tiles.row_data(row)) {
                tile.cover = image;
                tiles.set_row_data(row, tile);
            }
        }
    })
    .expect("the event loop is running");
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

/// Makes the playlist unless the name is taken, as the server has the
/// names now: those kept may be from up to an hour ago. Whether it did.
async fn save_new(client: &Client, name: &str, ids: &[String]) -> opensubsonic::Result<bool> {
    let playlists = client.asking_the_server().get_playlists(None).await?;
    if is_taken(&playlists.playlist, name) {
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

    fn made(title: &str, patch: Option<&str>) -> lb::Playlist {
        lb::Playlist {
            mbid: title.into(),
            title: title.into(),
            annotation: None,
            source_patch: patch.map(str::to_owned),
            date: None,
            tracks: Vec::new(),
        }
    }

    fn made_on(title: &str, patch: &str, date: &str) -> lb::Playlist {
        lb::Playlist {
            date: Some(date.into()),
            ..made(title, Some(patch))
        }
    }

    fn kind(patch: Option<&str>, is_last_week: bool) -> (String, KindTint) {
        kind_of(&made("Own title", patch), is_last_week)
    }

    #[test]
    fn the_algorithm_names_the_kind_and_picks_its_colour() {
        assert_eq!(
            kind(Some("weekly-jams"), false),
            ("Weekly Jams".into(), KindTint::Green)
        );
        assert_eq!(
            kind(Some("weekly-jams"), true),
            ("Last Week's Jams".into(), KindTint::Green)
        );
        assert_eq!(
            kind(Some("daily-jams"), false),
            ("Daily Jams".into(), KindTint::Blue)
        );
        assert_eq!(
            kind(Some("top-discoveries-of-2025"), false),
            ("2025 Top Discoveries".into(), KindTint::Red)
        );
        assert_eq!(
            kind(Some("top-missed-recordings-of-2025"), false),
            ("2025 Missed Tracks".into(), KindTint::Red)
        );
        assert_eq!(
            kind(Some("lb-radio"), false),
            ("Own title".into(), KindTint::Blue)
        );
        assert_eq!(
            kind(Some("something-new"), false),
            ("Own title".into(), KindTint::Blue)
        );
        assert_eq!(kind(None, false), ("Own title".into(), KindTint::Blue));
    }

    #[test]
    fn every_yearly_playlist_listenbrainz_makes_is_red_with_its_year() {
        // As ListenBrainz lists them for the user "rob".
        let yearly = |patch: Option<&str>, title: &str| kind_of(&made(title, patch), false);
        assert_eq!(
            yearly(
                Some("top-discoveries-for-year"),
                "Top Discoveries of 2022 for rob"
            ),
            ("2022 Top Discoveries".into(), KindTint::Red)
        );
        assert_eq!(
            yearly(
                Some("top-recordings-for-year"),
                "Top Recordings of 2021 for rob"
            ),
            ("2021 Top Recordings".into(), KindTint::Red)
        );
        assert_eq!(
            yearly(
                Some("top-new-recordings-for-year"),
                "Top New Releases in 2021 for rob"
            ),
            ("2021 Top New Releases".into(), KindTint::Red)
        );
        assert_eq!(
            yearly(
                None,
                "Top Missed Recordings of 2022 for rob (Updated 2022-02-02)"
            ),
            ("2022 Missed Tracks".into(), KindTint::Red)
        );
        // Without a year to be found, the kind alone.
        assert_eq!(
            yearly(Some("top-recordings-for-year"), "Top Recordings for rob"),
            ("Top Recordings".into(), KindTint::Red)
        );
        // A playlist without a patch and a title of its own is blue.
        assert_eq!(
            yearly(None, "Hand made 2022"),
            ("Hand made 2022".into(), KindTint::Blue)
        );
    }

    #[test]
    fn a_date_counts_days_from_the_epoch_whatever_the_month_and_year() {
        let day = |date: &str| made_on("x", "x", date).day_number();
        assert_eq!(day("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(day("2000-03-01T00:00:00+00:00"), Some(11_017));
        assert_eq!(day("2024-02-29T23:59:59+00:00"), Some(19_782));
        assert_eq!(day("2026-08-23T22:07:21.126678+00:00"), Some(20_688));
        assert_eq!(day("2026-08-16"), Some(20_681));
        assert_eq!(day("not a date"), None);
        assert_eq!(day("2026-13-01"), None);
        assert_eq!(made("x", None).day_number(), None);
    }

    #[test]
    fn only_the_weekly_playlist_of_the_week_before_is_last_weeks() {
        let weekly = |date: &str| made_on(&format!("Weekly {date}"), "weekly-jams", date);
        let tiles = tiles(&[
            weekly("2026-08-23T22:07:21+00:00"),
            made_on("Daily", "daily-jams", "2026-08-24T01:00:00+00:00"),
            weekly("2026-08-16T22:07:13+00:00"),
            weekly("2026-07-12T22:07:13+00:00"),
            made("Weekly without a date", Some("weekly-jams")),
        ]);
        let titles: Vec<String> = tiles.iter().map(|tile| tile.title.to_string()).collect();
        assert_eq!(
            titles,
            [
                "Weekly Jams",
                "Daily Jams",
                "Last Week's Jams",
                "Weekly Jams",
                "Weekly Jams"
            ]
        );
        let beneath = tiles[3]
            .names
            .row_data(0)
            .map(|name| name.label.to_string());
        assert_eq!(beneath.as_deref(), Some("Weekly 2026-07-12T22:07:13+00:00"));
    }

    #[test]
    fn only_a_weekly_playlist_six_to_eight_days_before_the_newest_is_last_weeks() {
        let weekly = |date: &str| made_on(date, "weekly-jams", date);
        let titles: Vec<String> = tiles(&[
            weekly("2026-08-23T22:00:00+00:00"),
            weekly("2026-08-17T22:00:00+00:00"),
            weekly("2026-08-15T22:00:00+00:00"),
            weekly("2026-08-18T22:00:00+00:00"),
            weekly("2026-08-14T22:00:00+00:00"),
        ])
        .iter()
        .map(|tile| tile.title.to_string())
        .collect();
        assert_eq!(
            titles,
            [
                "Weekly Jams",
                "Last Week's Jams",
                "Last Week's Jams",
                "Weekly Jams",
                "Weekly Jams"
            ]
        );
    }

    #[test]
    fn a_playlist_whose_tracks_failed_to_come_is_asked_for_again() {
        let mut state = State {
            shelf: vec![made("a", None), made("b", None), made("c", None)],
            ..State::default()
        };
        let with_tracks = lb::Playlist {
            tracks: vec![wanted("T", "Ann", None)],
            ..made("a", None)
        };
        keep_tracks(&mut state, "a".into(), Ok(with_tracks));
        let limited = lb::Error::RateLimited { reset_in: Some(30) };
        keep_tracks(&mut state, "b".into(), Err(limited));
        assert_eq!(playlists_without_tracks(&state), ["b", "c"]);
        assert_eq!(state.shelf_tracks.keys().collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn only_a_rate_limit_answer_ends_a_round_of_fetches() {
        let limited = Err(lb::Error::RateLimited { reset_in: None });
        let broken = Err(lb::Error::Api {
            status: 500,
            message: String::new(),
        });
        assert!(is_rate_limited(&limited));
        assert!(!is_rate_limited(&broken));
        assert!(!is_rate_limited(&Ok(made("a", None))));
    }

    #[test]
    fn a_server_without_banners_gives_none_even_when_the_artists_have_them() {
        let b = Browser {
            artists: vec![artist("a", "Ann", None, Some("banner-a"))],
            created_for: State {
                shelf: vec![made("one", None)],
                shelf_tracks: HashMap::from([("one".to_owned(), vec![by(&[("Ann", None)])])]),
                ..State::default()
            },
            ..Browser::default()
        };
        assert_eq!(banners_of(&b), [None]);
    }

    #[test]
    fn each_tile_keeps_its_kinds_colour() {
        let tints: Vec<KindTint> = tiles(&[
            made("a", Some("weekly-jams")),
            made("b", Some("daily-jams")),
            made("c", Some("top-recordings-for-year")),
            made("d", Some("lb-radio")),
        ])
        .iter()
        .map(|tile| tile.tint)
        .collect();
        assert_eq!(
            tints,
            [
                KindTint::Green,
                KindTint::Blue,
                KindTint::Red,
                KindTint::Blue
            ]
        );
    }

    #[test]
    fn a_tile_names_its_playlist_beneath_unless_that_is_its_title() {
        let tiles = tiles(&[
            made(
                "Weekly Jams for joe, week of 2026-11-02",
                Some("weekly-jams"),
            ),
            made(
                "LB Radio for country germany on easy mode",
                Some("lb-radio"),
            ),
        ]);
        let beneath = |row: usize| {
            tiles[row]
                .names
                .row_data(0)
                .map(|name| name.label.to_string())
        };
        assert_eq!(
            beneath(0).as_deref(),
            Some("Weekly Jams for joe, week of 2026-11-02")
        );
        assert_eq!(beneath(1), None);
        assert_eq!(
            tiles[0].link,
            "createdfor:Weekly Jams for joe, week of 2026-11-02"
        );
    }

    fn artist(id: &str, name: &str, mbid: Option<&str>, banner: Option<&str>) -> ArtistID3 {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "musicBrainzId": mbid, "bannerArt": banner
        }))
        .unwrap()
    }

    fn by(artists: &[(&str, Option<&str>)]) -> lb::PlaylistTrack {
        let mut track = wanted("T", "joined", None);
        track.artists = artists.iter().map(|(name, _)| (*name).to_owned()).collect();
        track.artist_mbids = artists
            .iter()
            .map(|(_, mbid)| mbid.map(str::to_owned))
            .collect();
        track
    }

    fn ranked(tracks: &[lb::PlaylistTrack], artists: &[ArtistID3]) -> Vec<(String, usize)> {
        (artists_in(tracks, artists).into_iter())
            .map(|(name, tracks)| (name.link.to_string(), tracks))
            .collect()
    }

    #[test]
    fn the_artist_with_most_tracks_is_first_and_ties_keep_the_playlist_order() {
        let artists = [
            artist("a", "Ann", None, None),
            artist("b", "Bo", None, None),
            artist("c", "Cy", None, None),
        ];
        let tracks = [
            by(&[("Cy", None)]),
            by(&[("Bo", None)]),
            by(&[("Ann", None), ("Bo", None)]),
            by(&[("Nobody", None)]),
        ];
        assert_eq!(
            ranked(&tracks, &artists),
            [
                ("artist:b".to_owned(), 2),
                ("artist:c".to_owned(), 1),
                ("artist:a".to_owned(), 1)
            ]
        );
    }

    #[test]
    fn an_artist_is_matched_by_its_id_else_by_name_and_counts_once_per_track() {
        let artists = [
            artist("a", "Ann Lee", Some("MB-A"), None),
            artist("b", "Bo", None, None),
        ];
        let tracks = [
            by(&[("Anne Lee", Some("mb-a"))]),
            by(&[("ann lee", Some("mb-other"))]),
            by(&[("Bo", None), ("BO", None)]),
            by(&[("Bo", Some("mb-unknown"))]),
        ];
        assert_eq!(
            ranked(&tracks, &artists),
            [("artist:a".to_owned(), 2), ("artist:b".to_owned(), 2)]
        );
    }

    #[test]
    fn no_playlist_shows_a_banner_an_earlier_one_does_and_a_bannerless_artist_is_passed_over() {
        let artists = [
            artist("a", "Ann", None, None),
            artist("b", "Bo", None, Some("banner-b")),
            artist("c", "Cy", None, Some("banner-c")),
        ];
        let playlists = [
            made("one", None),
            made("two", None),
            made("three", None),
            made("four", None),
        ];
        let tracks = HashMap::from([
            (
                "one".to_owned(),
                vec![
                    by(&[("Ann", None)]),
                    by(&[("Ann", None)]),
                    by(&[("Bo", None)]),
                ],
            ),
            ("two".to_owned(), vec![by(&[("Bo", None)])]),
            ("three".to_owned(), vec![by(&[("Bo", None)])]),
            // The tracks of "four" couldn't be fetched.
            ("four".to_owned(), Vec::new()),
        ]);
        assert_eq!(
            shelf_banners(&playlists, &tracks, &artists, true),
            [Some("banner-b".to_owned()), None, None, None]
        );
        let tracks = HashMap::from([
            ("one".to_owned(), vec![by(&[("Bo", None)])]),
            (
                "two".to_owned(),
                vec![by(&[("Bo", None)]), by(&[("Cy", None)])],
            ),
        ]);
        assert_eq!(
            shelf_banners(&playlists[..2], &tracks, &artists, true),
            [Some("banner-b".to_owned()), Some("banner-c".to_owned())]
        );
        assert_eq!(
            shelf_banners(&playlists[..2], &tracks, &artists, false),
            [None, None]
        );
    }

    #[test]
    fn exploration_playlists_are_not_offered() {
        let playlist = |title: &str, patch: Option<&str>| lb::Playlist {
            mbid: title.into(),
            title: title.into(),
            annotation: None,
            source_patch: patch.map(str::to_owned),
            date: None,
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
