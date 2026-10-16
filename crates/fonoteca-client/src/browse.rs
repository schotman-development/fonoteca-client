//! The library pages after sign-in: what each page shows, where its links lead,
//! and the history behind back and forward. A link is a page's address, such
//! as "albums", "artists", "album:<id>" or "artist:<id>".

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frame, Frames};
use opensubsonic::{AlbumID3, AlbumListType, ArtistID3, Child, Client, ItemDate, ListOptions};
use slint::{
    ComponentHandle, Image, Model, ModelRc, SharedPixelBuffer, Timer, TimerMode, VecModel,
};

use crate::{AlbumTile, AppWindow, Name, Page, Track};

/// Tiles added to the albums or artists page at a time.
const TILES_AT_ONCE: usize = 100;
/// Albums asked for at a time while the library loads, and at most how many,
/// against servers that ignore the offset.
const LIBRARY_AT_ONCE: u32 = 500;
const LIBRARY_AT_MOST: usize = 50_000;
/// How long typing in the filter pauses before the albums are filtered.
const TYPING_PAUSE: Duration = Duration::from_millis(200);
/// Tile covers fetched at the same time.
const COVERS_AT_ONCE: usize = 6;
/// The sizes covers are asked for, in logical pixels. Grid tiles are at least
/// 200 wide and widen to fill their row, which keeps them under 300 on all
/// but the narrowest windows; page headers are 220.
const TILE_SIZE: f32 = 300.0;
const HEADER_SIZE: f32 = 220.0;
/// Fonoteca's extension for motion covers.
const MOTION: &str = "fonotecaAlbumMotion";

#[derive(Default)]
pub struct Browser {
    client: Option<Rc<Client>>,
    listenbrainz: Option<Rc<listenbrainz::Client>>,
    history: Vec<String>,
    at: usize,
    /// Counts the pages shown, so an answer for a page left behind is dropped.
    visit: i32,
    tiles: Rc<VecModel<AlbumTile>>,
    tracks: Rc<VecModel<Track>>,
    /// Song id and MusicBrainz recording id of each track row.
    songs: Vec<(String, Option<String>)>,
    /// Tile rows whose covers are still to fetch, the rows on screen or just
    /// below it, and how many fetches run.
    covers: BTreeMap<usize, String>,
    in_view: Range<usize>,
    fetching: usize,
    /// Covers seen this session, by cover id and size.
    // ponytail: unbounded, every cover seen stays decoded; an LRU when big libraries need it.
    pictures: HashMap<(String, u32), Image>,
    /// Shows the next frame of the album's motion cover.
    motion: Timer,
    /// Every album on the server in the order they arrived, while they arrive
    /// and once they all have.
    library: Vec<AlbumID3>,
    library_loading: bool,
    library_loaded: bool,
    /// Every artist on the server, once they have arrived.
    artists: Vec<ArtistID3>,
    artists_loaded: bool,
    /// The filter of the albums or artists page on screen, and what it
    /// shows, as indexes into `library` or `artists` in the page's order.
    filter: String,
    view: Vec<usize>,
    /// Filters the albums or artists once typing pauses.
    typing: Timer,
    /// The links of the albums selected, in the order they were picked. They
    /// stay selected through sorting, filtering and other pages.
    selected: Vec<String>,
}

pub type Shared = Rc<RefCell<Browser>>;

/// Opens the albums page with a signed-in client.
pub fn start(
    ui: &AppWindow,
    browser: &Shared,
    client: Client,
    listenbrainz: Option<listenbrainz::Client>,
) {
    {
        let mut b = browser.borrow_mut();
        b.client = Some(Rc::new(client));
        b.listenbrainz = listenbrainz.map(Rc::new);
        b.history.clear();
    }
    ui.set_album_filter("".into());
    ui.set_artist_filter("".into());
    open(ui, browser, "albums".into());
}

/// Whether the library is open: signed in, and not signed out since.
pub fn started(browser: &Shared) -> bool {
    browser.borrow().client.is_some()
}

/// Forgets the client and the pages seen, on sign out.
pub fn stop(browser: &Shared) {
    let visit = browser.borrow().visit;
    *browser.borrow_mut() = Browser {
        visit: visit + 1,
        ..Browser::default()
    };
}

pub fn wire(ui: &AppWindow, browser: &Shared) {
    ui.on_open({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |link| open(&weak.unwrap(), &browser, link.into())
    });
    ui.on_show_section({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| {
            let link = match index {
                1 => "artists",
                2 => "account",
                _ => "albums",
            };
            open(&weak.unwrap(), &browser, link.into());
        }
    });
    ui.on_back({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || step(&weak.unwrap(), &browser, -1)
    });
    ui.on_forward({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || step(&weak.unwrap(), &browser, 1)
    });
    ui.on_show_more({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || more_tiles(&weak.unwrap(), &browser)
    });
    ui.on_toggle_love({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| toggle_love(&weak.unwrap(), &browser, row as usize)
    });
    ui.on_sort_albums({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || show(&weak.unwrap(), &browser)
    });
    let filter = {
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let (weak, shared) = (weak.clone(), browser.clone());
            browser
                .borrow()
                .typing
                .start(TimerMode::SingleShot, TYPING_PAUSE, move || {
                    let ui = weak.unwrap();
                    if typed_filter(&ui).is_some_and(|typed| shared.borrow().filter != typed) {
                        show(&ui, &shared);
                    }
                });
        }
    };
    ui.on_filter_albums(filter.clone());
    ui.on_filter_artists(filter);
    ui.on_tiles_in_view({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |first, last| {
            let first = first.max(0) as usize;
            browser.borrow_mut().in_view = first..((last + 1).max(0) as usize).max(first);
            fetch_covers(&weak.unwrap(), &browser);
        }
    });
    ui.on_select_tile({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| select_tile(&weak.unwrap(), &browser, row as usize)
    });
    ui.on_clear_selection({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || clear_selection(&weak.unwrap(), &browser)
    });
}

/// Selects the tile at `row`, or lets it go if it is selected.
fn select_tile(ui: &AppWindow, browser: &Shared, row: usize) {
    let tiles = browser.borrow().tiles.clone();
    let Some(mut tile) = tiles.row_data(row) else {
        return;
    };
    let count = {
        let mut b = browser.borrow_mut();
        let link = tile.link.to_string();
        tile.selected = match b.selected.iter().position(|picked| *picked == link) {
            Some(at) => {
                b.selected.remove(at);
                false
            }
            None => {
                b.selected.push(link);
                true
            }
        };
        b.selected.len()
    };
    tiles.set_row_data(row, tile);
    ui.set_selected_tiles(count as i32);
}

fn clear_selection(ui: &AppWindow, browser: &Shared) {
    let tiles = {
        let mut b = browser.borrow_mut();
        b.selected.clear();
        b.tiles.clone()
    };
    for row in 0..tiles.row_count() {
        if let Some(mut tile) = tiles.row_data(row).filter(|tile| tile.selected) {
            tile.selected = false;
            tiles.set_row_data(row, tile);
        }
    }
    ui.set_selected_tiles(0);
}

fn open(ui: &AppWindow, browser: &Shared, link: String) {
    {
        let mut b = browser.borrow_mut();
        if b.history.get(b.at) == Some(&link) {
            return;
        }
        let keep = if b.history.is_empty() { 0 } else { b.at + 1 };
        b.history.truncate(keep);
        b.history.push(link);
        b.at = b.history.len() - 1;
    }
    show(ui, browser);
}

/// Moves `by` pages back (-1) or forward (1) through the history.
fn step(ui: &AppWindow, browser: &Shared, by: isize) {
    {
        let mut b = browser.borrow_mut();
        match b.at.checked_add_signed(by) {
            Some(at) if at < b.history.len() => b.at = at,
            _ => return,
        }
    }
    show(ui, browser);
}

fn show(ui: &AppWindow, browser: &Shared) {
    let link = {
        let mut b = browser.borrow_mut();
        b.visit += 1;
        b.tiles = Rc::default();
        b.tracks = Rc::default();
        b.songs.clear();
        b.covers.clear();
        b.fetching = 0;
        b.motion = Timer::default();
        ui.set_visit(b.visit);
        ui.set_can_go_back(b.at > 0);
        ui.set_can_go_forward(b.at + 1 < b.history.len());
        ui.set_tiles(ModelRc::from(b.tiles.clone()));
        ui.set_tracks(ModelRc::from(b.tracks.clone()));
        b.history[b.at].clone()
    };
    ui.set_page_problem("".into());
    ui.set_loading(false);
    ui.set_more_tiles(false);
    ui.set_selected_tiles(browser.borrow().selected.len() as i32);
    ui.set_header_image(Image::default());
    ui.set_header_title("".into());
    ui.set_header_names(ModelRc::default());
    ui.set_header_facts("".into());

    let (page, id) = link.split_once(':').unwrap_or((&link, ""));
    let id = id.to_owned();
    match page {
        "artists" => {
            ui.set_page(Page::Artists);
            ui.set_section(1);
            artists(ui, browser);
        }
        "album" => {
            ui.set_page(Page::Album);
            ui.set_section(-1);
            album(ui, browser, id);
        }
        "artist" => {
            ui.set_page(Page::Artist);
            ui.set_section(-1);
            artist(ui, browser, id);
        }
        "account" => {
            ui.set_page(Page::Account);
            ui.set_section(2);
        }
        _ => {
            ui.set_page(Page::Albums);
            ui.set_section(0);
            albums(ui, browser);
        }
    }
}

/// Runs `task` for the page on screen, and drops its answer if that page has
/// been left by then.
fn load<T: 'static, F: Future<Output = opensubsonic::Result<T>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    task: impl FnOnce(Rc<Client>) -> F,
    done: impl FnOnce(&AppWindow, &Shared, T) + 'static,
) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let visit = browser.borrow().visit;
    let task = task(client);
    ui.set_loading(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = task.await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().visit != visit {
            return;
        }
        ui.set_loading(false);
        match result {
            Ok(value) => done(&ui, &browser, value),
            Err(e) => ui.set_page_problem(e.to_string().into()),
        }
    })
    .expect("the event loop is running");
}

/// Shows the library on the albums page, asking for it first if it isn't all
/// there.
fn albums(ui: &AppWindow, browser: &Shared) {
    let ask = {
        let b = browser.borrow();
        !b.library_loaded && !b.library_loading
    };
    if ask {
        load_library(ui, browser);
    }
    arrange(ui, browser);
}

/// Asks for every album, a page at a time, and arranges the albums page as
/// they arrive. Sorting and filtering then need nothing from the server.
fn load_library(ui: &AppWindow, browser: &Shared) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    browser.borrow_mut().library_loading = true;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        loop {
            let options = ListOptions {
                size: Some(LIBRARY_AT_ONCE),
                offset: Some(browser.borrow().library.len() as u32),
                ..ListOptions::default()
            };
            let list = client
                .get_album_list2(&AlbumListType::AlphabeticalByName, &options)
                .await;
            let Some(ui) = weak.upgrade() else { return };
            let signed_in = browser
                .borrow()
                .client
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, &client));
            if !signed_in {
                return;
            }
            let shown = ui.get_page() == Page::Albums;
            let list = match list {
                Ok(list) => list.album,
                Err(e) => {
                    browser.borrow_mut().library_loading = false;
                    if shown {
                        ui.set_loading(false);
                        ui.set_page_problem(e.to_string().into());
                    }
                    return;
                }
            };
            let done = {
                let mut b = browser.borrow_mut();
                let last = (list.len() as u32) < LIBRARY_AT_ONCE;
                b.library.extend(list);
                let done = last || b.library.len() >= LIBRARY_AT_MOST;
                b.library_loading = !done;
                b.library_loaded = done;
                done
            };
            if shown {
                arrange(&ui, &browser);
            }
            if done {
                return;
            }
        }
    })
    .expect("the event loop is running");
}

/// Shows every artist on the artists page, asking for them first if they
/// haven't arrived yet.
fn artists(ui: &AppWindow, browser: &Shared) {
    if browser.borrow().artists_loaded {
        arrange(ui, browser);
        return;
    }
    load(
        ui,
        browser,
        |client| async move { client.get_artists(None).await },
        |ui, browser, artists| {
            {
                let mut b = browser.borrow_mut();
                b.artists = artists
                    .index
                    .into_iter()
                    .flat_map(|index| index.artist)
                    .collect();
                b.artists_loaded = true;
            }
            arrange(ui, browser);
        },
    );
}

/// What is typed in the filter of the page on screen, if it has one.
fn typed_filter(ui: &AppWindow) -> Option<String> {
    let typed = match ui.get_page() {
        Page::Albums => ui.get_album_filter(),
        Page::Artists => ui.get_artist_filter(),
        _ => return None,
    };
    Some(typed.trim().to_owned())
}

/// Puts the library in the albums page's order, or the artists in the
/// server's, with the page's filter. The tiles shown stay when they are still
/// the first ones; otherwise as many are shown again, and at least a page.
fn arrange(ui: &AppWindow, browser: &Shared) {
    let (sort, descending) = (ui.get_album_sort(), ui.get_album_descending());
    let filter = typed_filter(ui).unwrap_or_default();
    let mut b = browser.borrow_mut();
    let view = if ui.get_page() == Page::Artists {
        filtered(&b.artists, &filter)
    } else {
        arranged(&b.library, &filter, sort, descending)
    };
    b.filter = filter;
    let shown = b.tiles.row_count();
    if view.get(..shown) != b.view.get(..shown) {
        b.tiles = Rc::default();
        b.covers.clear();
        b.fetching = 0;
        ui.set_tiles(ModelRc::from(b.tiles.clone()));
    }
    b.view = view;
    drop(b);
    show_tiles(ui, browser, shown.max(TILES_AT_ONCE));
}

/// Shows the next tiles, and asks for the library or the artists again if
/// that failed.
fn more_tiles(ui: &AppWindow, browser: &Shared) {
    ui.set_page_problem("".into());
    if ui.get_page() == Page::Artists {
        if !browser.borrow().artists_loaded {
            artists(ui, browser);
            return;
        }
    } else {
        let ask = {
            let b = browser.borrow();
            !b.library_loaded && !b.library_loading
        };
        if ask {
            load_library(ui, browser);
        }
    }
    let shown = browser.borrow().tiles.row_count();
    show_tiles(ui, browser, shown + TILES_AT_ONCE);
}

/// Shows the page's albums or artists up to the `upto`th.
fn show_tiles(ui: &AppWindow, browser: &Shared, upto: usize) {
    let artists_page = ui.get_page() == Page::Artists;
    let (tiles, more, waiting) = {
        let b = browser.borrow();
        let end = upto.min(b.view.len());
        let start = b.tiles.row_count().min(end);
        let tiles: Vec<_> = b.view[start..end]
            .iter()
            .map(|&i| {
                if artists_page {
                    artist_tile(&b.artists[i])
                } else {
                    album_tile(&b.library[i], album_names(&b.library[i]))
                }
            })
            .collect();
        // The artists arrive at once, the library a part at a time.
        let more = end < b.view.len() || (!artists_page && !b.library_loaded);
        (
            tiles,
            more,
            !artists_page && b.library_loading && b.view.is_empty(),
        )
    };
    ui.set_more_tiles(more);
    ui.set_loading(waiting);
    add_tiles(ui, browser, tiles);
}

/// The albums where every word of `filter` starts a word of their name or
/// artists, as indexes into `library`, in the order of the Sort menu's
/// `sort`. Each order is sorted ascending and turned around for descending.
fn arranged(library: &[AlbumID3], filter: &str, sort: i32, descending: bool) -> Vec<usize> {
    let filter = filter.to_lowercase();
    let wanted: Vec<&str> = words(&filter).collect();
    let mut view: Vec<usize> = (0..library.len())
        .filter(|&i| {
            let album = &library[i];
            let text = album_names(album)
                .iter()
                .fold(album.name.to_lowercase(), |text, name| {
                    text + " " + &name.label.to_lowercase()
                });
            matches(&wanted, &text)
        })
        .collect();
    let name = |i: &usize| library[*i].name.to_lowercase();
    match sort {
        1 => view.sort_by_cached_key(|i| (sort_artist(&library[*i]).to_lowercase(), name(i))),
        2 => view.sort_by(|a, b| library[*a].created.cmp(&library[*b].created)),
        3 => {
            view.retain(|i| library[*i].year.is_some_and(|year| year > 0));
            view.sort_by_cached_key(|i| (library[*i].year, name(i)));
        }
        _ => view.sort_by_cached_key(name),
    }
    if descending {
        view.reverse();
    }
    view
}

/// The artists where every word of `filter` starts a word of their name, as
/// indexes into `artists`, in the server's order.
fn filtered(artists: &[ArtistID3], filter: &str) -> Vec<usize> {
    let filter = filter.to_lowercase();
    let wanted: Vec<&str> = words(&filter).collect();
    (0..artists.len())
        .filter(|&i| matches(&wanted, &artists[i].name.to_lowercase()))
        .collect()
}

/// Whether every word of `wanted` starts a word of `text`.
fn matches(wanted: &[&str], text: &str) -> bool {
    wanted
        .iter()
        .all(|word| words(text).any(|other| other.starts_with(word)))
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

/// The artist an album sorts by: its one artist field, or the first of its
/// artists.
fn sort_artist(album: &AlbumID3) -> &str {
    album
        .artist
        .as_deref()
        .or_else(|| album.artists.first().map(|artist| artist.name.as_str()))
        .unwrap_or_default()
}

fn album(ui: &AppWindow, browser: &Shared, id: String) {
    load(
        ui,
        browser,
        |client| async move { client.get_album(&id).await },
        |ui, browser, album| {
            let songs = album.song;
            let album = album.album;
            ui.set_header_title(album.name.as_str().into());
            ui.set_header_names(names(album_names(&album)));
            ui.set_header_facts(album_facts(&album).into());
            header_image(ui, browser, album.cover_art);
            header_motion(ui, browser, album.id);

            let mut b = browser.borrow_mut();
            // As Roon draws a classical album: consecutive tracks of one work
            // are headed by a row for the work, the years it was written, its
            // composers, the performers all its movements share, and when and
            // where they were recorded, with the movements under it; a work's
            // only track is such a row itself.
            let runs = songs.chunk_by(|one, next| {
                !one.works.is_empty() && one.works.first() == next.works.first()
            });
            for run in runs {
                let parts = run.len() > 1;
                let length: i64 = run.iter().filter_map(|song| song.duration).sum();
                let length = if run.iter().all(|song| song.duration.is_some()) {
                    duration(length)
                } else {
                    String::new()
                };
                let performers: Vec<Vec<Name>> = run.iter().map(song_names).collect();
                let shared: Vec<Name> = performers[0]
                    .iter()
                    .filter(|name| performers.iter().all(|them| them.contains(name)))
                    .cloned()
                    .collect();
                let performed = performed(run);
                for (i, song) in run.iter().enumerate() {
                    let alone = !parts && !song.works.is_empty();
                    let own = performers[i].clone();
                    let (title, composers, credits) = match (parts, alone) {
                        (true, _) if own.iter().all(|name| shared.contains(name)) => {
                            (track_title(song), Vec::new(), Vec::new())
                        }
                        (true, _) => (track_title(song), Vec::new(), own),
                        (false, true) => (song.title.clone(), composer_names(song), own),
                        (false, false) => (song.title.clone(), Vec::new(), own),
                    };
                    let (years, performed_alone) = if alone {
                        (composed(song), performed.as_str())
                    } else {
                        (String::new(), "")
                    };
                    let (work, work_composers, work_performers, work_duration) =
                        match song.works.first() {
                            Some(work) if parts && i == 0 => (
                                work.name.as_str(),
                                composer_names(song),
                                shared.clone(),
                                length.as_str(),
                            ),
                            _ => ("", Vec::new(), Vec::new(), ""),
                        };
                    let (work_years, work_performed) = if work.is_empty() {
                        (String::new(), "")
                    } else {
                        (composed(song), performed.as_str())
                    };
                    b.tracks.push(Track {
                        title: title.into(),
                        split: composers.len() as i32,
                        names: names([composers, credits].concat()),
                        duration: song.duration.map(duration).unwrap_or_default().into(),
                        loved: song.starred.is_some(),
                        years: years.into(),
                        performed: performed_alone.into(),
                        work: work.into(),
                        work_split: work_composers.len() as i32,
                        work_names: names([work_composers, work_performers].concat()),
                        work_duration: work_duration.into(),
                        work_years: work_years.into(),
                        work_performed: work_performed.into(),
                        part: parts,
                        alone,
                    });
                }
            }
            b.songs = songs
                .into_iter()
                .map(|song| (song.id, song.music_brainz_id))
                .collect();
        },
    );
}

fn artist(ui: &AppWindow, browser: &Shared, id: String) {
    load(
        ui,
        browser,
        |client| async move { client.get_artist(&id).await },
        |ui, browser, artist| {
            ui.set_header_title(artist.artist.name.as_str().into());
            ui.set_header_facts(count(artist.album.len(), "album", "albums").into());
            header_image(ui, browser, artist.artist.cover_art);
            let tiles = artist.album.iter().map(|album| {
                let year = album.year.map(|year| unlinked(year.to_string()));
                album_tile(album, year.into_iter().collect())
            });
            add_tiles(ui, browser, tiles.collect());
        },
    );
}

/// An album's tile, named by `credits`, and its cover's id.
fn album_tile(album: &AlbumID3, credits: Vec<Name>) -> (AlbumTile, Option<String>) {
    let tile = AlbumTile {
        title: album.name.as_str().into(),
        names: names(credits),
        link: format!("album:{}", album.id).into(),
        ..AlbumTile::default()
    };
    (tile, album.cover_art.clone())
}

/// An artist's tile, with how many albums they have, and their picture's id.
fn artist_tile(artist: &ArtistID3) -> (AlbumTile, Option<String>) {
    let albums = artist
        .album_count
        .and_then(|n| usize::try_from(n).ok())
        .map(|n| unlinked(count(n, "album", "albums")));
    let tile = AlbumTile {
        title: artist.name.as_str().into(),
        names: names(albums.into_iter().collect()),
        link: format!("artist:{}", artist.id).into(),
        ..AlbumTile::default()
    };
    (tile, artist.cover_art.clone())
}

/// Adds the tiles, each with its cover when it has been seen this session,
/// and fetches the others.
fn add_tiles(ui: &AppWindow, browser: &Shared, tiles: Vec<(AlbumTile, Option<String>)>) {
    let size = pixels(ui, TILE_SIZE);
    {
        let mut b = browser.borrow_mut();
        for (mut tile, cover) in tiles {
            let seen = cover
                .as_ref()
                .and_then(|cover| b.pictures.get(&(cover.clone(), size)).cloned());
            if let (None, Some(cover)) = (&seen, cover) {
                let row = b.tiles.row_count();
                b.covers.insert(row, cover);
            }
            tile.cover = seen.unwrap_or_default();
            tile.selected = b.selected.iter().any(|link| *link == tile.link.as_str());
            b.tiles.push(tile);
        }
    }
    fetch_covers(ui, browser);
}

/// Fetches the covers of the tiles in view, top to bottom, a few at a time.
/// The others wait until they are scrolled to.
fn fetch_covers(ui: &AppWindow, browser: &Shared) {
    let size = pixels(ui, TILE_SIZE);
    let mut b = browser.borrow_mut();
    let wanted = b
        .covers
        .range(b.in_view.clone())
        .count()
        .min(COVERS_AT_ONCE);
    while b.fetching < wanted {
        b.fetching += 1;
        let (browser, tiles, visit) = (browser.clone(), b.tiles.clone(), b.visit);
        slint::spawn_local(async move {
            loop {
                let (row, cover) = {
                    let mut b = browser.borrow_mut();
                    if b.visit != visit || !Rc::ptr_eq(&b.tiles, &tiles) {
                        return;
                    }
                    let next = b.covers.range(b.in_view.clone()).next();
                    let Some(row) = next.map(|(row, _)| *row) else {
                        b.fetching -= 1;
                        return;
                    };
                    (row, b.covers.remove(&row).expect("the row was just found"))
                };
                let Some(image) = cover_art(&browser, visit, cover, size).await else {
                    return;
                };
                if let (Some(image), Some(mut tile)) = (image, tiles.row_data(row)) {
                    tile.cover = image;
                    tiles.set_row_data(row, tile);
                }
            }
        })
        .expect("the event loop is running");
    }
}

fn header_image(ui: &AppWindow, browser: &Shared, cover: Option<String>) {
    let Some(cover) = cover else { return };
    let size = pixels(ui, HEADER_SIZE);
    let visit = browser.borrow().visit;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        if let Some(Some(image)) = cover_art(&browser, visit, cover, size).await
            && let Some(ui) = weak.upgrade()
        {
            ui.set_header_image(image);
        }
    })
    .expect("the event loop is running");
}

/// Plays the album's motion cover in the header in place of its cover, on
/// servers that have them. Frames are decoded as they are shown, so only the
/// file stays in memory.
fn header_motion(ui: &AppWindow, browser: &Shared, album: String) {
    let client = browser.borrow().client.clone();
    let Some(client) = client.filter(|client| client.supports(MOTION)) else {
        return;
    };
    let size = pixels(ui, HEADER_SIZE);
    let visit = browser.borrow().visit;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let Ok(bytes) = client.get_album_motion(&album, Some(size)).await else {
            return;
        };
        let bytes: Rc<[u8]> = bytes.into();
        let b = browser.borrow();
        let (Some(mut frames), true) = (motion_frames(&bytes), b.visit == visit) else {
            return;
        };
        let Some(Ok(first)) = frames.next() else {
            return;
        };
        let interval = Duration::from(first.delay()).max(Duration::from_millis(10));
        if let Some(ui) = weak.upgrade() {
            ui.set_header_image(picture(first));
        }
        b.motion.start(TimerMode::Repeated, interval, move || {
            let next = frames.next().or_else(|| {
                frames = motion_frames(&bytes)?;
                frames.next()
            });
            if let (Some(ui), Some(Ok(frame))) = (weak.upgrade(), next) {
                ui.set_header_image(picture(frame));
            }
        });
    })
    .expect("the event loop is running");
}

fn motion_frames(bytes: &Rc<[u8]>) -> Option<Frames<'static>> {
    let decoder = WebPDecoder::new(Cursor::new(bytes.clone())).ok()?;
    Some(decoder.into_frames())
}

fn picture(frame: Frame) -> Image {
    let frame = frame.into_buffer();
    Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
        frame.as_raw(),
        frame.width(),
        frame.height(),
    ))
}

/// The cover, or `Some(None)` when there is none to show. `None` means the
/// page has been left, so there is no point fetching more. Covers are kept
/// for the session, so a page shown again has them at once.
async fn cover_art(
    browser: &Shared,
    visit: i32,
    cover: String,
    size: u32,
) -> Option<Option<Image>> {
    let key = (cover, size);
    let client = {
        let b = browser.borrow();
        if b.visit != visit {
            return None;
        }
        if let Some(image) = b.pictures.get(&key) {
            return Some(Some(image.clone()));
        }
        b.client.clone()?
    };
    let bytes = client.get_cover_art(&key.0, Some(size)).await;
    let image = bytes
        .ok()
        .and_then(|bytes| Image::load_from_data(&bytes, None).ok());
    let mut b = browser.borrow_mut();
    let signed_in = b
        .client
        .as_ref()
        .is_some_and(|current| Rc::ptr_eq(current, &client));
    if let (Some(image), true) = (&image, signed_in) {
        b.pictures.insert(key, image.clone());
    }
    (b.visit == visit).then_some(image)
}

/// Loves or unloves a track: the star on the server, and on ListenBrainz when
/// it's connected and knows the recording.
fn toggle_love(ui: &AppWindow, browser: &Shared, row: usize) {
    let b = browser.borrow();
    let (Some(client), Some((id, recording)), Some(mut track)) = (
        b.client.clone(),
        b.songs.get(row).cloned(),
        b.tracks.row_data(row),
    ) else {
        return;
    };
    let loved = !track.loved;
    track.loved = loved;
    b.tracks.set_row_data(row, track);
    let (tracks, listenbrainz, visit) = (b.tracks.clone(), b.listenbrainz.clone(), b.visit);
    drop(b);

    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let starred = if loved {
            client.star(&[&id], &[], &[]).await
        } else {
            client.unstar(&[&id], &[], &[]).await
        };
        let problem = match (starred, listenbrainz, recording) {
            (Err(e), _, _) => {
                if let Some(mut track) = tracks.row_data(row) {
                    track.loved = !loved;
                    tracks.set_row_data(row, track);
                }
                Some(format!("Your server didn't take that: {e}"))
            }
            (Ok(()), Some(listenbrainz), Some(recording)) => {
                let score = if loved {
                    listenbrainz::Score::Love
                } else {
                    listenbrainz::Score::Clear
                };
                listenbrainz
                    .submit_feedback(&recording, score)
                    .await
                    .err()
                    .map(|e| format!("Saved on your server, but ListenBrainz didn't take it: {e}"))
            }
            (Ok(()), _, _) => None,
        };
        if let (Some(problem), Some(ui)) = (problem, weak.upgrade())
            && browser.borrow().visit == visit
        {
            ui.set_page_problem(problem.into());
        }
    })
    .expect("the event loop is running");
}

/// `logical` pixels in physical ones, which is what the server should scale to.
fn pixels(ui: &AppWindow, logical: f32) -> u32 {
    (logical * ui.window().scale_factor()).ceil() as u32
}

fn names(names: Vec<Name>) -> ModelRc<Name> {
    ModelRc::new(VecModel::from(names))
}

/// An artist that links to its page, unless the server gives it no id.
fn linked(artist: &ArtistID3) -> Name {
    let link = if artist.id.is_empty() {
        String::new()
    } else {
        format!("artist:{}", artist.id)
    };
    Name {
        label: artist.name.as_str().into(),
        link: link.into(),
    }
}

fn unlinked(label: String) -> Name {
    Name {
        label: label.into(),
        link: "".into(),
    }
}

/// The artists of an album, or its one artist field on servers without the
/// list.
fn album_names(album: &AlbumID3) -> Vec<Name> {
    named(&album.artists, &album.artist, &album.artist_id)
}

fn song_names(song: &Child) -> Vec<Name> {
    named(&song.artists, &song.artist, &song.artist_id)
}

/// The composers of a song, linked where the server names them as artists.
fn composer_names(song: &Child) -> Vec<Name> {
    let composers: Vec<Name> = song
        .contributors
        .iter()
        .filter(|contributor| contributor.role == "composer")
        .map(|contributor| linked(&contributor.artist))
        .collect();
    if composers.is_empty() {
        song.display_composer
            .iter()
            .cloned()
            .map(unlinked)
            .collect()
    } else {
        composers
    }
}

/// A track's title, or within a work its movement: the movement's name where
/// the server sends one, else the title without the work's name in front.
fn track_title(song: &Child) -> String {
    if let Some(movement) = song.movements.first() {
        return movement.name.clone();
    }
    song.works
        .first()
        .and_then(|work| song.title.strip_prefix(work.name.as_str()))
        .and_then(|rest| rest.strip_prefix(": "))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(&song.title)
        .to_owned()
}

/// "1804 – 1805", the years a song's work was written.
fn composed(song: &Child) -> String {
    let span = song.works.first().and_then(|work| work.composed.as_ref());
    let year = |date: &Option<ItemDate>| date.as_ref().and_then(|date| date.year);
    match span.map(|span| (year(&span.from), year(&span.to))) {
        Some((Some(from), Some(to))) if from != to => format!("{from} – {to}"),
        Some((Some(year), _) | (None, Some(year))) => year.to_string(),
        _ => String::new(),
    }
}

/// "Performed 30 Mar 2006 – 31 Oct 2006, Teldex Studio Berlin": from the
/// first day any of `songs` was recorded to the last, and every place.
fn performed(songs: &[Child]) -> String {
    let recorded: Vec<_> = songs
        .iter()
        .filter_map(|song| song.recorded.as_ref())
        .collect();
    let order = |date: &&ItemDate| (date.year, date.month, date.day);
    let from = recorded
        .iter()
        .filter_map(|r| r.from.as_ref().filter(|date| date.year.is_some()))
        .min_by_key(order);
    let to = recorded
        .iter()
        .filter_map(|r| r.to.as_ref().filter(|date| date.year.is_some()))
        .max_by_key(order);
    let when = match (from, to) {
        (Some(from), Some(to)) if from != to => format!("{} – {}", date(from), date(to)),
        (Some(day), _) | (None, Some(day)) => date(day),
        (None, None) => String::new(),
    };
    let mut places: Vec<&str> = Vec::new();
    for place in recorded.iter().flat_map(|r| &r.places) {
        if !places.contains(&place.as_str()) {
            places.push(place);
        }
    }
    let said: Vec<String> = [when, places.join(", ")]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    if said.is_empty() {
        String::new()
    } else {
        format!("Performed {}", said.join(", "))
    }
}

/// "30 Mar 2006", "Mar 2006" or "2006", as far as it is known.
fn date(date: &ItemDate) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = date
        .month
        .and_then(|month| MONTHS.get(usize::try_from(month).ok()?.checked_sub(1)?));
    let day = date.day.filter(|_| month.is_some());
    [
        day.map(|day| day.to_string()),
        month.map(|month| month.to_string()),
        date.year.map(|year| year.to_string()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

fn named(artists: &[ArtistID3], artist: &Option<String>, id: &Option<String>) -> Vec<Name> {
    if !artists.is_empty() {
        return artists.iter().map(linked).collect();
    }
    match (artist, id) {
        (Some(name), Some(id)) => vec![Name {
            label: name.as_str().into(),
            link: format!("artist:{id}").into(),
        }],
        (Some(name), None) => vec![unlinked(name.clone())],
        _ => Vec::new(),
    }
}

/// "2021 · Folk · 9 tracks, 41 minutes", leaving out what the server doesn't
/// know.
fn album_facts(album: &AlbumID3) -> String {
    let genres: Vec<&str> = album.genres.iter().map(|g| g.name.as_str()).collect();
    let genres = if genres.is_empty() {
        album.genre.clone().unwrap_or_default()
    } else {
        genres.join(", ")
    };
    // At least a minute, so a short single doesn't say 0.
    let minutes = match album.duration {
        0 => 0,
        seconds => ((seconds + 30) / 60).max(1),
    };
    let length = format!(
        "{}, {}",
        count(album.song_count as usize, "track", "tracks"),
        count(minutes as usize, "minute", "minutes"),
    );
    [
        album.year.map(|y| y.to_string()).unwrap_or_default(),
        genres,
        length,
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "4:05", or "1:02:05" from an hour on.
fn duration(seconds: i64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_counts() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(245), "4:05");
        assert_eq!(duration(3725), "1:02:05");
        assert_eq!(count(1, "track", "tracks"), "1 track");
        assert_eq!(count(9, "track", "tracks"), "9 tracks");
    }

    #[test]
    fn performances() {
        let day = |year, month, day| ItemDate {
            year: Some(year),
            month,
            day,
        };
        assert_eq!(date(&day(2006, Some(3), Some(30))), "30 Mar 2006");
        assert_eq!(date(&day(1992, Some(11), None)), "Nov 1992");
        assert_eq!(date(&day(1992, Some(13), Some(2))), "1992");
        assert_eq!(date(&day(1817, None, None)), "1817");

        let song = |json: &str| -> Child {
            serde_json::from_str(&format!(r#"{{"id":"s","isDir":false,"title":"t",{json}}}"#))
                .unwrap()
        };
        let symphony = [
            song(
                r#""recorded":{"from":{"year":1992,"month":11},"to":{"year":1992,"month":11},"places":["Concertgebouw"]}"#,
            ),
            song(r#""genre":"Classical""#),
            song(
                r#""recorded":{"from":{"year":1992,"month":5,"day":2},"to":{"year":1992,"month":5,"day":4},"places":["Concertgebouw","Amsterdam"]}"#,
            ),
        ];
        assert_eq!(
            performed(&symphony),
            "Performed 2 May 1992 – Nov 1992, Concertgebouw, Amsterdam"
        );
        assert_eq!(
            performed(&symphony[..1]),
            "Performed Nov 1992, Concertgebouw"
        );
        assert_eq!(performed(&symphony[1..2]), "");
        assert_eq!(
            performed(&[song(r#""recorded":{"places":["Apollo Theater"]}"#)]),
            "Performed Apollo Theater"
        );
        assert_eq!(
            performed(&[song(
                r#""recorded":{"from":{"month":3},"to":{"year":1992,"month":5}}"#
            )]),
            "Performed May 1992"
        );

        let written = |span: &str| composed(&song(&format!(r#""works":[{{"name":"w"{span}}}]"#)));
        assert_eq!(
            written(r#","composed":{"from":{"year":1804},"to":{"year":1805,"month":2}}"#),
            "1804 – 1805"
        );
        assert_eq!(
            written(
                r#","composed":{"from":{"year":1817,"month":11},"to":{"year":1817,"month":11}}"#
            ),
            "1817"
        );
        assert_eq!(written(r#","composed":{"to":{"year":1955}}"#), "1955");
        assert_eq!(written(""), "");
    }
}
