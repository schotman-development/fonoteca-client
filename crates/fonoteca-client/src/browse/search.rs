//! The search the top bar's button opens. It looks for the server's
//! artists, albums and songs with search3, for the playlists and saved
//! stations the app holds by their names, on servers with Fonoteca's
//! compositions for its composers and compositions by their names, and for
//! the radio directory's stations. It searches once typing pauses, or at
//! once on Enter, and shows what it found last until the next answers are
//! in. Nothing of it is cached.

use opensubsonic::SearchResult3;

use super::*;

/// How long typing pauses before the search goes out.
const PAUSE: Duration = Duration::from_millis(300);

/// Where the radio directory's stations found are kept among the shelves',
/// so they play and are saved as a shelf's are.
pub(super) const RADIO: usize = GENRE_RADIO + 1;

/// What the search found last, kept for coming back to its page.
#[derive(Default)]
pub(super) struct State {
    /// The query searched for last, and how many searches there have been,
    /// so the answers to one a later search took over from are dropped.
    query: String,
    round: u32,
    songs: Vec<Child>,
    artists: Rc<VecModel<AlbumTile>>,
    albums: Rc<VecModel<AlbumTile>>,
    composers: Rc<VecModel<AlbumTile>>,
    works: Rc<VecModel<Track>>,
    playlists: Rc<VecModel<AlbumTile>>,
    saved: Rc<VecModel<AlbumTile>>,
    pub(super) radio: Rc<VecModel<AlbumTile>>,
}

pub(super) fn wire(ui: &AppWindow, browser: &Shared) {
    ui.on_search_typed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let (weak, shared) = (weak.clone(), browser.clone());
            browser
                .borrow()
                .typing
                .start(TimerMode::SingleShot, PAUSE, move || {
                    let ui = weak.unwrap();
                    // Enter may have searched for it already.
                    if ui.get_search_query().trim() != shared.borrow().search.query {
                        search(&ui, &shared);
                    }
                });
        }
    });
    ui.on_search_now({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || search(&weak.unwrap(), &browser)
    });
}

/// The search's page: what it found last, at once, and the same search
/// again for what changed since. The composers and compositions it looks
/// through are asked for unless they are in.
pub(super) fn show_page(ui: &AppWindow, browser: &Shared) {
    ui.set_header_title("Search".into());
    {
        let mut b = browser.borrow_mut();
        let songs = b.search.songs.clone();
        let rows = songs.iter().map(song_track).collect();
        show_songs(&mut b, songs, rows);
        show_kept(ui, &b.search);
    }
    if offers_compositions(&browser.borrow()) {
        composers(ui, browser);
        compositions(ui, browser);
    }
    search(ui, browser);
}

fn show_kept(ui: &AppWindow, kept: &State) {
    ui.set_found_artists(ModelRc::from(kept.artists.clone()));
    ui.set_found_albums(ModelRc::from(kept.albums.clone()));
    ui.set_found_composers(ModelRc::from(kept.composers.clone()));
    ui.set_found_works(ModelRc::from(kept.works.clone()));
    ui.set_found_playlists(ModelRc::from(kept.playlists.clone()));
    ui.set_found_stations(ModelRc::from(kept.saved.clone()));
    ui.set_found_radio(radio_shelf(&kept.radio));
}

/// Searches for what is typed: at once among the lists the app holds, and
/// on the server and in the radio directory, whose answers take the place
/// of what the page shows as they come in.
fn search(ui: &AppWindow, browser: &Shared) {
    if ui.get_page() != Page::Search {
        return;
    }
    let query = ui.get_search_query().trim().to_owned();
    let round = {
        let mut b = browser.borrow_mut();
        b.search.query = query.clone();
        b.search.round += 1;
        b.search.round
    };
    ui.set_page_problem("".into());
    ui.set_search_answered(false);
    show_lists(ui, browser);
    if query.is_empty() {
        let nothing = SearchResult3 {
            artist: Vec::new(),
            album: Vec::new(),
            song: Vec::new(),
        };
        show_found(ui, browser, nothing);
        show_radio(ui, browser, Vec::new());
        ui.set_loading(false);
        return;
    }
    ask(ui, browser, query, round);
}

/// Asks the server, then the radio directory, for what matches `query`,
/// and shows each answer unless a later search took over meanwhile.
fn ask(ui: &AppWindow, browser: &Shared, query: String, round: u32) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let directory = directory(browser);
    let visit = browser.borrow().visit;
    ui.set_loading(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let found = client.search3(&query, &SearchOptions::default()).await;
        let Some(ui) = weak.upgrade() else { return };
        if !is_current(&browser, visit, round) {
            return;
        }
        match found {
            Ok(found) => show_found(&ui, &browser, found),
            Err(e) => ui.set_page_problem(e.to_string().into()),
        }
        if let Some(directory) = directory {
            learn(&browser, &directory).await;
            let stations = directory.named_like(&query, SHELF_STATIONS).await;
            if !is_current(&browser, visit, round) {
                return;
            }
            match stations {
                Ok(stations) => show_radio(&ui, &browser, stations),
                Err(e) if ui.get_page_problem().is_empty() => {
                    let problem = format!("The radio directory didn't answer: {e}");
                    ui.set_page_problem(problem.into());
                }
                Err(_) => {}
            }
        }
        ui.set_loading(false);
        ui.set_search_answered(true);
    })
    .expect("the event loop is running");
}

/// Whether search `round` is the latest, on the page shown at `visit`.
fn is_current(browser: &Shared, visit: i32, round: u32) -> bool {
    let b = browser.borrow();
    b.visit == visit && b.search.round == round
}

/// The server's artists, albums and songs, in place of those shown.
fn show_found(ui: &AppWindow, browser: &Shared, found: SearchResult3) {
    let size = tile_pixels(ui);
    let artists = found.artist.iter().map(artist_tile).collect();
    let albums = (found.album.iter())
        .map(|album| album_tile(album, album_names(album)))
        .collect();
    let rows = found.song.iter().map(song_track).collect();
    let (wanted_artists, wanted_albums) = {
        let mut b = browser.borrow_mut();
        let (artists, wanted_artists) = seen_covers(&b, artists, size, false);
        let (albums, wanted_albums) = seen_covers(&b, albums, size, false);
        b.search.artists = artists;
        b.search.albums = albums;
        b.search.songs = found.song.clone();
        show_songs(&mut b, found.song, rows);
        ui.set_found_artists(ModelRc::from(b.search.artists.clone()));
        ui.set_found_albums(ModelRc::from(b.search.albums.clone()));
        (wanted_artists, wanted_albums)
    };
    side_covers(browser, |b| &b.search.artists, wanted_artists, size, false);
    side_covers(browser, |b| &b.search.albums, wanted_albums, size, false);
}

/// The playlists, saved stations, composers and compositions whose names
/// match the query searched, in place of those shown. Only servers with
/// Fonoteca's compositions have composers and compositions.
pub(super) fn show_lists(ui: &AppWindow, browser: &Shared) {
    let size = tile_pixels(ui);
    let (wanted_playlists, wanted_composers) = {
        let mut b = browser.borrow_mut();
        let query = b.search.query.clone();
        let playlists = named(&b.playlists, &query, |playlist| &playlist.name)
            .map(playlist_tile)
            .collect();
        let composers = named(&b.composers, &query, |composer| &composer.name)
            .map(composer_tile)
            .collect();
        let works: Vec<Track> = named(&b.compositions, &query, |piece| &piece.name)
            .map(|piece| composition_row(piece, None))
            .collect();
        let stations: Vec<AlbumTile> = b.saved.iter().collect();
        let saved: Vec<AlbumTile> = named(&stations, &query, |station| &station.title)
            .cloned()
            .collect();
        let (playlists, wanted_playlists) = seen_covers(&b, playlists, size, false);
        let (composers, wanted_composers) = seen_covers(&b, composers, size, false);
        b.search.playlists = playlists;
        b.search.composers = composers;
        b.search.works = Rc::new(VecModel::from(works));
        b.search.saved = Rc::new(VecModel::from(saved));
        ui.set_found_playlists(ModelRc::from(b.search.playlists.clone()));
        ui.set_found_composers(ModelRc::from(b.search.composers.clone()));
        ui.set_found_works(ModelRc::from(b.search.works.clone()));
        ui.set_found_stations(ModelRc::from(b.search.saved.clone()));
        (wanted_playlists, wanted_composers)
    };
    side_covers(
        browser,
        |b| &b.search.playlists,
        wanted_playlists,
        size,
        false,
    );
    side_covers(
        browser,
        |b| &b.search.composers,
        wanted_composers,
        size,
        false,
    );
}

/// The first of `items` whose `name` has every word of `query` start one
/// of its words; none for an empty query.
fn named<'a, T, N: AsRef<str>>(
    items: &'a [T],
    query: &str,
    name: impl Fn(&T) -> &N,
) -> impl Iterator<Item = &'a T> {
    let found = if query.is_empty() {
        Vec::new()
    } else {
        filtered(items, query, |item| name(item).as_ref().to_owned())
    };
    found.into_iter().take(SHELF).map(move |at| &items[at])
}

/// The radio directory's stations, in place of those found before, as tiles
/// that play them, their logos asked for.
fn show_radio(ui: &AppWindow, browser: &Shared, stations: Vec<Station>) {
    let found = stations.len();
    {
        let mut b = browser.borrow_mut();
        let tiles: Vec<AlbumTile> = (stations.iter().enumerate())
            .map(|(row, station)| AlbumTile {
                link: format!("{STATION_LINK}shelf:{RADIO}:{row}").into(),
                ..station_tile(&b, station)
            })
            .collect();
        b.search.radio = Rc::new(VecModel::from(tiles));
        b.logos_asked.retain(|(shelf, _)| *shelf != RADIO);
        b.shelves.insert(RADIO, stations);
        ui.set_found_radio(radio_shelf(&b.search.radio));
    }
    shelf_logos(browser, RADIO, 0..found);
}

fn radio_shelf(tiles: &Rc<VecModel<AlbumTile>>) -> StationShelf {
    StationShelf {
        title: "Radio directory".into(),
        shelf: RADIO as i32,
        tiles: ModelRc::from(tiles.clone()),
        ..StationShelf::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_by_the_starts_of_their_words() {
        let names = ["Road Trip", "Empty", "Trip Hop", "Railroad"];
        let found: Vec<_> = named(&names, "tri", |name| name).collect();
        assert_eq!(found, [&"Road Trip", &"Trip Hop"]);
        let found: Vec<_> = named(&names, "road TRIP", |name| name).collect();
        assert_eq!(found, [&"Road Trip"]);
        assert_eq!(named(&names, "", |name| name).count(), 0);
    }

    #[test]
    fn at_most_a_shelf_matches() {
        let names = vec!["Song"; SHELF + 1];
        assert_eq!(named(&names, "song", |name| name).count(), SHELF);
    }
}
