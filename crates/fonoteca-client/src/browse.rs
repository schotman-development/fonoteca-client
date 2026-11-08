//! The library pages after sign-in: what each page shows, where its links lead,
//! and the history behind back and forward. A link is a page's address, such
//! as "home", "albums", "artists", "tracks", "playlists", "album:<id>",
//! "artist:<id>", "playlist:<id>" or "song:<id>", ListenBrainz's playlist
//! "createdfor:<id>" and the listening history "history", the files'
//! "folders" and
//! "folder:<id>", on servers with playlist folders "playlists:<id>", on
//! servers with Fonoteca's compositions "composers", "compositions",
//! "composer:<id>" or "composition:<id>", on servers with Fonoteca's tags
//! "tags" and "tag:<id>", "genres", "genre:<name>" and a genre's albums or
//! artists "albums:<genre>" or "artists:<genre>", "radio", "search",
//! "account", "settings" and its sections "setting:<name>".

use std::cell::RefCell;
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frame, Frames};
#[cfg(not(target_family = "wasm"))]
use opensubsonic::AlbumID3WithSongs;
use opensubsonic::{
    AlbumID3, AlbumListType, Answers, ArtistID3, CatalogueStation, Child, Client, Composer,
    Composition, Directory, ExplicitStatus, GenericMediaType, Genre, Indexes, InternetRadioStation,
    ItemDate, ListOptions, MediaType, Movement, Playlist, PlaylistFolder, PlaylistFolderUpdate,
    PlaylistUpdate, PlaylistWithSongs, RadioStation, ScanStatus, SearchOptions, Span, Tag,
    TagUpdate, User,
};
use radiobrowser::Station;
use slint::{
    ComponentHandle, DataTransfer, Image, Model, ModelRc, SharedPixelBuffer, SharedString, Timer,
    TimerMode, VecModel,
};

use crate::{
    AlbumTile, AppWindow, ColumnChoice, Detail, Drag, Icons, Name, NavEntry, Page, Stat,
    StationRow, StationShelf, TagChoice, TagRow, Track, TrackColumn, TrackLine, answers, catalogue,
    logo_ink, offline, prefs, zone,
};

pub(crate) mod created_for;
mod listens;
mod played;
mod search;
mod stats;
#[cfg(test)]
pub use stats::TEST_ROOTS as LISTENBRAINZ_ROOTS;
pub(crate) mod matching;

/// The newest albums on the home page, and the albums played last.
const RECENT: usize = 24;
/// The artists or albums a genre's shelves show before "See all".
const SHELF: usize = 24;
/// The radio directory's shelves on the radio page, by title and genre tag,
/// music only; the first has the stations with the most votes of all.
const GENRES: [(&str, &str); 27] = [
    ("Top worldwide", ""),
    ("Pop", "pop"),
    ("Rock", "rock"),
    ("Classical", "classical"),
    ("Jazz", "jazz"),
    ("Dance", "dance"),
    ("Electronic", "electronic"),
    ("Hip hop", "hip hop"),
    ("R&B", "rnb"),
    ("Soul", "soul"),
    ("Oldies", "oldies"),
    ("80s", "80s"),
    ("90s", "90s"),
    ("Classic rock", "classic rock"),
    ("Alternative", "alternative"),
    ("Indie", "indie"),
    ("Metal", "metal"),
    ("Country", "country"),
    ("Folk", "folk"),
    ("Blues", "blues"),
    ("Reggae", "reggae"),
    ("Latin", "latin"),
    ("World", "world music"),
    ("Ambient", "ambient"),
    ("Chillout", "chillout"),
    ("Lounge", "lounge"),
    ("Christian", "christian"),
];
/// The shelf of the country the device is in, after the genres'.
const LOCAL: usize = GENRES.len();
/// The genre page's shelf of stations, after the radio page's.
const GENRE_RADIO: usize = LOCAL + 1;
/// How many stations a shelf shows, and a genre's page asks for at a time;
/// how many of each language its first page has.
const SHELF_STATIONS: u32 = 12;
const GENRE_STATIONS: u32 = 60;
const GENRE_IN_LANGUAGE: u32 = 100;
/// How the app names itself to the radio directory.
pub(crate) const USER_AGENT: &str = concat!("fonoteca-client/", env!("CARGO_PKG_VERSION"));
/// Tiles added to the albums or artists page at a time.
const TILES_AT_ONCE: usize = 100;
/// The entries for the most played in the albums page's and the artists
/// page's Sort menus, in app.slint. The composers page has the artists' menu.
const MOST_PLAYED_ALBUMS: i32 = 4;
const MOST_PLAYED_ARTISTS: i32 = 1;
/// Albums asked for at a time while the library loads, and at most how many,
/// against servers that ignore the offset.
const LIBRARY_AT_ONCE: u32 = 500;
const LIBRARY_AT_MOST: usize = 50_000;
/// At most how many songs the tracks page asks for, against servers that
/// ignore the offset.
// ponytail: every song stays in memory; page from the server if libraries outgrow this.
const SONGS_AT_MOST: usize = 200_000;
/// How long typing in the filter pauses before the albums are filtered.
const TYPING_PAUSE: Duration = Duration::from_millis(200);
/// How long a notice, such as songs added to a playlist, stays up.
const NOTICE_TIME: Duration = Duration::from_secs(3);
/// How often the pages' answers are saved when they changed. The browser
/// keeps them in memory only.
#[cfg(not(any(test, target_family = "wasm")))]
const SAVE_EVERY: Duration = Duration::from_secs(30);
#[cfg(test)]
const SAVE_EVERY: Duration = Duration::from_millis(200);
/// How long the radio page waits after a language is ticked before it asks
/// for the shelves again, so ticking several asks once.
const LANGUAGE_WAIT: Duration = Duration::from_millis(600);
/// How often the account page asks again while the server scans.
const SCAN_POLL: Duration = Duration::from_secs(2);
/// Tile covers fetched at the same time.
const COVERS_AT_ONCE: usize = 6;
/// The sizes covers are asked for, in logical pixels. Grid tiles are at least
/// 200 wide and widen to fill their row, which keeps them under 300 on all
/// but the narrowest windows, where the grid keeps two columns and a tile is
/// under half the window wide (see `tile_pixels`); page headers are 220.
const TILE_SIZE: f32 = 300.0;
/// The widest a genre's tile gets: two to a row across the page's most width,
/// 1600 points, less its padding and the gap.
const GENRE_SIZE: f32 = 800.0;
/// The widest a genre page's banner gets, the page's own most width.
const BANNER_SIZE: f32 = 1600.0;
const HEADER_SIZE: f32 = 220.0;
/// The size of a cover in a row of the tracks table.
const LINE_COVER_SIZE: f32 = 42.0;
/// Fonoteca's extension for the curated radio catalogue.
const RADIO_CATALOGUE: &str = "fonotecaRadioCatalogue";
/// Fonoteca's extension for motion covers.
const MOTION: &str = "fonotecaAlbumMotion";
/// Fonoteca's extension for composers and compositions.
const COMPOSITIONS: &str = "fonotecaCompositions";
/// The version of that extension whose composers and compositions carry
/// their plays.
const COMPOSITION_PLAYS: i64 = 2;
/// OpenSubsonic's extension for an artist's top songs by id, not name.
const TOP_SONGS_BY_ID: &str = "topSongsByArtistId";
/// The proposed OpenSubsonic extension for folders of playlists.
const FOLDERS: &str = "playlistFolders";
/// Fonoteca's extension for the user's own tags.
const TAGS: &str = "fonotecaTags";
/// Fonoteca's extension for the genres each genre sits under.
const GENRE_TREE: &str = "fonotecaGenres";
/// The version of that extension whose genres carry descriptions.
const GENRE_DESCRIPTIONS: i64 = 2;
/// Fonoteca's extension for artists' banners.
const ARTIST_BANNER: &str = "fonotecaArtistBanner";
/// How many of an artist's top songs their page shows.
const TOP_SONGS: u32 = 5;
/// About how long the start of a biography or notes is, before More.
const ABOUT_SHORT: usize = 320;

/// The motion cover of the album playing, for the now playing sheet.
#[derive(Default)]
struct PlayingMotion {
    /// The album it is of, and whether it has shown a frame yet.
    album: Option<String>,
    framed: bool,
    /// Shows the next frame, and runs only while `moving`.
    timer: Timer,
    moving: bool,
}

#[derive(Default)]
pub struct Browser {
    client: Option<Arc<Client>>,
    /// The answers the pages read, kept for the account signed in, and what
    /// saves them on this device as they change.
    answers: Option<(String, Arc<Answers>)>,
    /// What ListenBrainz answered, kept for the same account and saved with
    /// them.
    listenbrainz_answers: Option<Arc<Answers>>,
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    saving: Timer,
    history: Vec<String>,
    at: usize,
    /// Counts the pages shown, so an answer for a page left behind is dropped.
    visit: i32,
    tiles: Rc<VecModel<AlbumTile>>,
    tracks: Rc<VecModel<Track>>,
    /// The song of each track row.
    songs: Vec<Child>,
    created_for: created_for::State,
    search: search::State,
    /// The library's songs found for ListenBrainz's tracks this session.
    found: matching::Found,
    loves: crate::loves::State,
    /// The album or playlist on screen, when one is, for downloading it.
    album: Option<AlbumID3>,
    playlist: Option<Playlist>,
    /// What is kept on this device for use without a connection, whether the
    /// sidebar lists it, and whether the server didn't answer at start, so
    /// the pages show what is kept without asking it.
    downloads: offline::Downloads,
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    downloads_listed: bool,
    offline: bool,
    /// The links of an artist's own albums, in the order their page shows.
    own_albums: Vec<String>,
    /// Tile, tracks table or recording rows whose covers are still to fetch, the rows on
    /// screen or just below it, and how many fetches run.
    covers: BTreeMap<usize, String>,
    in_view: Range<usize>,
    fetching: usize,
    /// Covers seen this session, by cover id and size.
    // ponytail: unbounded, every cover seen stays decoded; an LRU when big libraries need it.
    pictures: HashMap<(String, u32), Image>,
    /// Shows the next frame of the motion cover of the album page's header.
    motion: Timer,
    playing: PlayingMotion,
    /// Every album on the server in the order they arrived, while they arrive
    /// and once they all have.
    library: Vec<AlbumID3>,
    library_loading: bool,
    library_loaded: bool,
    /// How many tracks the server's last finished scan counted, once it
    /// told; and whether it has answered, or failed to, since sign-in.
    scanned_tracks: Option<usize>,
    scanned_tracks_asked: bool,
    /// Whether the server was told of a play since the library's play counts
    /// were asked for.
    library_misses_plays: bool,
    /// Every artist on the server, once they have arrived.
    artists: Vec<ArtistID3>,
    artists_loaded: bool,
    /// Every composer and every composition, on servers with Fonoteca's
    /// compositions, once they have arrived.
    composers: Vec<Composer>,
    composers_loaded: bool,
    compositions: Vec<Composition>,
    compositions_loaded: bool,
    /// Whether the server was told of a play since the composers' and the
    /// compositions' play counts were asked for.
    composers_miss_plays: bool,
    compositions_miss_plays: bool,
    /// Every genre, once they have arrived, and on a genre's page the tiles
    /// of its subgenres and of its artists.
    genres: Vec<Genre>,
    genres_loaded: bool,
    genre_tiles: Rc<VecModel<AlbumTile>>,
    genre_artists: Rc<VecModel<AlbumTile>>,
    /// The tiles of the genre page's stations, whose stations are in
    /// `shelves` under `GENRE_RADIO`.
    genre_radio: Rc<VecModel<AlbumTile>>,
    /// The pictures in the genres' shared tint, for the session.
    tinted: HashMap<(String, u32), Image>,
    /// The playlists, for the sidebar and the playlists page, asked for on
    /// sign-in and on every visit to that page, and their folders on
    /// servers with them.
    playlists: Vec<Playlist>,
    playlists_loading: bool,
    playlists_loaded: bool,
    playlist_folders: Vec<PlaylistFolder>,
    /// The user's tags, on servers with Fonoteca's, asked for on sign-in, on
    /// every visit to the tags page and after every change to them.
    tags: Vec<Tag>,
    tags_loaded: bool,
    /// The songs the tags dialog is for, each with the tags on it, once
    /// known; how often it was opened, so songs looked up for an earlier
    /// one are dropped.
    tagging: Vec<(String, Vec<String>)>,
    tagging_round: u32,
    /// Every song on the server in the order they arrived, while they arrive
    /// and once they all have; the tracks table's order of them, as indexes,
    /// and the rows shown.
    song_list: Vec<Child>,
    song_list_loading: bool,
    song_list_loaded: bool,
    /// Whether the server was told of a play since the songs' play counts
    /// were asked for.
    song_list_misses_plays: bool,
    song_view: Vec<usize>,
    lines: Rc<VecModel<TrackLine>>,
    /// The folder the folders page shows, by its link, its name, and what is
    /// in it: folders first, then files, as the server sends them. Their
    /// order on the page is `view`.
    folder: String,
    folder_name: String,
    entries: Vec<Child>,
    /// What the tracks table shows of `COLUMNS` and the compositions table
    /// of `PIECE_COLUMNS`, and how they are sorted.
    track_table: Table,
    piece_table: Table,
    /// The same for the folders' tables, of `COLUMNS` too.
    entry_table: Table,
    /// The filter of the albums or artists page on screen, and what it
    /// shows, as indexes into `library` or `artists` in the page's order.
    filter: String,
    view: Vec<usize>,
    /// Filters the albums or artists, or searches, once typing pauses.
    typing: Timer,
    /// The links of the albums selected, in the order they were picked. They
    /// stay selected through sorting, filtering and other pages.
    selected: Vec<String>,
    /// The link last picked or let go on the page on screen, where a range
    /// picked with Shift starts.
    anchor: Option<String>,
    /// Takes the notice down.
    notice: Timer,
    /// Asks how the server's scan is going while the settings page is up.
    scan: Timer,
    /// The account signed in, once the server has said what it may do.
    me: Option<User>,
    /// The stations saved on the server as last asked, and the home page's
    /// shelf of them.
    server_stations: Vec<InternetRadioStation>,
    saved: Rc<VecModel<AlbumTile>>,
    /// The user's most played on ListenBrainz, for the home page.
    stats: stats::Stats,
    /// What the server lists as played last, for the home page.
    played: played::Played,
    /// The radio directory once asked, its countries' names by code, its
    /// languages, each shelf's stations as they came, the shelves shown, the
    /// shelves asked for, each genre's place on the page, the shelves' tiles
    /// whose pictures were asked for, and how often the languages changed.
    directory: Option<Rc<radiobrowser::Client>>,
    countries: HashMap<String, String>,
    language_names: Vec<String>,
    shelves: HashMap<usize, Vec<Station>>,
    shelf_model: Rc<VecModel<StationShelf>>,
    shelves_asked: HashSet<usize>,
    shelf_ranks: Vec<usize>,
    logos_asked: HashSet<(usize, usize)>,
    shelf_round: u32,
    language_wait: Timer,
    /// The device's country, and the languages whose stations the genres
    /// show first, in the order picked.
    local: Option<String>,
    languages: Vec<String>,
    /// The genre page's id, its stations so far, how far into the worldwide
    /// ones it is, whether the directory has more, and whether more are on
    /// their way.
    genre: String,
    genre_stations: Vec<Station>,
    genre_offset: u32,
    genre_more: bool,
    genre_loading: bool,
    /// The server's curated stations once asked, whether they were, how many
    /// albums the library has of each genre (by `genre_key`), the shelves
    /// they are in and shown as, and the shelves' tiles whose pictures were
    /// asked for.
    catalogue: Vec<CatalogueStation>,
    catalogue_asked: bool,
    catalogue_albums: HashMap<String, i64>,
    catalogue_shelves: Vec<catalogue::Shelf>,
    catalogue_model: Rc<VecModel<StationShelf>>,
    catalogue_logos_asked: HashSet<(usize, usize)>,
    /// The pictures of saved stations by stream address, where the
    /// directory has them.
    favicons: HashMap<String, Option<String>>,
    /// The same pictures as this device keeps them, read when first needed.
    station_logos: Option<HashMap<String, prefs::Logo>>,
}

pub type Shared = Rc<RefCell<Browser>>;

/// Opens the home page with a signed-in client.
pub fn start(ui: &AppWindow, browser: &Shared, client: Client) {
    open_library(ui, browser, client);
    ask_signed_in(ui, browser);
}

/// Opens the home page with `client`, which may still be signing in: the
/// pages show what this device kept at once, and ask the server for the
/// rest once it has. [`signed_in`] then goes on from there.
pub fn open_library(ui: &AppWindow, browser: &Shared, client: Client) {
    {
        let mut b = browser.borrow_mut();
        let client = keep_answers(&mut b, client);
        b.client = Some(Arc::new(client));
        b.history.clear();
        b.offline = false;
        b.downloads.set_offline(false);
    }
    load_prefs(ui, browser);
    ui.set_album_filter("".into());
    ui.set_artist_filter("".into());
    ui.set_composer_filter("".into());
    ui.set_composition_filter("".into());
    ui.set_track_filter("".into());
    ui.set_folder_filter("".into());
    ui.set_search_query("".into());
    ui.set_roles("".into());
    ui.set_may_administer(true);
    ui.set_may_download(true);
    sidebar(ui, browser);
    load_playlists(ui, browser);
    open(ui, browser, "home".into());
}

/// Goes on once the client the library opened with has signed in: shows
/// what the server's extensions offer, the playlist folders among them, and
/// asks for what a signed-in library needs besides its pages.
pub fn signed_in(ui: &AppWindow, browser: &Shared) {
    sidebar(ui, browser);
    load_playlists(ui, browser);
    if ui.get_page() == Page::Home && offers_compositions(&browser.borrow()) {
        show_home(ui, browser);
        composers(ui, browser);
        compositions(ui, browser);
    }
    ask_signed_in(ui, browser);
}

/// Goes on without the server, which didn't answer the sign-in: the pages
/// show what is kept on this device, from the downloads page on, until
/// `reconnect`.
pub fn go_offline(ui: &AppWindow, browser: &Shared) {
    {
        let mut b = browser.borrow_mut();
        b.offline = true;
        b.downloads.set_offline(true);
    }
    open(ui, browser, "downloads".into());
}

/// Goes on with `client`, signed in at last after the server didn't answer
/// the sign-in at start, on the page on screen, asked for anew.
pub fn reconnect(ui: &AppWindow, browser: &Shared, client: Client) {
    {
        let mut b = browser.borrow_mut();
        let client = keep_answers(&mut b, client);
        b.client = Some(Arc::new(client));
        b.offline = false;
        b.downloads.set_offline(false);
    }
    ui.set_roles("".into());
    ui.set_may_administer(true);
    ui.set_may_download(true);
    // The server's extensions are known now.
    sidebar(ui, browser);
    load_playlists(ui, browser);
    ask_signed_in(ui, browser);
    refresh(ui, browser);
}

/// Asks for what a signed-in library needs besides its pages: the tags,
/// the user's roles, the hearts, the queue left playing and the downloads
/// left to do.
fn ask_signed_in(ui: &AppWindow, browser: &Shared) {
    load_tags(ui, browser);
    load_me(ui, browser);
    crate::loves::sync(ui, browser);
    ui.invoke_library_started();
    #[cfg(not(target_family = "wasm"))]
    crate::downloads::resume(ui, browser);
}

/// Whether the pages show what is kept on this device instead of asking
/// the server, which didn't answer at start.
pub fn is_offline(browser: &Shared) -> bool {
    browser.borrow().offline
}

/// Whether the server didn't answer the sign-in the library opened with:
/// its pages ask the server nothing until [`reconnect`].
pub fn sign_in_failed(browser: &Shared) -> bool {
    let b = browser.borrow();
    (b.client.as_ref()).is_some_and(|client| client.sign_in_failed())
}

/// What `call` asks the server for, or when it can't be reached, or
/// while offline, what `kept` has.
pub async fn saved_or<T>(
    offline: bool,
    kept: impl Fn() -> Option<T>,
    call: impl Future<Output = opensubsonic::Result<T>>,
) -> opensubsonic::Result<T> {
    if offline && let Some(saved) = kept() {
        return Ok(saved);
    }
    match call.await {
        Err(opensubsonic::Error::Http(e)) => kept().ok_or(opensubsonic::Error::Http(e)),
        answer => answer,
    }
}

/// `songs` without those not kept on this device, while the server isn't
/// there to stream them.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub fn available(browser: &Shared, songs: Vec<Child>) -> Vec<Child> {
    let b = browser.borrow();
    if !b.offline {
        return songs;
    }
    let kept = songs.into_iter();
    kept.filter(|song| b.downloads.local(&song.id).is_some())
        .collect()
}

/// Reads what is kept on this device, once at start.
#[cfg(not(target_family = "wasm"))]
pub fn load_downloads(browser: &Shared) {
    browser.borrow_mut().downloads = offline::Downloads::load();
}

/// What is kept on this device.
pub fn downloads(browser: &Shared) -> offline::Downloads {
    browser.borrow().downloads.clone()
}

/// The link of the page on screen.
#[cfg(not(target_family = "wasm"))]
pub fn link(browser: &Shared) -> Option<String> {
    let b = browser.borrow();
    b.history.get(b.at).cloned()
}

/// The album or playlist on screen, as something to keep.
#[cfg(not(target_family = "wasm"))]
pub fn shown_item(browser: &Shared) -> Option<offline::Item> {
    let b = browser.borrow();
    if let Some(album) = b.album.clone() {
        return Some(offline::Item::Album(AlbumID3WithSongs {
            album,
            song: b.songs.clone(),
        }));
    }
    let playlist = b.playlist.clone()?;
    Some(offline::Item::Playlist(PlaylistWithSongs {
        playlist,
        entry: b.songs.clone(),
    }))
}

/// The song of the `row`th track row of the album or playlist on screen.
#[cfg(not(target_family = "wasm"))]
pub fn song_at(browser: &Shared, row: usize) -> Option<Child> {
    browser.borrow().songs.get(row).cloned()
}

/// The downloads page: the albums, playlists and folders kept as tiles, the
/// songs kept on their own as rows.
fn downloads_page(ui: &AppWindow, browser: &Shared) {
    ui.set_header_title("Downloads".into());
    let mut tiles = Vec::new();
    let mut songs = Vec::new();
    for item in browser.borrow().downloads.items() {
        match item {
            offline::Item::Album(album) => {
                tiles.push(album_tile(&album.album, album_names(&album.album)))
            }
            offline::Item::Playlist(playlist) => tiles.push(playlist_tile(&playlist.playlist)),
            offline::Item::Song(song) => songs.push(*song),
            offline::Item::Folder(folder) => tiles.push(music_folder_tile(&folder)),
        }
    }
    let rows: Vec<Track> = songs.iter().map(song_track).collect();
    show_songs(&mut browser.borrow_mut(), songs, rows);
    add_tiles(ui, browser, tiles);
    show_downloads(ui, browser);
}

/// Shows what was added to or removed from the downloads: the sidebar's
/// entry, the downloads page, or else the marks on the page on screen.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub fn downloads_changed(ui: &AppWindow, browser: &Shared) {
    let (on_page, any, listed) = {
        let b = browser.borrow();
        let on_page = b.history.get(b.at).is_some_and(|link| link == "downloads");
        (on_page, b.downloads.any(), b.downloads_listed)
    };
    if any != listed {
        browser.borrow_mut().downloads_listed = any;
        sidebar(ui, browser);
    }
    if on_page {
        show(ui, browser);
    } else {
        show_downloads(ui, browser);
    }
}

/// Shows how much of the page on screen is kept, on its header and on the
/// rows of its songs and folders.
pub fn show_downloads(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    let progress = b
        .history
        .get(b.at)
        .and_then(|link| b.downloads.progress_of(link));
    let (state, text) = match progress {
        Some((done, total)) if done == total => (DOWNLOADED, String::new()),
        Some((done, total)) => (DOWNLOADING, format!("{done} of {total}")),
        None => (NOT_DOWNLOADED, String::new()),
    };
    ui.set_download_state(state);
    ui.set_download_progress(text.into());
    let summary = b.downloads.summary();
    ui.set_download_songs(summary.songs as i32);
    ui.set_download_space(summary.space().into());
    ui.set_download_status(summary.status().into());
    ui.set_sign_out_frees(summary.frees().into());
    for (row, song) in b.songs.iter().enumerate() {
        let downloaded = b.downloads.local(&song.id).is_some();
        let alone = downloaded && b.downloads.is_kept_alone(&song.id);
        if let Some(mut track) = b.tracks.row_data(row)
            && (track.downloaded, track.downloaded_alone) != (downloaded, alone)
        {
            track.downloaded = downloaded;
            track.downloaded_alone = alone;
            b.tracks.set_row_data(row, track);
        }
    }
    for row in 0..b.tiles.row_count() {
        let Some(mut tile) = b.tiles.row_data(row) else {
            continue;
        };
        let kept = b.downloads.is_kept(&tile.link);
        if tile.downloaded != kept {
            tile.downloaded = kept;
            b.tiles.set_row_data(row, tile);
        }
    }
    for row in 0..b.lines.row_count() {
        let Some(mut line) = b.lines.row_data(row).filter(|line| line.folder) else {
            continue;
        };
        let kept = b.downloads.is_kept(&line.link);
        if line.downloaded != kept {
            line.downloaded = kept;
            b.lines.set_row_data(row, line);
        }
    }
}

/// The values of the header's download button state.
const NOT_DOWNLOADED: i32 = 0;
const DOWNLOADING: i32 = 1;
const DOWNLOADED: i32 = 2;

/// The links of the albums of the artist on screen that are their own.
pub fn own_albums(browser: &Shared) -> Vec<String> {
    browser.borrow().own_albums.clone()
}

/// The client signed in with, for calls made outside the pages.
pub fn client(browser: &Shared) -> Option<Arc<Client>> {
    browser.borrow().client.clone()
}

/// Notes that the server was told of a play, which the play counts of the
/// library, the songs, the composers and the compositions asked for before
/// miss: the next page sorted by them asks for them again.
#[cfg(not(target_family = "wasm"))]
pub fn told_play(browser: &Shared) {
    let mut b = browser.borrow_mut();
    b.library_misses_plays = true;
    b.song_list_misses_plays = true;
    b.composers_miss_plays = true;
    b.compositions_miss_plays = true;
}

/// The songs on screen in their rows' order, and where the `row`th row is
/// among them: an album's, a playlist's (ListenBrainz's without the songs the
/// library lacks) or an artist's top songs, or the
/// songs of the tracks, folders or a tag's page as filtered and sorted,
/// without the folders between them.
pub fn songs(browser: &Shared, page: Page, row: usize) -> (Vec<Child>, usize) {
    let b = browser.borrow();
    if !matches!(page, Page::Tracks | Page::Folders | Page::Tag) {
        let is_playable = |song: &&Child| !created_for::is_missing(song);
        let before = b.songs.iter().take(row).filter(is_playable).count();
        return (
            b.songs.iter().filter(is_playable).cloned().collect(),
            before,
        );
    }
    let (songs, order) = listed(&b, page);
    let rows = order.iter().map(|&song| &songs[song]);
    let start = rows.clone().take(row).filter(|song| !song.is_dir).count();
    (rows.filter(|song| !song.is_dir).cloned().collect(), start)
}

/// Fetches a cover `logical` pixels wide and hands it to `done`.
pub fn cover(
    ui: &AppWindow,
    browser: &Shared,
    cover: String,
    logical: f32,
    done: impl FnOnce(&AppWindow, Image) + 'static,
) {
    let size = pixels(ui, logical);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        if let Some(Some(image)) = cover_art(&browser, None, cover, size).await
            && let Some(ui) = weak.upgrade()
        {
            done(&ui, image);
        }
    })
    .expect("the event loop is running");
}

/// Goes on with `client` instead, signed in anew after a password change,
/// on the page on screen.
pub fn swap(browser: &Shared, client: Client) {
    let mut b = browser.borrow_mut();
    let client = keep_answers(&mut b, client);
    b.client = Some(Arc::new(client));
}

/// `client`, keeping the pages' answers for its account: read from this
/// device the first time, and saved there as they change.
fn keep_answers(b: &mut Browser, client: Client) -> Client {
    let account = client.account(&prefs::answers_salt());
    if b.answers.as_ref().is_none_or(|(kept, _)| *kept != account) {
        let answers = answers::load(&account);
        let listenbrainz = answers::load(&listenbrainz_account(&account));
        #[cfg(not(target_family = "wasm"))]
        b.saving.start(TimerMode::Repeated, SAVE_EVERY, {
            let (account, answers) = (account.clone(), answers.clone());
            let listenbrainz = listenbrainz.clone();
            move || {
                answers::save(&account, &answers);
                answers::save(&listenbrainz_account(&account), &listenbrainz);
            }
        });
        b.answers = Some((account, answers));
        b.listenbrainz_answers = Some(listenbrainz);
    }
    let (_, answers) = b.answers.as_ref().expect("the answers were just kept");
    client.keeping_answers_in(answers.clone())
}

/// What names the answers ListenBrainz gave for `account` apart from the
/// pages' own.
pub(crate) fn listenbrainz_account(account: &str) -> String {
    format!("{account}-listenbrainz")
}

/// Forgets the answers kept for the account signed in, ListenBrainz's too,
/// on this device too, before it signs out.
pub fn forget_answers(browser: &Shared) -> std::io::Result<()> {
    let b = browser.borrow();
    b.saving.stop();
    let (Some((account, answers)), Some(listenbrainz)) = (&b.answers, &b.listenbrainz_answers)
    else {
        return Ok(());
    };
    answers::forget(account, answers)?;
    answers::forget(&listenbrainz_account(account), listenbrainz)
}

/// Forgets what ListenBrainz answered for the account signed in, on this
/// device too, when its token goes.
pub fn forget_listenbrainz_answers(browser: &Shared) -> std::io::Result<()> {
    let b = browser.borrow();
    let (Some((account, _)), Some(listenbrainz)) = (&b.answers, &b.listenbrainz_answers) else {
        return Ok(());
    };
    answers::clear(&listenbrainz_account(account), listenbrainz)
}

/// Where what ListenBrainz answered is kept for the account signed in, once
/// those saved on this device are in.
pub(crate) async fn listenbrainz_answers(browser: &Shared) -> Option<Arc<Answers>> {
    let answers = browser.borrow().listenbrainz_answers.clone()?;
    Some(answers::loaded(answers).await)
}

/// Ages the listening history kept of ListenBrainz, which lacks a play the
/// server just took and passes on to it, so that the next visit asks for it.
#[cfg(not(target_family = "wasm"))]
pub fn play_taken(browser: &Shared) {
    if let Some(listenbrainz) = &browser.borrow().listenbrainz_answers {
        listenbrainz.age(listens::KEPT_UNDER);
    }
}

/// Logs `song` as played on this device for the account signed in, begun
/// at `listened_at`.
#[cfg(not(target_family = "wasm"))]
pub fn log_play(browser: &Shared, song: &Child, listened_at: i64) {
    if let Some(account) = account_key(&browser.borrow()) {
        crate::plays::add(account, crate::plays::Play::of(song, listened_at));
    }
}

/// What names the account signed in on this device, for the plays that
/// wait for its server.
#[cfg(not(target_family = "wasm"))]
pub fn account(browser: &Shared) -> Option<String> {
    account_key(&browser.borrow()).map(str::to_owned)
}

/// Forgets the plays logged on this device for the account signed in,
/// before it signs out.
pub fn forget_plays(browser: &Shared) -> std::io::Result<()> {
    match account_key(&browser.borrow()) {
        Some(account) => crate::plays::forget(account),
        None => Ok(()),
    }
}

/// What names the account signed in on this device, without saying whose
/// it is.
fn account_key(b: &Browser) -> Option<&str> {
    (b.answers.as_ref()).map(|(account, _)| account.as_str())
}

/// Whether the library is open: signed in, and not signed out since.
pub fn started(browser: &Shared) -> bool {
    let b = browser.borrow();
    b.client.is_some() && !b.offline
}

/// Forgets the client and the pages seen, on sign out.
pub fn stop(browser: &Shared) {
    let visit = browser.borrow().visit;
    let downloads = downloads(browser);
    *browser.borrow_mut() = Browser {
        visit: visit + 1,
        downloads,
        ..Browser::default()
    };
}

pub fn wire(ui: &AppWindow, browser: &Shared) {
    stats::wire(ui, browser);
    search::wire(ui, browser);
    let mut carried = DataTransfer::default();
    carried.set_plain_text("playlist".into());
    ui.global::<Drag>().set_data(carried);
    // The bottom bar's entries are the same on every server, so they are
    // there before the library opens: a bar without them is no wider than
    // the top bar's buttons, and narrows the whole screen to it.
    ui.set_bar_entries(bar_entries(&ui.global::<Icons>()));
    ui.on_open({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |link| open(&weak.unwrap(), &browser, link.into())
    });
    ui.on_show_section({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| {
            let link = usize::try_from(index).ok().and_then(|index| {
                let b = browser.borrow();
                sections_of(&b)
                    .into_iter()
                    .nth(index)
                    .map(|(link, ..)| link)
            });
            if let Some(link) = link {
                open(&weak.unwrap(), &browser, link);
            }
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
    ui.on_back_or_home({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || back_or_home(&weak.unwrap(), &browser)
    });
    ui.on_refresh({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || ask_anew(&weak.unwrap(), &browser, refresh)
    });
    ui.on_show_more({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || more_tiles(&weak.unwrap(), &browser)
    });
    ui.on_try_again({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || ask_anew(&weak.unwrap(), &browser, more_tiles)
    });
    ui.on_save_created_for({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || created_for::save(&weak.unwrap(), &browser)
    });
    ui.on_listenbrainz_changed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            if ui.get_page() == Page::History {
                show(&ui, &browser);
            }
        }
    });
    ui.on_toggle_love({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| toggle_love(&weak.unwrap(), &browser, row as usize)
    });
    ui.on_toggle_line_love({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| toggle_line_love(&weak.unwrap(), &browser, row as usize)
    });
    ui.on_toggle_track_column({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| toggle_column(&weak.unwrap(), &browser, index as usize)
    });
    let sort = {
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            save_prefs(&ui, &browser);
            show(&ui, &browser);
        }
    };
    ui.on_sort_albums(sort.clone());
    ui.on_sort_artists(sort.clone());
    ui.on_sort_composers(sort);
    ui.on_sort_tracks({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| sort_tracks(&weak.unwrap(), &browser, index as usize)
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
    ui.on_filter_artists(filter.clone());
    ui.on_filter_composers(filter.clone());
    ui.on_filter_compositions(filter.clone());
    ui.on_filter_tracks(filter.clone());
    ui.on_filter_folders(filter);
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
        move |row, range| select_tile(&weak.unwrap(), &browser, row as usize, range)
    });
    ui.on_select_line({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, range| select_line(&weak.unwrap(), &browser, row as usize, range)
    });
    ui.on_select_track({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, range| select_track(&weak.unwrap(), &browser, row as usize, range)
    });
    ui.on_select_work({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, range| select_work(&weak.unwrap(), &browser, row as usize, range)
    });
    ui.on_clear_selection({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || clear_selection(&weak.unwrap(), &browser)
    });
    ui.on_create_playlist({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name| create_playlist(&weak.unwrap(), &browser, name.trim().to_owned())
    });
    ui.on_rename_playlist({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name| rename_playlist(&weak.unwrap(), &browser, name.trim().to_owned())
    });
    ui.on_start_scan({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || start_scan(&weak.unwrap(), &browser)
    });
    ui.on_station_shelf_in_view({
        let browser = browser.clone();
        move |shelf, first, last| {
            let rows = first.max(0) as usize..(last + 1).max(0) as usize;
            shelf_logos(&browser, shelf as usize, rows);
        }
    });
    ui.on_catalogue_shelf_in_view({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |shelf, first, last| {
            let rows = first.max(0) as usize..(last + 1).max(0) as usize;
            catalogue_logos(&weak.unwrap(), &browser, shelf as usize, rows);
        }
    });
    ui.on_save_catalogue_station({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |shelf, index| {
            save_catalogue_station(&weak.unwrap(), &browser, shelf as usize, index as usize)
        }
    });
    ui.on_toggle_language({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| toggle_language(&weak.unwrap(), &browser, index as usize)
    });
    ui.on_save_station({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |shelf, index| save_station(&weak.unwrap(), &browser, shelf, index as usize)
    });
    ui.on_create_station({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name, stream, homepage| {
            let [name, stream, homepage] = [name, stream, homepage].map(|s| s.trim().to_owned());
            let done = format!("Added {name}.");
            change_stations(&weak.unwrap(), &browser, done, |client| async move {
                let station = station(&name, &stream, &homepage);
                client.create_internet_radio_station(&station).await
            });
        }
    });
    ui.on_update_station({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |id, name, stream, homepage| {
            let id = id.to_string();
            let [name, stream, homepage] = [name, stream, homepage].map(|s| s.trim().to_owned());
            let done = format!("Saved {name}.");
            change_stations(&weak.unwrap(), &browser, done, |client| async move {
                let station = station(&name, &stream, &homepage);
                client.update_internet_radio_station(&id, &station).await
            });
        }
    });
    ui.on_delete_station({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |id, name| {
            let id = id.to_string();
            let done = format!("Deleted {name}.");
            change_stations(&weak.unwrap(), &browser, done, |client| async move {
                client.delete_internet_radio_station(&id).await
            });
        }
    });
    ui.on_delete_playlist({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || delete_playlist(&weak.unwrap(), &browser)
    });
    ui.on_remove_from_playlist({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| remove_from_playlist(&weak.unwrap(), &browser, row as usize)
    });
    ui.on_add_line_to_playlist({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, id| {
            let ui = weak.unwrap();
            let song = {
                let b = browser.borrow();
                let (songs, order) = listed(&b, ui.get_page());
                order.get(row as usize).map(|&song| song_link(&songs[song]))
            };
            add_to_playlist(&ui, &browser, id.into(), song.into_iter().collect());
        }
    });
    ui.on_add_track_to_playlist({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, id| {
            let song = browser
                .borrow()
                .songs
                .get(row as usize)
                .map(|song| song.id.clone());
            let links = song
                .map(|song| format!("song:{song}"))
                .into_iter()
                .collect();
            add_to_playlist(&weak.unwrap(), &browser, id.into(), links);
        }
    });
    ui.on_add_selection_to_playlist({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |id| {
            let ui = weak.unwrap();
            let links = browser.borrow().selected.clone();
            clear_selection(&ui, &browser);
            add_to_playlist(&ui, &browser, id.into(), links);
        }
    });
    ui.on_create_folder({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name| create_folder(&weak.unwrap(), &browser, name.trim().to_owned())
    });
    ui.on_rename_folder({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name| rename_folder(&weak.unwrap(), &browser, name.trim().to_owned())
    });
    ui.on_move_folder({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |target| move_folder(&weak.unwrap(), &browser, target.into())
    });
    ui.on_delete_folder({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || delete_folder(&weak.unwrap(), &browser)
    });
    ui.on_move_playlist({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |target| move_playlist(&weak.unwrap(), &browser, target.into())
    });
    ui.on_move_tile({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, target| {
            let link = browser
                .borrow()
                .tiles
                .row_data(row as usize)
                .map(|tile| tile.link);
            if let Some(id) = link
                .as_deref()
                .and_then(|link| link.strip_prefix("playlist:"))
            {
                move_playlists(&weak.unwrap(), &browser, vec![id.to_owned()], target.into());
            }
        }
    });
    ui.on_move_selection({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |target| {
            let ui = weak.unwrap();
            let ids = browser
                .borrow()
                .selected
                .iter()
                .filter_map(|link| link.strip_prefix("playlist:").map(str::to_owned))
                .collect();
            clear_selection(&ui, &browser);
            move_playlists(&ui, &browser, ids, target.into());
        }
    });
    ui.on_drop_on_tile({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            let ui = weak.unwrap();
            let link = browser
                .borrow()
                .tiles
                .row_data(row as usize)
                .map(|tile| tile.link);
            let folder = link
                .as_deref()
                .and_then(|link| link.strip_prefix("playlists:"));
            if let (Some(id), Some(folder)) = (dropped_playlist(&ui), folder) {
                move_playlists(&ui, &browser, vec![id], folder.to_owned());
            }
        }
    });
    ui.on_drop_on_section({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| {
            let ui = weak.unwrap();
            let heading = sections_of(&browser.borrow())
                .get(index as usize)
                .is_some_and(|section| section.0 == "playlists");
            if let Some(id) = dropped_playlist(&ui).filter(|_| heading) {
                move_playlists(&ui, &browser, vec![id], String::new());
            }
        }
    });
    ui.on_create_tag({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |parent, name| {
            let (parent, name) = (parent.to_string(), name.trim().to_owned());
            if name.is_empty() {
                return;
            }
            change_tags(
                &weak.unwrap(),
                &browser,
                |client| async move {
                    let parent = Some(parent.as_str()).filter(|parent| !parent.is_empty());
                    client.create_tag(&name, parent).await.map(drop)
                },
                |_, _, _| {},
            );
        }
    });
    ui.on_rename_tag({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |id, name| rename_tag(&weak.unwrap(), &browser, id.into(), name.trim().to_owned())
    });
    ui.on_move_tag({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |id, target| move_tag(&weak.unwrap(), &browser, id.into(), target.into())
    });
    ui.on_delete_tag({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |id| delete_tag(&weak.unwrap(), &browser, id.into())
    });
    ui.on_tag_line({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            let ui = weak.unwrap();
            let song = {
                let b = browser.borrow();
                let (songs, order) = listed(&b, ui.get_page());
                order.get(row as usize).map(|&song| song_link(&songs[song]))
            };
            start_tagging(&ui, &browser, song.into_iter().collect());
        }
    });
    ui.on_tag_track({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            let song = browser
                .borrow()
                .songs
                .get(row as usize)
                .map(|song| song.id.clone());
            let links = song.map(|song| format!("song:{song}")).into_iter();
            start_tagging(&weak.unwrap(), &browser, links.collect());
        }
    });
    ui.on_tag_selection({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            let links = browser.borrow().selected.clone();
            clear_selection(&ui, &browser);
            start_tagging(&ui, &browser, links);
        }
    });
    ui.on_toggle_tag({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| toggle_tag(&weak.unwrap(), &browser, index as usize)
    });
    ui.on_tag_with_new({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name| tag_with_new(&weak.unwrap(), &browser, name.trim().to_owned())
    });
}

/// Selects the tile at `row`, or lets it go if it is selected. With
/// `range`, selects every tile from the one last picked to it instead.
fn select_tile(ui: &AppWindow, browser: &Shared, row: usize, range: bool) {
    let tiles = browser.borrow().tiles.clone();
    let links: Vec<String> = tiles.iter().map(|tile| tile.link.into()).collect();
    let picked = pick(&mut browser.borrow_mut(), &links, row, range);
    for (row, selected) in picked {
        if let Some(mut tile) = tiles.row_data(row) {
            tile.selected = selected;
            tiles.set_row_data(row, tile);
        }
    }
    show_selected(ui, &browser.borrow());
}

/// Selects the song or composition of a row of the tracks or compositions
/// table, or lets it go if it is selected. With `range`, selects every row
/// from the one last picked to it instead.
fn select_line(ui: &AppWindow, browser: &Shared, row: usize, range: bool) {
    let page = ui.get_page();
    let mut b = browser.borrow_mut();
    let links = table_links(&b, page);
    let lines = b.lines.clone();
    for (row, selected) in pick(&mut b, &links, row, range) {
        if let Some(mut line) = lines.row_data(row) {
            line.selected = selected;
            lines.set_row_data(row, line);
        }
    }
    show_selected(ui, &b);
}

/// The links of the rows of the tracks or compositions table on `page`, in
/// the order shown.
fn table_links(b: &Browser, page: Page) -> Vec<String> {
    if page == Page::Compositions {
        return b
            .view
            .iter()
            .map(|&piece| piece_link(&b.compositions[piece]))
            .collect();
    }
    let (songs, order) = listed(b, page);
    order.iter().map(|&song| song_link(&songs[song])).collect()
}

/// Puts `songs` on screen as the rows of an album's, playlist's, artist's or
/// downloads page, the ones already selected marked as such.
fn show_songs(b: &mut Browser, songs: Vec<Child>, rows: Vec<Track>) {
    b.tracks.set_vec(rows);
    b.songs = songs;
    show_track_selection(b);
}

/// Selects the song of a track row of an album, playlist, artist or
/// downloads page, or lets it go if it is selected. With `range`, selects
/// every row from the one last picked to it instead.
fn select_track(ui: &AppWindow, browser: &Shared, row: usize, range: bool) {
    let mut b = browser.borrow_mut();
    let links: Vec<String> = b.songs.iter().map(song_link).collect();
    pick(&mut b, &links, row, range);
    created_for::forget_missing(&mut b);
    show_track_selection(&b);
    show_selected(ui, &b);
}

/// Selects the songs of the work whose row heads the tracks from `row`, or
/// lets them all go if they are all selected. With `range`, selects every
/// row from the one last picked to the work's nearest end instead.
fn select_work(ui: &AppWindow, browser: &Shared, row: usize, range: bool) {
    let mut b = browser.borrow_mut();
    let Some(movements) = b
        .tracks
        .row_data(row)
        .map(|track| track.work_tracks as usize)
    else {
        return;
    };
    let links: Vec<String> = b.songs.iter().map(song_link).collect();
    let work = row..(row + movements).min(links.len());
    let anchor = b
        .anchor
        .as_ref()
        .and_then(|anchor| links.iter().position(|link| link == anchor));
    match anchor.filter(|anchor| range && !work.contains(anchor)) {
        Some(anchor) => {
            let end = if anchor < row {
                work.end - 1
            } else {
                work.start
            };
            pick(&mut b, &links, end, true);
        }
        None => pick_all(&mut b, &links[work]),
    }
    show_track_selection(&b);
    show_selected(ui, &b);
}

/// Shows on every track row whether its song is selected, and on a work's
/// row whether all of the work's songs are, whichever way they were picked;
/// and for each such row on screen, the neighbours it is selected with, so
/// that a run of them is outlined as one.
fn show_track_selection(b: &Browser) {
    let is_selected = |song: &Child| b.selected.contains(&song_link(song));
    let mut rows = Vec::new();
    for row in 0..b.tracks.row_count() {
        let (Some(track), Some(song)) = (b.tracks.row_data(row), b.songs.get(row)) else {
            continue;
        };
        let work = &b.songs[row..(row + track.work_tracks as usize).min(b.songs.len())];
        let work_selected = !work.is_empty() && work.iter().all(is_selected);
        rows.push((row, track, is_selected(song), work_selected));
    }
    // A work's row is on screen above its first movement.
    let on_screen: Vec<bool> = rows
        .iter()
        .flat_map(|(_, track, selected, work_selected)| {
            let heading = (track.work_tracks > 0).then_some(*work_selected);
            heading.into_iter().chain([*selected])
        })
        .collect();
    let mut joins = joined(&on_screen).into_iter();
    for (row, track, selected, work_selected) in rows {
        let work_joined_above = track.work_tracks > 0 && joins.next().unwrap_or_default().0;
        let (joined_above, joined_below) = joins.next().unwrap_or_default();
        let shown = Track {
            selected,
            work_selected,
            joined_above,
            joined_below,
            work_joined_above,
            ..track.clone()
        };
        if shown != track {
            b.tracks.set_row_data(row, shown);
        }
    }
}

/// For each of `selected` rows in a column, whether the row above it and the
/// row below it are selected as well; none of a row that isn't selected.
fn joined(selected: &[bool]) -> Vec<(bool, bool)> {
    (0..selected.len())
        .map(|at| {
            let next_to = |other: Option<usize>| {
                other.is_some_and(|other| selected.get(other) == Some(&true))
            };
            (
                selected[at] && next_to(at.checked_sub(1)),
                selected[at] && next_to(Some(at + 1)),
            )
        })
        .collect()
}

/// The kinds of links whose songs the selection's menu can play.
const WITH_SONGS: [&str; 5] = ["song", "album", "artist", "playlist", "folder"];

/// Says how many tiles and rows are selected, whether any has songs to play,
/// and where the playlists among them can be moved: the top level and every
/// folder.
fn show_selected(ui: &AppWindow, b: &Browser) {
    ui.set_selected_tiles(b.selected.len() as i32);
    let has_songs = |link: &String| {
        link.split_once(':')
            .is_some_and(|(kind, _)| WITH_SONGS.contains(&kind))
    };
    ui.set_selection_has_songs(b.selected.iter().any(has_songs));
    let playlists = b.selected.iter().any(|link| link.starts_with("playlist:"));
    let moves = match folders_of(b) {
        Some(folders) if playlists && !folders.is_empty() => {
            destinations(&folder_nodes(folders), None, Some(""))
        }
        _ => Vec::new(),
    };
    ui.set_selection_moves(ModelRc::new(VecModel::from(moves)));
}

/// Picks the item at `row` of a page's `links`, or lets it go if it is
/// picked, and makes it where a range starts. With `range` and such a start
/// on the page, picks every item from there to `row` instead. Gives the rows
/// changed and whether each is picked now.
fn pick(b: &mut Browser, links: &[String], row: usize, range: bool) -> Vec<(usize, bool)> {
    let Some(link) = links.get(row) else {
        return Vec::new();
    };
    let start = b
        .anchor
        .as_ref()
        .filter(|_| range)
        .and_then(|anchor| links.iter().position(|link| link == anchor));
    if let Some(start) = start {
        let rows = start.min(row)..=start.max(row);
        for link in &links[rows.clone()] {
            if !b.selected.contains(link) {
                b.selected.push(link.clone());
            }
        }
        return rows.map(|row| (row, true)).collect();
    }
    b.anchor = Some(link.clone());
    match b.selected.iter().position(|picked| picked == link) {
        Some(at) => {
            b.selected.remove(at);
            vec![(row, false)]
        }
        None => {
            b.selected.push(link.clone());
            vec![(row, true)]
        }
    }
}

/// Picks all of `links`, or lets them all go if all are picked, and makes the
/// first where a range starts.
fn pick_all(b: &mut Browser, links: &[String]) {
    b.anchor = links.first().cloned();
    if links.iter().all(|link| b.selected.contains(link)) {
        b.selected.retain(|picked| !links.contains(picked));
        return;
    }
    for link in links {
        if !b.selected.contains(link) {
            b.selected.push(link.clone());
        }
    }
}

/// A song's page, or a folder's for a folder in one, which also stands for
/// it in the selection.
fn song_link(song: &Child) -> String {
    if song.is_dir {
        format!("folder:{}", song.id)
    } else {
        format!("song:{}", song.id)
    }
}

fn clear_selection(ui: &AppWindow, browser: &Shared) {
    let (tiles, lines) = {
        let mut b = browser.borrow_mut();
        b.selected.clear();
        b.anchor = None;
        (b.tiles.clone(), b.lines.clone())
    };
    for row in 0..tiles.row_count() {
        if let Some(mut tile) = tiles.row_data(row).filter(|tile| tile.selected) {
            tile.selected = false;
            tiles.set_row_data(row, tile);
        }
    }
    for row in 0..lines.row_count() {
        if let Some(mut line) = lines.row_data(row).filter(|line| line.selected) {
            line.selected = false;
            lines.set_row_data(row, line);
        }
    }
    show_track_selection(&browser.borrow());
    show_selected(ui, &browser.borrow());
}

/// The links selected, in the order `in_page_order` gives; and ends
/// selecting.
pub fn take_selection(ui: &AppWindow, browser: &Shared) -> Vec<String> {
    let links = in_page_order(&browser.borrow(), ui.get_page());
    clear_selection(ui, browser);
    links
}

/// The links selected, those on `page` in the order it shows them and the
/// rest after them in the order picked.
fn in_page_order(b: &Browser, page: Page) -> Vec<String> {
    let shown = selectable_links(b, page);
    let place: HashMap<&str, usize> = shown
        .iter()
        .enumerate()
        .map(|(at, link)| (link.as_str(), at))
        .collect();
    let mut links = b.selected.clone();
    links.sort_by_key(|link| place.get(link.as_str()).copied().unwrap_or(usize::MAX));
    links
}

/// The links of what can be selected on `page`, in the order it shows them:
/// a table's rows, or else the track rows and then the tiles, but the tiles
/// first on the downloads page.
fn selectable_links(b: &Browser, page: Page) -> Vec<String> {
    if matches!(
        page,
        Page::Tracks | Page::Compositions | Page::Folders | Page::Tag
    ) {
        return table_links(b, page);
    }
    let rows = b.songs.iter().map(song_link);
    let tiles = b.tiles.iter().map(|tile| tile.link.to_string());
    if page == Page::Downloads {
        tiles.chain(rows).collect()
    } else {
        rows.chain(tiles).collect()
    }
}

/// The songs of `links` that the pages left in the browser, by link, so as
/// not to ask the server for them again.
pub fn songs_held(browser: &Shared, links: &[String]) -> HashMap<String, Child> {
    let wanted: HashSet<&str> = links
        .iter()
        .filter_map(|link| link.strip_prefix("song:"))
        .collect();
    let b = browser.borrow();
    let held = b.songs.iter().chain(&b.song_list).chain(&b.entries);
    held.filter(|song| !song.is_dir && wanted.contains(song.id.as_str()))
        .map(|song| (song_link(song), song.clone()))
        .collect()
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
    ui.set_arrival(ui.get_arrival() + 1);
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
    ui.set_arrival(ui.get_arrival() + 1);
    show(ui, browser);
}

/// Goes back a page, or with none to go back to, to the home page, put
/// before the page on screen so that forward comes back to it. On the home
/// page with nothing before it, stays.
fn back_or_home(ui: &AppWindow, browser: &Shared) {
    if browser.borrow().at > 0 {
        return step(ui, browser, -1);
    }
    {
        let mut b = browser.borrow_mut();
        if b.history.first().is_none_or(|link| link == "home") {
            return;
        }
        b.history.insert(0, "home".into());
    }
    ui.set_arrival(ui.get_arrival() + 1);
    show(ui, browser);
}

/// Shows the page on screen again, with the library's albums, artists,
/// composers, compositions, genres and songs asked for anew. A list still
/// arriving is new already and goes on arriving.
fn refresh(ui: &AppWindow, browser: &Shared) {
    {
        let mut b = browser.borrow_mut();
        // Shown at once, and asked for again.
        if let Some((_, answers)) = &b.answers {
            answers.age_all();
        }
        if let Some(listenbrainz) = &b.listenbrainz_answers {
            listenbrainz.age_all();
        }
        // The orders index into the lists; the page shown sorts its own anew.
        b.view.clear();
        b.song_view.clear();
        if !b.library_loading {
            b.library.clear();
            b.library_loaded = false;
        }
        if !b.song_list_loading {
            b.song_list.clear();
            b.song_list_loaded = false;
        }
        b.artists_loaded = false;
        b.stats.forget_indexes();
        b.composers_loaded = false;
        b.compositions_loaded = false;
        b.genres_loaded = false;
    }
    show(ui, browser);
}

fn show(ui: &AppWindow, browser: &Shared) {
    let link = {
        let mut b = browser.borrow_mut();
        b.visit += 1;
        b.tiles = Rc::default();
        b.tracks = Rc::default();
        b.lines = Rc::default();
        b.songs.clear();
        b.album = None;
        b.playlist = None;
        b.covers.clear();
        b.fetching = 0;
        b.motion = Timer::default();
        b.scan.stop();
        b.anchor = None;
        ui.set_visit(b.visit);
        ui.set_can_go_back(b.at > 0);
        ui.set_can_go_forward(b.at + 1 < b.history.len());
        ui.set_tiles(ModelRc::from(b.tiles.clone()));
        ui.set_tracks(ModelRc::from(b.tracks.clone()));
        ui.set_track_lines(ModelRc::from(b.lines.clone()));
        ui.set_section(section(&b));
        ui.set_bar_section(bar_section(&b));
        b.history[b.at].clone()
    };
    ui.set_page_problem("".into());
    ui.set_loading(false);
    ui.set_more_tiles(false);
    show_selected(ui, &browser.borrow());
    ui.set_header_image(Image::default());
    ui.set_header_title("".into());
    ui.set_header_names(ModelRc::default());
    ui.set_header_facts("".into());
    ui.set_details(ModelRc::default());
    ui.set_playlist_editable(false);
    ui.set_created_for(false);
    ui.set_created_for_matching(false);
    ui.set_download_state(NOT_DOWNLOADED);
    ui.set_download_progress("".into());
    ui.set_shown_id("".into());
    ui.set_move_targets(ModelRc::default());
    ui.set_tile_moves(ModelRc::default());
    ui.set_about("".into());
    ui.set_about_short("".into());
    ui.set_about_open(false);
    ui.set_similar(ModelRc::default());
    ui.set_genre_tiles(ModelRc::default());
    ui.set_genre_artists(ModelRc::default());
    ui.set_genre_banner(Image::default());
    ui.set_genre_radio(StationShelf::default());
    ui.set_about_source("".into());
    ui.set_about_url("".into());

    let (page, id) = link.split_once(':').unwrap_or((&link, ""));
    let id = id.to_owned();
    match page {
        "home" => {
            ui.set_page(Page::Home);
            home(ui, browser);
        }
        "artists" => {
            ui.set_page(Page::Artists);
            ui.set_header_title(id.as_str().into());
            let by_plays = ui.get_artist_sort() == MOST_PLAYED_ARTISTS;
            if !id.is_empty() || by_plays {
                want_library(ui, browser);
            }
            if by_plays {
                recount_library(ui, browser);
            }
            artists(ui, browser);
        }
        "tracks" => {
            ui.set_page(Page::Tracks);
            tracks(ui, browser);
        }
        "playlists" => {
            ui.set_page(Page::Playlists);
            ui.set_shown_id(id.as_str().into());
            playlists(ui, browser);
        }
        "album" => {
            ui.set_page(Page::Album);
            album(ui, browser, id);
        }
        "artist" => {
            ui.set_page(Page::Artist);
            artist(ui, browser, id);
        }
        "playlist" => {
            ui.set_page(Page::Playlist);
            playlist(ui, browser, id);
        }
        "createdfor" => {
            ui.set_page(Page::Playlist);
            created_for::open_page(ui, browser, id);
        }
        "history" => {
            ui.set_page(Page::History);
            listens::open_page(ui, browser, &id);
        }
        "song" => {
            ui.set_page(Page::Track);
            song(ui, browser, id);
        }
        "folders" | "folder" => {
            ui.set_page(Page::Folders);
            folders(ui, browser, link.clone());
        }
        "tags" => {
            ui.set_page(Page::Tags);
            tags(ui, browser);
        }
        "tag" => {
            ui.set_page(Page::Tag);
            folders(ui, browser, link.clone());
        }
        "composers" => {
            ui.set_page(Page::Composers);
            recount_composers(ui, browser);
            composers(ui, browser);
        }
        "compositions" => {
            ui.set_page(Page::Compositions);
            recount_compositions(ui, browser);
            compositions(ui, browser);
        }
        "composer" => {
            ui.set_page(Page::Composer);
            composer(ui, browser, id);
        }
        "composition" => {
            ui.set_page(Page::Composition);
            composition(ui, browser, id);
        }
        "genres" => {
            ui.set_page(Page::Genres);
            genres(ui, browser);
        }
        "genre" => {
            ui.set_page(Page::Genre);
            genre(ui, browser, id);
        }
        "account" => {
            ui.set_page(Page::Account);
            show_downloads(ui, browser);
            // Asks again when the server didn't say at sign-in.
            if browser.borrow().me.is_none() {
                load_me(ui, browser);
            }
        }
        "downloads" => {
            ui.set_page(Page::Downloads);
            downloads_page(ui, browser);
        }
        "search" => {
            ui.set_page(Page::Search);
            search::show_page(ui, browser);
        }
        "browse" => ui.set_page(Page::Browse),
        "library" => ui.set_page(Page::Library),
        "settings" => ui.set_page(Page::Settings),
        "setting" => {
            ui.set_page(Page::Setting);
            ui.set_shown_id(id.as_str().into());
            if id == "downloads" {
                show_downloads(ui, browser);
            }
            if id == "library" {
                scan_status(ui, browser);
                // Whether it offers the scan follows the roles the server gives now.
                load_me(ui, browser);
            }
        }
        "radio" => {
            ui.set_page(Page::Radio);
            stations(ui, browser);
            directory_shelves(ui, browser);
            catalogue_stations(ui, browser);
        }
        "stations" => {
            ui.set_page(Page::Stations);
            {
                let mut b = browser.borrow_mut();
                ui.set_header_title(page_title(&b, &id).into());
                b.genre = id;
                b.genre_stations.clear();
                b.genre_offset = 0;
                b.genre_more = true;
                b.genre_loading = false;
            }
            more_stations(ui, browser);
        }
        _ => {
            ui.set_page(Page::Albums);
            ui.set_header_title(shown_genre(&browser.borrow()).into());
            albums(ui, browser);
        }
    }
}

/// Whether the server has Fonoteca's composers and compositions.
fn offers_compositions(b: &Browser) -> bool {
    b.client
        .as_ref()
        .is_some_and(|client| client.supports(COMPOSITIONS))
}

/// Whether the server counts the plays of its composers and compositions.
fn counts_composition_plays(b: &Browser) -> bool {
    offers_version(b, COMPOSITIONS, COMPOSITION_PLAYS)
}

/// Whether the server has this extension.
fn offers(b: &Browser, extension: &str) -> bool {
    b.client
        .as_ref()
        .is_some_and(|client| client.supports(extension))
}

/// Whether the server has this version of the extension.
fn offers_version(b: &Browser, extension: &str, version: i64) -> bool {
    b.client
        .as_ref()
        .is_some_and(|client| client.supports_version(extension, version))
}

/// The playlist folders, on servers with them.
fn folders_of(b: &Browser) -> Option<&[PlaylistFolder]> {
    offers(b, FOLDERS).then_some(&b.playlist_folders[..])
}

/// The sidebar's entries for what the server has.
fn sections_of(b: &Browser) -> Vec<(String, &str, &str)> {
    sections(
        &b.playlists,
        offers_compositions(b),
        offers(b, TAGS),
        b.downloads.any(),
    )
}

/// The sidebar's entries, as links, labels and groups: the home page, genres,
/// tags where the server has them, the radio and the history, the library's
/// pages, with the composers and compositions where the server has them,
/// and every playlist, those in folders too.
fn sections(
    playlists: &[Playlist],
    compositions: bool,
    tags: bool,
    downloads: bool,
) -> Vec<(String, &str, &str)> {
    let library = [
        ("albums", "Albums", true),
        ("artists", "Artists", true),
        ("tracks", "Tracks", true),
        ("composers", "Composers", compositions),
        ("compositions", "Compositions", compositions),
        ("folders", "Folders", true),
    ]
    .into_iter()
    .filter(|&(.., shown)| shown)
    .map(|(link, label, _)| (link.to_owned(), label, "Library"));
    let playlists = playlists.iter().map(|playlist| {
        let link = format!("playlist:{}", playlist.id);
        (link, playlist.name.as_str(), "Playlists")
    });
    [
        ("home", "Home", true),
        ("genres", "Genres", true),
        ("tags", "Tags", tags),
        ("radio", "Radio", true),
        ("history", "History", true),
    ]
    .into_iter()
    .filter(|&(.., shown)| shown)
    .map(|(link, label, _)| (link.to_owned(), label, "Browse"))
    .chain(downloads.then(|| ("downloads".to_owned(), "Downloads", "Browse")))
    .chain(library)
    .chain([("playlists".to_owned(), "Playlists", "Playlists")])
    .chain(playlists)
    .collect()
}

/// A playlist folder or a playlist, in the order of those beside it.
enum Placed<'a> {
    Folder(&'a PlaylistFolder),
    Playlist(&'a Playlist),
}

/// The playlists, in the server's order; or with `folders`, the folders and
/// playlists at the top level, or in `folder`, in the order the user put
/// them in, then by name.
fn placed<'a>(
    playlists: &'a [Playlist],
    folders: Option<&'a [PlaylistFolder]>,
    folder: Option<&str>,
) -> Vec<Placed<'a>> {
    let Some(folders) = folders else {
        return playlists.iter().map(Placed::Playlist).collect();
    };
    let nodes = folder_nodes(folders);
    let known = |id: Option<&'a str>| id.filter(|id| nodes.iter().any(|node| node.0 == *id));
    let sub = folders
        .iter()
        .zip(&nodes)
        .filter(|(_, node)| node.2 == folder)
        .map(|(sub, _)| (sub.sort_order, &sub.name, Placed::Folder(sub)));
    let lists = playlists
        .iter()
        .filter(|playlist| known(playlist.playlist_folder_id.as_deref()) == folder)
        .map(|playlist| {
            let order = playlist.playlist_folder_sort_order;
            (order, &playlist.name, Placed::Playlist(playlist))
        });
    let mut items: Vec<_> = sub.chain(lists).collect();
    items.sort_by_cached_key(|(order, name, _)| (order.is_none(), *order, name.to_lowercase()));
    items.into_iter().map(|(.., item)| item).collect()
}

/// A playlist folder or tag: its id, its name and the one it is in, `None`
/// at the top level, as is one in a folder or tag that isn't known.
type Node<'a> = (&'a str, &'a str, Option<&'a str>);

fn rooted<'a>(nodes: impl Iterator<Item = Node<'a>>) -> Vec<Node<'a>> {
    let nodes: Vec<Node> = nodes.collect();
    let ids: HashSet<&str> = nodes.iter().map(|node| node.0).collect();
    nodes
        .into_iter()
        .map(|(id, name, parent)| (id, name, parent.filter(|parent| ids.contains(parent))))
        .collect()
}

fn folder_nodes(folders: &[PlaylistFolder]) -> Vec<Node<'_>> {
    rooted(folders.iter().map(|folder| {
        let parent = folder.parent_id.as_deref();
        (folder.id.as_str(), folder.name.as_str(), parent)
    }))
}

fn tag_nodes(tags: &[Tag]) -> Vec<Node<'_>> {
    rooted(
        tags.iter()
            .map(|tag| (tag.id.as_str(), tag.name.as_str(), tag.parent_id.as_deref())),
    )
}

/// The nodes, each followed by those in it and with how deep it sits, those
/// beside each other by name.
fn tree<'a>(nodes: &[Node<'a>]) -> Vec<(Node<'a>, usize)> {
    fn under<'a>(
        nodes: &[Node<'a>],
        parent: Option<&str>,
        depth: usize,
        tree: &mut Vec<(Node<'a>, usize)>,
    ) {
        if depth > nodes.len() {
            return;
        }
        let mut children: Vec<Node> = nodes
            .iter()
            .copied()
            .filter(|node| node.2 == parent)
            .collect();
        children.sort_by_cached_key(|node| node.1.to_lowercase());
        for child in children {
            tree.push((child, depth));
            under(nodes, Some(child.0), depth + 1, tree);
        }
    }
    let mut tree = Vec::new();
    under(nodes, None, 0, &mut tree);
    tree
}

/// Whether `id` sits somewhere in `ancestor`.
fn within(nodes: &[Node], id: &str, ancestor: &str) -> bool {
    let mut at = id;
    for _ in 0..nodes.len() {
        let Some(parent) = nodes
            .iter()
            .find(|node| node.0 == at)
            .and_then(|node| node.2)
        else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        at = parent;
    }
    false
}

/// "Mood / Calm": the names of the node and of those it is in.
fn path(nodes: &[Node], id: &str) -> String {
    let mut names = Vec::new();
    let mut at = Some(id);
    while let Some(id) = at
        && names.len() <= nodes.len()
        && let Some(node) = nodes.iter().find(|node| node.0 == id)
    {
        names.push(node.1);
        at = node.2;
    }
    names.reverse();
    names.join(" / ")
}

/// Where the folder or tag `moving`, or with none a playlist, can go from
/// the one it is in, as names linking to their ids: the top level, linking
/// to "", then every other one but those in it.
fn destinations(nodes: &[Node], moving: Option<&str>, from: Option<&str>) -> Vec<Name> {
    let top = from.is_some().then(|| Name {
        label: "Top level".into(),
        link: "".into(),
    });
    let others = tree(nodes)
        .into_iter()
        .map(|(node, _)| node.0)
        .filter(|&id| Some(id) != from)
        .filter(|&id| moving.is_none_or(|moving| id != moving && !within(nodes, id, moving)))
        .map(|id| Name {
            label: path(nodes, id).into(),
            link: id.into(),
        });
    top.into_iter().chain(others).collect()
}

/// The sidebar entry of the page on screen, or -1 when it has none: its own;
/// else that of the pages it is among, as an album's is Albums and a
/// playlist folder's Playlists.
fn section(b: &Browser) -> i32 {
    let Some(link) = b.history.get(b.at) else {
        return -1;
    };
    let sections = sections_of(b);
    let at = |wanted: &str| sections.iter().position(|section| section.0 == wanted);
    let page = link.split_once(':').map_or(link.as_str(), |(page, _)| page);
    let among = match page {
        "album" | "albums" => "albums",
        "artist" | "artists" => "artists",
        "song" => "tracks",
        "genre" => "genres",
        "tag" => "tags",
        "composer" => "composers",
        "composition" => "compositions",
        "folder" => "folders",
        "playlist" | "playlists" => "playlists",
        "stations" => "radio",
        "createdfor" => "home",
        other => other,
    };
    let found = at(link).or_else(|| at(among));
    found.map_or(-1, |at| at as i32)
}

/// The phone's bottom bar: the links it opens and their names.
const BAR: [(&str, &str); 4] = [
    ("home", "Home"),
    ("browse", "Browse"),
    ("library", "Library"),
    ("settings", "Settings"),
];

fn bar_entries(icons: &Icons) -> ModelRc<NavEntry> {
    let icon = |link| match link {
        "home" => icons.get_home(),
        "browse" => icons.get_genre(),
        "library" => icons.get_album(),
        _ => icons.get_settings(),
    };
    let entries: Vec<_> = BAR
        .iter()
        .map(|&(link, title)| NavEntry {
            title: title.into(),
            icon: icon(link),
            link: link.into(),
        })
        .collect();
    ModelRc::new(VecModel::from(entries))
}

/// The bottom bar's entry of the page on screen, or -1 when it has none:
/// the account's and the search's are on the top bar. Every page of the
/// library, such as an album, belongs to Library.
fn bar_section(b: &Browser) -> i32 {
    let Some(link) = b.history.get(b.at) else {
        return -1;
    };
    let page = link.split_once(':').map_or(link.as_str(), |(page, _)| page);
    let entry = match page {
        "home" | "createdfor" => "home",
        "browse" | "genres" | "genre" | "tags" | "tag" | "radio" | "stations" | "history" => {
            "browse"
        }
        "settings" | "setting" => "settings",
        "account" | "search" => return -1,
        _ => "library",
    };
    BAR.iter()
        .position(|&(link, _)| link == entry)
        .map_or(-1, |at| at as i32)
}

/// What the Browse page, or else the Library page, links to: the sidebar's
/// entries, but not the playlists: their page is enough here.
fn nav_entries(
    icons: &Icons,
    sections: &[(String, &str, &str)],
    browse: bool,
) -> ModelRc<NavEntry> {
    let entries: Vec<_> = sections
        .iter()
        .filter(|(link, ..)| {
            link != "home"
                && !link.starts_with("playlist:")
                && matches!(link.as_str(), "genres" | "tags" | "radio" | "history") == browse
        })
        .map(|(link, title, _)| NavEntry {
            title: (*title).into(),
            icon: match link.as_str() {
                "playlists" => icons.get_playlist(),
                _ => section_icon(icons, link),
            },
            link: link.into(),
        })
        .collect();
    ModelRc::new(VecModel::from(entries))
}

/// Lists the sidebar's entries, the page on screen's current, the playlists
/// songs can be added to: those the server doesn't call read-only, and
/// whether the server has playlist folders and tags.
fn sidebar(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    let icons = ui.global::<Icons>();
    let (mut labels, mut groups, mut pictures) = (Vec::new(), Vec::new(), Vec::new());
    for (link, label, group) in sections_of(&b) {
        labels.push(SharedString::from(label));
        groups.push(SharedString::from(group));
        pictures.push(section_icon(&icons, &link));
    }
    ui.set_sections(ModelRc::new(VecModel::from(labels)));
    ui.set_section_groups(ModelRc::new(VecModel::from(groups)));
    ui.set_section_icons(ModelRc::new(VecModel::from(pictures)));
    ui.set_section(section(&b));
    ui.set_bar_section(bar_section(&b));
    let entries = sections_of(&b);
    ui.set_browse_links(nav_entries(&icons, &entries, true));
    ui.set_library_links(nav_entries(&icons, &entries, false));
    show_selected(ui, &b);
    ui.set_folders_offered(offers(&b, FOLDERS));
    ui.set_tags_offered(offers(&b, TAGS));
    ui.set_composer_plays_counted(counts_composition_plays(&b));
    let targets: Vec<Name> = b
        .playlists
        .iter()
        .filter(|playlist| playlist.readonly != Some(true))
        .map(|playlist| Name {
            label: playlist.name.as_str().into(),
            link: playlist.id.as_str().into(),
        })
        .collect();
    ui.set_playlist_targets(names(targets));
    let listed = b.downloads.any();
    drop(b);
    browser.borrow_mut().downloads_listed = listed;
}

/// The icon in front of the sidebar's entry for `link`; none for the
/// playlists, whose heading is their page's entry.
fn section_icon(icons: &Icons, link: &str) -> SharedString {
    match link.split_once(':').map_or(link, |(page, _)| page) {
        "home" => icons.get_home(),
        "albums" => icons.get_album(),
        "artists" => icons.get_artist(),
        "tracks" => icons.get_note(),
        "composers" => icons.get_composer(),
        "compositions" => icons.get_composition(),
        "folders" => icons.get_folder(),
        "tags" => icons.get_tag(),
        "playlists" | "playlist" => SharedString::new(),
        "radio" => icons.get_radio(),
        "history" => icons.get_history(),
        "genres" => icons.get_genre(),
        "downloads" => icons.get_download(),
        _ => icons.get_person(),
    }
}

/// Runs `task` for the page on screen, and drops its answer if that page has
/// been left by then. An aged answer shows at once, and then `task` runs
/// again, asking the server.
fn load<T: 'static, F: Future<Output = opensubsonic::Result<T>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    task: impl Fn(Arc<Client>) -> F + 'static,
    done: impl Fn(&AppWindow, &Shared, T) + 'static,
) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let visit = browser.borrow().visit;
    let reader = Arc::new(client.reader(true));
    let first = task(reader.clone());
    ui.set_loading(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = first.await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().visit != visit {
            return;
        }
        ui.set_loading(false);
        match result {
            Ok(value) => done(&ui, &browser, value),
            Err(e) => return ui.set_page_problem(e.to_string().into()),
        }
        if reader.gave_aged() {
            ask_again(weak, &browser, visit, &client, task, done).await;
        }
    })
    .expect("the event loop is running");
}

/// Runs `task` again after it showed aged answers, with a client that asks
/// the server for them, and puts what it says in their place with `done`
/// where it differs, unless the page has been left by then. When the server
/// can't answer, the aged answers stay.
async fn ask_again<T, F: Future<Output = opensubsonic::Result<T>>>(
    weak: slint::Weak<AppWindow>,
    browser: &Shared,
    visit: i32,
    client: &Client,
    task: impl Fn(Arc<Client>) -> F,
    done: impl Fn(&AppWindow, &Shared, T),
) {
    let asker = Arc::new(client.reader(false));
    let answer = task(asker.clone()).await;
    let Some(ui) = weak.upgrade() else { return };
    let Ok(value) = answer else { return };
    if !asker.heard_news() || browser.borrow().visit != visit {
        return;
    }
    {
        // The orders index into the lists; the page sorts its own anew.
        let mut b = browser.borrow_mut();
        b.view.clear();
        b.song_view.clear();
    }
    done(&ui, browser, value);
}

/// Shows the library's counts and its newest albums on the home page, asking
/// for what hasn't arrived yet.
fn home(ui: &AppWindow, browser: &Shared) {
    let (ask, more) = {
        let b = browser.borrow();
        (
            !b.library_loaded && !b.library_loading,
            offers_compositions(&b),
        )
    };
    show_home(ui, browser);
    if ask {
        load_library(ui, browser);
    }
    count_tracks(ui, browser);
    played::ask(ui, browser);
    saved_stations(ui, browser);
    created_for::shelf(ui, browser);
    stats::ask(ui, browser);
    artists(ui, browser);
    if more {
        composers(ui, browser);
        compositions(ui, browser);
    }
}

/// The home page's counts, each once all of it has arrived, and the newest
/// albums once the library has.
fn show_home(ui: &AppWindow, browser: &Shared) {
    stats::show(ui, browser);
    played::show(ui, browser);
    let icons = ui.global::<Icons>();
    let stat = |icon: SharedString, label: &str, n: Option<usize>| Stat {
        icon,
        label: label.into(),
        value: n.map_or("…".into(), |n| n.to_string().into()),
        link: label.to_lowercase().into(),
    };
    let tiles: Vec<_> = {
        let b = browser.borrow();
        let library = b.library_loaded.then_some(&b.library);
        let mut stats = vec![
            stat(
                icons.get_artist(),
                "Artists",
                b.artists_loaded.then_some(b.artists.len()),
            ),
            stat(icons.get_album(), "Albums", library.map(Vec::len)),
            stat(icons.get_note(), "Tracks", track_count(&b)),
        ];
        if offers_compositions(&b) {
            stats.push(stat(
                icons.get_composer(),
                "Composers",
                b.composers_loaded.then_some(b.composers.len()),
            ));
            stats.push(stat(
                icons.get_composition(),
                "Compositions",
                b.compositions_loaded.then_some(b.compositions.len()),
            ));
        }
        ui.set_stats(ModelRc::new(VecModel::from(stats)));
        let Some(library) = library else { return };
        let newest: Vec<_> = arranged(library, "", 2, true)
            .iter()
            .take(RECENT)
            .map(|&i| album_tile(&library[i], album_names(&library[i])))
            .collect();
        let shown = b.tiles.row_count() == newest.len()
            && (b.tiles.iter().zip(&newest)).all(|(shown, (tile, _))| is_same_album(&shown, tile));
        if shown {
            return;
        }
        newest
    };
    // A library asked for again may have others.
    clear_tiles(ui, &mut browser.borrow_mut());
    add_tiles(ui, browser, tiles);
}

/// How many tracks the home page counts: as many as the tracks page lists,
/// once they have arrived; until then as many as the server's last scan
/// counted, which matches them; and from a server that doesn't count, the
/// albums' songs.
fn track_count(b: &Browser) -> Option<usize> {
    let albums_songs = || b.library.iter().map(|a| a.song_count.max(0) as usize).sum();
    (b.song_list_loaded.then_some(b.song_list.len()))
        .or(b.scanned_tracks)
        .or_else(|| (b.library_loaded && b.scanned_tracks_asked).then(albums_songs))
}

/// Asks the server how many tracks its scan counted, for the home page. A
/// scan still going counts what it has walked so far, so the count from
/// before it stays, as it does when the server can't answer.
fn count_tracks(ui: &AppWindow, browser: &Shared) {
    aside(
        ui,
        browser,
        |client| async move { Ok(client.get_scan_status().await.ok()) },
        |ui, browser, status: Option<ScanStatus>| {
            {
                let mut b = browser.borrow_mut();
                b.scanned_tracks_asked = true;
                let finished = status.filter(|status| !status.scanning);
                if let Some(count) = finished.and_then(|status| status.count) {
                    b.scanned_tracks = usize::try_from(count).ok();
                }
            }
            show_home(ui, browser);
        },
    );
}

/// Whether two album tiles name the same album the same way.
fn is_same_album(a: &AlbumTile, b: &AlbumTile) -> bool {
    a.link == b.link && a.title == b.title && a.names.iter().eq(b.names.iter())
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
    if ui.get_album_sort() == MOST_PLAYED_ALBUMS {
        recount_library(ui, browser);
    }
    arrange(ui, browser);
}

/// Asks for the library again, for a page sorted by plays, when the server
/// was told of one since it was asked for. The albums shown stay until all
/// have come, and then take their place where any differ.
fn recount_library(ui: &AppWindow, browser: &Shared) {
    let mut b = browser.borrow_mut();
    if !b.library_misses_plays || !b.library_loaded || b.library_loading {
        return;
    }
    b.library_misses_plays = false;
    drop(b);
    ask_library(ui, browser, false);
}

/// Asks for every album, a page at a time, and arranges the albums page as
/// they arrive. Sorting and filtering then need nothing from the server.
fn load_library(ui: &AppWindow, browser: &Shared) {
    ask_library(ui, browser, true);
}

/// Asks for the library as `load_library` says, showing aged answers at once
/// where `shows_aged`, and then asking again without: those albums take the
/// library's place once all have come, if any differ.
fn ask_library(ui: &AppWindow, browser: &Shared, shows_aged: bool) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let reader = client.reader(shows_aged);
    browser.borrow_mut().library_loading = true;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let mut asked_again = Vec::new();
        loop {
            // Asked anew, it goes on from where the library stopped.
            let asked = match shows_aged {
                true => browser.borrow().library.len(),
                false => asked_again.len(),
            };
            let options = ListOptions {
                size: Some(LIBRARY_AT_ONCE),
                offset: Some(asked as u32),
                ..ListOptions::default()
            };
            let list = reader
                .get_album_list2(&AlbumListType::AlphabeticalByName, &options)
                .await;
            let Some(ui) = weak.upgrade() else { return };
            let signed_in = browser
                .borrow()
                .client
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &client));
            if !signed_in {
                return;
            }
            let shown = match ui.get_page() {
                Page::Albums | Page::Home | Page::Genres | Page::Genre => true,
                Page::Artists => {
                    !shown_genre(&browser.borrow()).is_empty()
                        || ui.get_artist_sort() == MOST_PLAYED_ARTISTS
                }
                _ => false,
            };
            let list = match list {
                Ok(list) => list.album,
                Err(e) => {
                    browser.borrow_mut().library_loading = false;
                    if !shows_aged {
                        // Its counts may miss a play, which the next page
                        // sorted by them asks for once more.
                        browser.borrow_mut().library_misses_plays = true;
                    }
                    // Asked again, the albums shown stay.
                    if shown && shows_aged {
                        ui.set_loading(false);
                        ui.set_page_problem(e.to_string().into());
                    }
                    return;
                }
            };
            let last = (list.len() as u32) < LIBRARY_AT_ONCE;
            let done = last || asked + list.len() >= LIBRARY_AT_MOST;
            let changed = {
                let mut b = browser.borrow_mut();
                b.library_loading = !done;
                b.library_loaded |= done;
                if shows_aged {
                    // New, it shows as it arrives.
                    b.library.extend(list);
                    true
                } else {
                    // Asked again, it takes the library's place once all of
                    // it is in, if any of it differs.
                    asked_again.extend(list);
                    let changed = done && reader.heard_news();
                    if changed {
                        // The orders and the stats' index point into the
                        // library; they are made anew.
                        b.view.clear();
                        b.stats.forget_indexes();
                        b.library = std::mem::take(&mut asked_again);
                    }
                    changed
                }
            };
            if shown && changed {
                arrange(&ui, &browser);
            }
            if done {
                if reader.gave_aged() {
                    ask_library(&ui, &browser, false);
                }
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
                // Asked again, they may differ from those the stats' index
                // points into.
                b.stats.forget_indexes();
            }
            arrange(ui, browser);
        },
    );
}

/// Shows every composer on the composers page, asking for them first if they
/// haven't arrived yet.
fn composers(ui: &AppWindow, browser: &Shared) {
    if browser.borrow().composers_loaded {
        arrange(ui, browser);
        return;
    }
    load(
        ui,
        browser,
        |client| async move { client.get_composers().await },
        |ui, browser, composers| {
            {
                let mut b = browser.borrow_mut();
                b.composers = composers
                    .index
                    .into_iter()
                    .flat_map(|index| index.composer)
                    .collect();
                b.composers_loaded = true;
            }
            arrange(ui, browser);
        },
    );
}

/// Shows the library's genres, or a genre's page, asking for them, the
/// library and the artists where they haven't arrived yet.
fn genres(ui: &AppWindow, browser: &Shared) {
    want_library(ui, browser);
    if !browser.borrow().artists_loaded {
        artists(ui, browser);
    }
    if browser.borrow().genres_loaded {
        arrange(ui, browser);
    } else {
        load_genres(ui, browser);
    }
}

/// A genre's page, from the library: its albums, its artists and the genres
/// under it, and the directory's stations for it.
fn genre(ui: &AppWindow, browser: &Shared, name: String) {
    ui.set_header_title(name.as_str().into());
    genres(ui, browser);
    genre_radio(ui, browser, &name);
}

/// Asks for the library unless it is in or on its way.
fn want_library(ui: &AppWindow, browser: &Shared) {
    let ask = {
        let b = browser.borrow();
        !b.library_loaded && !b.library_loading
    };
    if ask {
        load_library(ui, browser);
    }
}

/// Asks for every genre, then arranges the page.
fn load_genres(ui: &AppWindow, browser: &Shared) {
    load(
        ui,
        browser,
        |client| async move { client.get_genres().await },
        |ui, browser, genres| {
            {
                let mut b = browser.borrow_mut();
                b.genres = genres.genre;
                b.genres_loaded = true;
            }
            arrange(ui, browser);
        },
    );
}

/// Shows every composition on the compositions page, asking for them first if
/// they haven't arrived yet.
fn compositions(ui: &AppWindow, browser: &Shared) {
    show_columns(ui, browser);
    if browser.borrow().compositions_loaded {
        arrange(ui, browser);
        return;
    }
    load(
        ui,
        browser,
        |client| async move { client.get_compositions(None).await },
        |ui, browser, compositions| {
            {
                let mut b = browser.borrow_mut();
                b.compositions = compositions.composition;
                b.compositions_loaded = true;
            }
            arrange(ui, browser);
        },
    );
}

/// Asks for the composers again when their page sorts by plays and the
/// server was told of one since they were asked for.
fn recount_composers(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    let by_plays = counts_composition_plays(&b) && ui.get_composer_sort() == MOST_PLAYED_ARTISTS;
    if !b.composers_loaded || !by_plays {
        return;
    }
    drop(b);
    recount(
        ui,
        browser,
        Page::Composers,
        |b| &mut b.composers_miss_plays,
        |reader| async move { reader.get_composers().await },
        |b, composers| {
            b.composers = (composers.index.into_iter())
                .flat_map(|index| index.composer)
                .collect();
        },
    );
}

/// Asks for the compositions again when their table shows or sorts by the
/// plays and the server was told of one since they were asked for.
fn recount_compositions(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    let table = piece_table(&b);
    let uses_plays =
        (table.shown().into_iter().chain(table.sort)).any(|c| PIECE_COLUMNS[c].is_count);
    if !b.compositions_loaded || !uses_plays {
        return;
    }
    drop(b);
    recount(
        ui,
        browser,
        Page::Compositions,
        |b| &mut b.compositions_miss_plays,
        |reader| async move { reader.get_compositions(None).await },
        |b, compositions| b.compositions = compositions.composition,
    );
}

/// Asks the server for a list again with `task` when `misses_plays` says it
/// was told of a play since the list was asked for. What is shown stays
/// until the answer comes; `take` then puts it in the list's place, and
/// `page`, if on screen, shows it in its order. When the server can't
/// answer, the next visit asks again.
fn recount<T: 'static, F: Future<Output = opensubsonic::Result<T>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    page: Page,
    misses_plays: fn(&mut Browser) -> &mut bool,
    task: impl FnOnce(Client) -> F,
    take: impl FnOnce(&mut Browser, T) + 'static,
) {
    let client = {
        let mut b = browser.borrow_mut();
        let misses = misses_plays(&mut b);
        if !*misses {
            return;
        }
        *misses = false;
        b.client.clone()
    };
    let Some(client) = client else { return };
    let answer = task(client.reader(false));
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let answer = answer.await;
        let Some(ui) = weak.upgrade() else { return };
        let signed_in =
            (browser.borrow().client.as_ref()).is_some_and(|current| Arc::ptr_eq(current, &client));
        if !signed_in {
            return;
        }
        let list = match answer {
            Ok(list) => list,
            Err(e) => {
                eprintln!("The plays on the {page:?} page couldn't be asked for again: {e}");
                *misses_plays(&mut browser.borrow_mut()) = true;
                return;
            }
        };
        let mut b = browser.borrow_mut();
        take(&mut b, list);
        if ui.get_page() != page {
            return;
        }
        // The order indexes into the list; the page sorts it anew.
        b.view.clear();
        drop(b);
        arrange(&ui, &browser);
    })
    .expect("the event loop is running");
}

/// Asks for the playlists, for the sidebar and the playlists page.
fn load_playlists(ui: &AppWindow, browser: &Shared) {
    ask_playlists(ui, browser, true);
}

/// Asks for the playlists as `load_playlists` says, showing aged answers at
/// once where `shows_aged`, and then asking again without.
fn ask_playlists(ui: &AppWindow, browser: &Shared, shows_aged: bool) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let reader = client.reader(shows_aged);
    browser.borrow_mut().playlists_loading = true;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let answer = reader.get_playlists(None).await;
        // The folders known stay when asking for them fails.
        let folders = if reader.supports(FOLDERS) {
            reader.get_playlist_folders(None).await.ok()
        } else {
            None
        };
        let Some(ui) = weak.upgrade() else { return };
        let signed_in = browser
            .borrow()
            .client
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &client));
        if !signed_in {
            return;
        }
        let shown = ui.get_page() == Page::Playlists;
        #[cfg(not(target_family = "wasm"))]
        let mut out_of_step = (Vec::new(), Vec::new());
        let (changed, known) = {
            let mut b = browser.borrow_mut();
            b.playlists_loading = false;
            let known = b.playlists_loaded;
            let changed = match answer {
                Ok(list) => {
                    b.playlists_loaded = true;
                    #[cfg(not(target_family = "wasm"))]
                    {
                        out_of_step = b.downloads.playlists_out_of_step(&list.playlist);
                    }
                    let mut changed = b.playlists != list.playlist;
                    b.playlists = list.playlist;
                    if let Some(folders) = folders {
                        changed |= b.playlist_folders != folders.playlist_folder;
                        b.playlist_folders = folders.playlist_folder;
                    }
                    Ok(changed)
                }
                Err(e) => Err(e),
            };
            (changed, known)
        };
        if shown {
            ui.set_loading(false);
        }
        match changed {
            Ok(true) => {
                sidebar(&ui, &browser);
                if shown {
                    show_playlists(&ui, &browser);
                }
                playlist_moves(&ui, &browser.borrow());
            }
            Ok(false) => {}
            // The playlists shown stay when asking again fails.
            Err(e) if shown && !known => ui.set_page_problem(e.to_string().into()),
            Err(_) => {}
        }
        #[cfg(not(target_family = "wasm"))]
        crate::downloads::follow_playlists(&ui, &browser, out_of_step);
        if reader.gave_aged() {
            ask_playlists(&ui, &browser, false);
        }
    })
    .expect("the event loop is running");
}

/// Shows the playlists on the playlists page that are known, and asks for
/// them again: OpenSubsonic says to refresh playlists on each access.
// ponytail: ignores validUntil, which only allows fewer requests; read it if they cost.
fn playlists(ui: &AppWindow, browser: &Shared) {
    let (loaded, loading) = {
        let b = browser.borrow();
        (b.playlists_loaded, b.playlists_loading)
    };
    if loaded {
        show_playlists(ui, browser);
    } else {
        folder_heading(ui, &browser.borrow());
        ui.set_loading(true);
    }
    if !loading {
        load_playlists(ui, browser);
    }
}

/// Puts the playlists on the playlists page, in place of what it showed: on
/// servers with folders those at the top level or in the folder on screen,
/// with the folders there.
// ponytail: all tiles at once; page them like the albums if someone has thousands.
fn show_playlists(ui: &AppWindow, browser: &Shared) {
    let folder = ui.get_shown_id();
    let tiles = {
        let mut b = browser.borrow_mut();
        b.tiles = Rc::default();
        b.covers.clear();
        b.fetching = 0;
        ui.set_tiles(ModelRc::from(b.tiles.clone()));
        let folders = folders_of(&b);
        let folder = Some(folder.as_str()).filter(|folder| !folder.is_empty());
        let moves = match folders {
            Some(folders) if !folders.is_empty() => {
                destinations(&folder_nodes(folders), None, folder)
            }
            _ => Vec::new(),
        };
        ui.set_tile_moves(ModelRc::new(VecModel::from(moves)));
        let tiles = placed(&b.playlists, folders, folder).into_iter();
        tiles
            .map(|item| match item {
                Placed::Folder(folder) => {
                    let inside = placed(&b.playlists, folders, Some(&folder.id)).len();
                    folder_tile(folder, inside)
                }
                Placed::Playlist(playlist) => playlist_tile(playlist),
            })
            .collect()
    };
    folder_heading(ui, &browser.borrow());
    add_tiles(ui, browser, tiles);
}

/// The playlists page's heading: the folder's name in a folder, and where
/// that can be moved.
fn folder_heading(ui: &AppWindow, b: &Browser) {
    let id = ui.get_shown_id();
    if id.is_empty() {
        ui.set_header_title("Playlists".into());
        return;
    }
    let nodes = folder_nodes(&b.playlist_folders);
    let Some(&(_, name, parent)) = nodes.iter().find(|node| node.0 == id.as_str()) else {
        return;
    };
    ui.set_header_title(name.into());
    let targets = destinations(&nodes, Some(&id), parent);
    ui.set_move_targets(ModelRc::new(VecModel::from(targets)));
}

/// On a playlist's page, on servers with playlist folders, where it can be
/// moved.
fn playlist_moves(ui: &AppWindow, b: &Browser) {
    if ui.get_page() != Page::Playlist {
        return;
    }
    let (Some(folders), Some(id)) = (folders_of(b), playlist_on_screen(b)) else {
        return;
    };
    let nodes = folder_nodes(folders);
    let playlist = b.playlists.iter().find(|playlist| playlist.id == id);
    let folder = playlist.and_then(|playlist| playlist.playlist_folder_id.as_deref());
    let folder = folder.filter(|folder| nodes.iter().any(|node| node.0 == *folder));
    let targets = destinations(&nodes, None, folder);
    ui.set_move_targets(ModelRc::new(VecModel::from(targets)));
}

/// A column a table can show: its heading, its width in logical pixels,
/// whether it is shown at first, what it shows of a row's item, and for
/// numbers the value it sorts by; other columns sort by what they show. A
/// count, as the plays, sorts the most first at first, its ties by name.
struct Column<T> {
    title: &'static str,
    width: f32,
    shown: bool,
    cell: fn(&T) -> Vec<Name>,
    number: Option<fn(&T) -> Option<f64>>,
    is_count: bool,
}

const fn column<T>(title: &'static str, width: f32, cell: fn(&T) -> Vec<Name>) -> Column<T> {
    Column {
        title,
        width,
        shown: false,
        cell,
        number: None,
        is_count: false,
    }
}

const fn numeric<T>(
    title: &'static str,
    width: f32,
    cell: fn(&T) -> Vec<Name>,
    number: fn(&T) -> Option<f64>,
) -> Column<T> {
    Column {
        number: Some(number),
        ..column(title, width, cell)
    }
}

/// A column of how often something happened.
const fn tally<T>(
    title: &'static str,
    width: f32,
    cell: fn(&T) -> Vec<Name>,
    number: fn(&T) -> Option<f64>,
) -> Column<T> {
    Column {
        is_count: true,
        ..numeric(title, width, cell, number)
    }
}

/// Which of a table's columns it shows, the one it is sorted by and whether
/// that is turned around.
#[derive(Default)]
struct Table {
    shown: Vec<bool>,
    sort: Option<usize>,
    descending: bool,
}

impl Table {
    /// The indexes of the columns shown.
    fn shown(&self) -> Vec<usize> {
        (0..self.shown.len()).filter(|&c| self.shown[c]).collect()
    }

    /// The table of its first `count` columns only.
    fn first(&self, count: usize) -> Table {
        Table {
            shown: self.shown.iter().copied().take(count).collect(),
            sort: self.sort.filter(|&c| c < count),
            descending: self.descending,
        }
    }
}

/// The compositions table as the server can fill it: without the plays on a
/// server that doesn't count them. The table picked keeps them, for a server
/// that does.
fn piece_table(b: &Browser) -> Table {
    offered_piece_table(&b.piece_table, counts_composition_plays(b))
}

/// `table` of `PIECE_COLUMNS` without its last column, the plays, unless
/// the server `counts_plays`.
fn offered_piece_table(table: &Table, counts_plays: bool) -> Table {
    match counts_plays {
        true => table.first(PIECE_COLUMNS.len()),
        false => table.first(PIECE_COLUMNS.len() - 1),
    }
}

/// The songs a table of `COLUMNS` on `page` lists and their order there: the
/// folder's or tag's entries on the folders or a tag's page, else every song.
fn listed(b: &Browser, page: Page) -> (&[Child], &[usize]) {
    if matches!(page, Page::Folders | Page::Tag) {
        (&b.entries, &b.view)
    } else {
        (&b.song_list, &b.song_view)
    }
}

/// The table of `COLUMNS` on `page`: the folders', else the tracks page's.
fn song_table(b: &Browser, page: Page) -> &Table {
    if page == Page::Folders {
        &b.entry_table
    } else {
        &b.track_table
    }
}

/// The table on `page`, to change.
fn table_mut(b: &mut Browser, page: Page) -> &mut Table {
    match page {
        Page::Compositions => &mut b.piece_table,
        Page::Folders => &mut b.entry_table,
        _ => &mut b.track_table,
    }
}

/// The column with the cover, the ones that take the room left over, and the
/// one with the heart.
const COVER: &str = "Cover";
const TITLE: &str = "Title";
const COMPOSITION: &str = "Composition";
const LOVED: &str = "Loved";

/// The tracks table's columns: one for every field of a song in OpenSubsonic
/// but the ids, which its links use.
#[rustfmt::skip]
const COLUMNS: &[Column<Child>] = &[
    Column { shown: true, ..column(COVER, LINE_COVER_SIZE, |_| Vec::new()) },
    Column { shown: true, ..column(TITLE, 240.0, |song| vec![Name { label: song.title.as_str().into(), link: song_link(song).into() }]) },
    Column { shown: true, ..column("Artists", 180.0, song_names) },
    column("Display artist", 180.0, |song| text_of(&song.display_artist)),
    Column { shown: true, ..column("Album", 200.0, |song| album_name(song).into_iter().collect()) },
    column("Album artists", 180.0, |song| song.album_artists.iter().map(linked).collect()),
    column("Display album artist", 180.0, |song| text_of(&song.display_album_artist)),
    column("Contributors", 240.0, contributor_names),
    column("Composer", 180.0, |song| text_of(&song.display_composer)),
    numeric("Disc", 48.0, |song| number(song.disc_number), |song| float(song.disc_number)),
    numeric("Track", 56.0, |song| number(song.track), |song| float(song.track)),
    numeric("Year", 56.0, |song| number(song.year), |song| float(song.year)),
    column("Genre", 120.0, |song| text_of(&song.genre)),
    column("Genres", 160.0, |song| text(&song.genres.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(", "))),
    Column { shown: true, ..numeric("Duration", 72.0, |song| text(&song.duration.map(duration).unwrap_or_default()), |song| float(song.duration)) },
    Column { shown: true, ..numeric(LOVED, 40.0, |_| Vec::new(), |song| Some(if song.starred.is_some() { 1.0 } else { 0.0 })) },
    column("Starred", 100.0, |song| day(&song.starred)),
    numeric("Rating", 64.0, |song| number(song.user_rating), |song| float(song.user_rating)),
    numeric("Average rating", 72.0, |song| text(&song.average_rating.map(|r| format!("{r:.1}")).unwrap_or_default()), |song| song.average_rating),
    tally("Plays", 56.0, |song| number(song.play_count), |song| float(Some(song.play_count.unwrap_or(0)))),
    column("Last played", 100.0, |song| day(&song.played)),
    column("Added", 100.0, |song| day(&song.created)),
    numeric("BPM", 56.0, |song| number(song.bpm), |song| float(song.bpm)),
    column("Comment", 240.0, |song| text_of(&song.comment)),
    column("Sort name", 180.0, |song| text_of(&song.sort_name)),
    column("Moods", 160.0, |song| text(&song.moods.join(", "))),
    column("Groupings", 160.0, |song| text(&song.groupings.join(", "))),
    column("Works", 200.0, |song| text(&song.works.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join(", "))),
    column("Movements", 200.0, |song| text(&song.movements.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", "))),
    column("Explicit", 72.0, |song| text(match song.explicit_status {
        Some(ExplicitStatus::Explicit) => "Explicit",
        Some(ExplicitStatus::Clean) => "Clean",
        _ => "",
    })),
    column("ISRC", 140.0, |song| text(&song.isrc.join(", "))),
    column("MusicBrainz ID", 290.0, |song| text_of(&song.music_brainz_id)),
    numeric("Track gain", 88.0, |song| gain(song.replay_gain.as_ref().and_then(|g| g.track_gain)), |song| song.replay_gain.as_ref().and_then(|g| g.track_gain)),
    numeric("Track peak", 80.0, |song| peak(song.replay_gain.as_ref().and_then(|g| g.track_peak)), |song| song.replay_gain.as_ref().and_then(|g| g.track_peak)),
    numeric("Album gain", 88.0, |song| gain(song.replay_gain.as_ref().and_then(|g| g.album_gain)), |song| song.replay_gain.as_ref().and_then(|g| g.album_gain)),
    numeric("Album peak", 80.0, |song| peak(song.replay_gain.as_ref().and_then(|g| g.album_peak)), |song| song.replay_gain.as_ref().and_then(|g| g.album_peak)),
    numeric("Base gain", 88.0, |song| gain(song.replay_gain.as_ref().and_then(|g| g.base_gain)), |song| song.replay_gain.as_ref().and_then(|g| g.base_gain)),
    numeric("Fallback gain", 104.0, |song| gain(song.replay_gain.as_ref().and_then(|g| g.fallback_gain)), |song| song.replay_gain.as_ref().and_then(|g| g.fallback_gain)),
    column("Format", 64.0, |song| text_of(&song.suffix)),
    column("Content type", 120.0, |song| text_of(&song.content_type)),
    numeric("Bit rate", 80.0, |song| text(&song.bit_rate.map(|r| format!("{r} kbps")).unwrap_or_default()), |song| float(song.bit_rate)),
    numeric("Bit depth", 64.0, |song| text(&song.bit_depth.map(|d| format!("{d}-bit")).unwrap_or_default()), |song| float(song.bit_depth)),
    numeric("Sample rate", 88.0, |song| text(&song.sampling_rate.map(|r| format!("{} kHz", r as f64 / 1000.0)).unwrap_or_default()), |song| float(song.sampling_rate)),
    numeric("Channels", 72.0, |song| number(song.channel_count), |song| float(song.channel_count)),
    numeric("Size", 80.0, |song| text(&song.size.map(|n| format!("{:.1} MB", n as f64 / 1_048_576.0)).unwrap_or_default()), |song| float(song.size)),
    column("Path", 360.0, |song| text_of(&song.path)),
    column("Transcoded format", 120.0, |song| text_of(&song.transcoded_suffix)),
    column("Transcoded type", 130.0, |song| text_of(&song.transcoded_content_type)),
    column("Type", 80.0, |song| text(match song.r#type {
        Some(GenericMediaType::Music) => "Music",
        Some(GenericMediaType::Video) => "Video",
        Some(GenericMediaType::Podcast) => "Podcast",
        Some(GenericMediaType::Audiobook) => "Audiobook",
        _ => "",
    })),
    column("Media type", 88.0, |song| text(match song.media_type {
        Some(MediaType::Song) => "Song",
        Some(MediaType::Album) => "Album",
        Some(MediaType::Artist) => "Artist",
        _ => "",
    })),
    column("Video", 56.0, |song| text(if song.is_video == Some(true) { "Yes" } else { "" })),
    numeric("Width", 64.0, |song| number(song.original_width), |song| float(song.original_width)),
    numeric("Height", 64.0, |song| number(song.original_height), |song| float(song.original_height)),
    numeric("Bookmark", 80.0, |song| text(&song.bookmark_position.map(duration).unwrap_or_default()), |song| float(song.bookmark_position)),
];

/// The compositions table's columns, as Roon's: one for every field of a
/// composition in Fonoteca's compositions but the ids. Dates sort by the
/// year it was begun, periods by when they were. The plays come last, so
/// that a server without them leaves the others in their places.
#[rustfmt::skip]
const PIECE_COLUMNS: &[Column<Composition>] = &[
    Column { shown: true, ..column(COVER, LINE_COVER_SIZE, |_| Vec::new()) },
    Column { shown: true, ..column(COMPOSITION, 200.0, |piece| vec![Name { label: piece.name.as_str().into(), link: piece_link(piece).into() }]) },
    Column { shown: true, ..column("Composer", 170.0, |piece| piece.composers.iter().map(composer_name).collect()) },
    Column { shown: true, ..numeric("Date", 88.0, |piece| text(&years(piece.composed.as_ref())), |piece| {
        let span = piece.composed.as_ref()?;
        float(span.from.as_ref().or(span.to.as_ref())?.year)
    }) },
    Column { shown: true, ..numeric("Period", 96.0, |piece| text_of(&piece.period), |piece| {
        let period = piece.period.as_deref()?;
        PERIODS.iter().position(|known| *known == period).map(|at| at as f64)
    }) },
    Column { shown: true, ..column("Form", 104.0, |piece| text_of(&piece.form)) },
    column("Key", 104.0, |piece| text_of(&piece.key)),
    numeric("Movements", 88.0, |piece| number(piece.movement_count), |piece| float(piece.movement_count)),
    numeric("Duration", 72.0, |piece| text(&piece.duration.map(duration).unwrap_or_default()), |piece| float(piece.duration)),
    column("Lyricists", 170.0, |piece| piece.lyricists.iter().map(lyricist_name).collect()),
    column("Languages", 120.0, |piece| text(&piece.languages.iter().map(|code| language(code)).collect::<Vec<_>>().join(", "))),
    Column { shown: true, ..numeric("Performances", 96.0, |piece| number(piece.album_count), |piece| float(piece.album_count)) },
    Column { shown: true, ..numeric(LOVED, 40.0, |_| Vec::new(), |piece| Some(if piece.starred.is_some() { 1.0 } else { 0.0 })) },
    column("Starred", 100.0, |piece| day(&piece.starred)),
    column("MusicBrainz ID", 290.0, |piece| text_of(&piece.music_brainz_id)),
    tally("Plays", 56.0, |piece| number(piece.play_count), |piece| float(Some(piece.play_count.unwrap_or(0)))),
];

/// A cell of one plain value, or an empty one.
fn text(value: &str) -> Vec<Name> {
    if value.is_empty() {
        Vec::new()
    } else {
        vec![unlinked(value.to_owned())]
    }
}

fn text_of(value: &Option<String>) -> Vec<Name> {
    text(value.as_deref().unwrap_or_default())
}

fn number(value: Option<i64>) -> Vec<Name> {
    value.map(|n| unlinked(n.to_string())).into_iter().collect()
}

fn float(value: Option<i64>) -> Option<f64> {
    value.map(|n| n as f64)
}

/// The day of an ISO 8601 date and time.
fn day(value: &Option<String>) -> Vec<Name> {
    text_of(&value.as_ref().map(|value| value.chars().take(10).collect()))
}

fn gain(gain: Option<f64>) -> Vec<Name> {
    text(&gain.map(|gain| format!("{gain:.2} dB")).unwrap_or_default())
}

fn peak(peak: Option<f64>) -> Vec<Name> {
    text(&peak.map(|peak| format!("{peak:.3}")).unwrap_or_default())
}

/// Everyone credited on a song, with what they did: "Anna Bach (performer,
/// piano)".
fn contributor_names(song: &Child) -> Vec<Name> {
    song.contributors
        .iter()
        .map(|contributor| {
            let role = match &contributor.sub_role {
                Some(sub) if !sub.is_empty() => format!("{}, {sub}", contributor.role),
                _ => contributor.role.clone(),
            };
            Name {
                label: format!("{} ({role})", contributor.artist.name).into(),
                ..linked(&contributor.artist)
            }
        })
        .collect()
}

/// The album of a song, linked when the server gives its id.
fn album_name(song: &Child) -> Option<Name> {
    let album = song.album.as_ref()?;
    let link = song.album_id.as_ref().map(|id| format!("album:{id}"));
    Some(Name {
        label: album.as_str().into(),
        link: link.unwrap_or_default().into(),
    })
}

/// Shows the songs on the tracks page in the columns and order picked, a
/// page of rows at a time, asking for them first if they aren't all there.
fn tracks(ui: &AppWindow, browser: &Shared) {
    show_columns(ui, browser);
    let ask = {
        let b = browser.borrow();
        !b.song_list_loaded && !b.song_list_loading
    };
    if ask {
        load_songs(ui, browser);
    }
    recount_songs(ui, browser);
    arrange_tracks(ui, browser);
}

/// The table's columns, the one it is sorted by, and the choices: the
/// compositions table's on that page, the folders' on theirs, else the
/// tracks table's.
fn show_columns(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    if ui.get_page() == Page::Compositions {
        set_columns(ui, PIECE_COLUMNS, &piece_table(&b));
    } else {
        set_columns(ui, COLUMNS, song_table(&b, ui.get_page()));
    }
}

/// How narrow a column `width` wide gets on a window short of room: to three
/// fifths, but not below what a short value such as a date needs.
fn narrowest(width: f32) -> f32 {
    (width * 0.6).max(width.min(96.0))
}

fn set_columns<T>(ui: &AppWindow, all: &[Column<T>], table: &Table) {
    let columns: Vec<TrackColumn> = table
        .shown()
        .into_iter()
        .map(|c| TrackColumn {
            title: all[c].title.into(),
            width: all[c].width,
            least: narrowest(all[c].width),
            stretch: [TITLE, COMPOSITION].contains(&all[c].title),
            heart: all[c].title == LOVED,
            cover: all[c].title == COVER,
            sorted: table.sort == Some(c),
            descending: table.descending,
        })
        .collect();
    // The columns, the menu button, the gaps between them, and the row's
    // padding; at the columns' widths, and at their narrowest.
    let around = 32.0 + 12.0 * columns.len() as f32 + 16.0;
    let width = columns.iter().map(|column| column.width).sum::<f32>() + around;
    let least = columns.iter().map(|column| column.least).sum::<f32>() + around;
    let choices: Vec<ColumnChoice> = all
        .iter()
        .zip(&table.shown)
        .map(|(column, &shown)| ColumnChoice {
            title: column.title.into(),
            shown,
        })
        .collect();
    ui.set_track_columns(ModelRc::new(VecModel::from(columns)));
    ui.set_track_table_width(width);
    ui.set_track_table_least(least);
    ui.set_track_choices(ModelRc::new(VecModel::from(choices)));
}

/// The tracks table's row for a song, with its cover if it was fetched
/// before at `size`, and whether it is selected.
fn track_line(b: &Browser, song: &Child, columns: &[usize], size: u32) -> TrackLine {
    let cells: Vec<ModelRc<Name>> = columns
        .iter()
        .map(|&c| names((COLUMNS[c].cell)(song)))
        .collect();
    let cover = song
        .cover_art
        .as_ref()
        .and_then(|cover| b.pictures.get(&(cover.clone(), size)).cloned());
    let link = song_link(song);
    let album = album_name(song).filter(|album| !album.link.is_empty());
    let artists: Vec<Name> = song_names(song)
        .into_iter()
        .filter(|artist| !artist.link.is_empty())
        .collect();
    TrackLine {
        title: song.title.as_str().into(),
        cells: ModelRc::new(VecModel::from(cells)),
        loved: song.starred.is_some(),
        cover: cover.unwrap_or_default(),
        folder: song.is_dir,
        selected: b.selected.contains(&link),
        album: album.unwrap_or_default(),
        artists: names(artists),
        downloaded: song.is_dir && b.downloads.is_kept(&link),
        link: link.into(),
    }
}

/// The compositions table's row for a composition, as `track_line` for a
/// song, its composers in place of the artists and no album.
fn piece_line(b: &Browser, piece: &Composition, columns: &[usize], size: u32) -> TrackLine {
    let cells: Vec<ModelRc<Name>> = columns
        .iter()
        .map(|&c| names((PIECE_COLUMNS[c].cell)(piece)))
        .collect();
    let cover = piece
        .cover_art
        .as_ref()
        .and_then(|cover| b.pictures.get(&(cover.clone(), size)).cloned());
    let composers: Vec<Name> = piece
        .composers
        .iter()
        .map(composer_name)
        .filter(|composer| !composer.link.is_empty())
        .collect();
    let link = piece_link(piece);
    TrackLine {
        title: piece.name.as_str().into(),
        cells: ModelRc::new(VecModel::from(cells)),
        loved: piece.starred.is_some(),
        cover: cover.unwrap_or_default(),
        folder: false,
        selected: b.selected.contains(&link),
        album: Name::default(),
        artists: names(composers),
        downloaded: false,
        link: link.into(),
    }
}

/// The periods Fonoteca puts compositions in, in order.
const PERIODS: &[&str] = &[
    "Medieval",
    "Renaissance",
    "Baroque",
    "Classical",
    "Romantic",
    "Modern",
    "Contemporary",
];

/// The English name of the languages words are mostly sung in, by their ISO
/// 639-3 code; others keep the code.
// ponytail: a short table; the full ISO 639-3 list when other codes turn up.
fn language(code: &str) -> &str {
    const NAMES: &[(&str, &str)] = &[
        ("ang", "Old English"),
        ("ces", "Czech"),
        ("cre", "Cree"),
        ("dan", "Danish"),
        ("deu", "German"),
        ("ell", "Greek"),
        ("eng", "English"),
        ("fin", "Finnish"),
        ("fra", "French"),
        ("heb", "Hebrew"),
        ("hun", "Hungarian"),
        ("ita", "Italian"),
        ("jpn", "Japanese"),
        ("kat", "Georgian"),
        ("lat", "Latin"),
        ("mul", "Multiple languages"),
        ("nld", "Dutch"),
        ("nor", "Norwegian"),
        ("pol", "Polish"),
        ("por", "Portuguese"),
        ("qaa", "Artificial"),
        ("qya", "Quenya"),
        ("rus", "Russian"),
        ("san", "Sanskrit"),
        ("sjn", "Sindarin"),
        ("smo", "Samoan"),
        ("spa", "Spanish"),
        ("swe", "Swedish"),
        ("tkl", "Tokelauan"),
        ("ukr", "Ukrainian"),
        ("zho", "Chinese"),
        ("zul", "Zulu"),
        ("zxx", "No words"),
    ];
    NAMES
        .iter()
        .find(|(known, _)| *known == code)
        .map_or(code, |(_, name)| name)
}

/// A lyricist's name, linked to their artist page where they have one.
fn lyricist_name(lyricist: &Composer) -> Name {
    let link = if lyricist.id.is_empty() {
        String::new()
    } else {
        format!("artist:{}", lyricist.id)
    };
    Name {
        label: lyricist.name.as_str().into(),
        link: link.into(),
    }
}

/// A composition's page, which also stands for it in the selection.
fn piece_link(piece: &Composition) -> String {
    format!("composition:{}", piece.id)
}

/// Lists the covers still to fetch for the rows of the table on `page` from
/// `start` on, when it shows them.
fn want_line_covers(b: &mut Browser, page: Page, start: usize) {
    let pieces = page == Page::Compositions;
    let shown = if pieces {
        shows_cover(PIECE_COLUMNS, &piece_table(b))
    } else {
        shows_cover(COLUMNS, song_table(b, page))
    };
    if !shown {
        return;
    }
    for row in start..b.lines.row_count() {
        let cover = if pieces {
            b.compositions[b.view[row]].cover_art.clone()
        } else {
            let (songs, order) = listed(b, page);
            songs[order[row]].cover_art.clone()
        };
        let missing = b
            .lines
            .row_data(row)
            .is_some_and(|line| line.cover.size().width == 0);
        if let (true, Some(cover)) = (missing, cover) {
            b.covers.insert(row, cover);
        }
    }
}

fn shows_cover<T>(all: &[Column<T>], table: &Table) -> bool {
    all.iter()
        .zip(&table.shown)
        .any(|(column, &shown)| shown && column.title == COVER)
}

/// Starts the tracks table's rows over, dropping the covers they waited for.
fn clear_lines(ui: &AppWindow, b: &mut Browser) {
    b.lines = Rc::default();
    b.covers.clear();
    b.fetching = 0;
    ui.set_track_lines(ModelRc::from(b.lines.clone()));
}

/// Empties the page's tiles, for others to take their place.
fn clear_tiles(ui: &AppWindow, b: &mut Browser) {
    b.tiles = Rc::default();
    b.covers.clear();
    b.fetching = 0;
    ui.set_tiles(ModelRc::from(b.tiles.clone()));
}

/// Asks for every song, a part at a time, with the empty search that
/// OpenSubsonic servers answer with all of them, and arranges the tracks page
/// as they arrive. Sorting then needs nothing from the server.
fn load_songs(ui: &AppWindow, browser: &Shared) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    browser.borrow_mut().song_list_loading = true;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        loop {
            let offset = browser.borrow().song_list.len();
            let answer = songs_from(&client, offset).await;
            let Some(ui) = weak.upgrade() else { return };
            let signed_in = browser
                .borrow()
                .client
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &client));
            if !signed_in {
                return;
            }
            let shown = ui.get_page() == Page::Tracks;
            let songs = match answer {
                Ok(songs) => songs,
                Err(e) => {
                    browser.borrow_mut().song_list_loading = false;
                    if shown {
                        ui.set_loading(false);
                        ui.set_page_problem(e.to_string().into());
                    }
                    return;
                }
            };
            let done = {
                let mut b = browser.borrow_mut();
                // Only an empty part is the end: the spec puts no limit on
                // `songCount`, so a server may send fewer than asked for.
                let done = songs.is_empty() || b.song_list.len() + songs.len() >= SONGS_AT_MOST;
                b.song_list.extend(songs);
                b.song_list_loading = !done;
                b.song_list_loaded = done;
                done
            };
            if shown {
                arrange_tracks(&ui, &browser);
            }
            if done {
                return;
            }
        }
    })
    .expect("the event loop is running");
}

/// The songs from `offset` on, a part of the empty search that OpenSubsonic
/// servers answer with all of them.
async fn songs_from(client: &Client, offset: usize) -> opensubsonic::Result<Vec<Child>> {
    let options = SearchOptions {
        artist_count: Some(0),
        album_count: Some(0),
        song_count: Some(LIBRARY_AT_ONCE),
        song_offset: Some(offset as u32),
        ..SearchOptions::default()
    };
    Ok(client.search3("", &options).await?.song)
}

/// Asks for every song again when the server was told of a play since they
/// were asked for and the tracks table shows or sorts by the plays. The
/// songs shown stay until all have come, and then take their place if any
/// differ.
fn recount_songs(ui: &AppWindow, browser: &Shared) {
    let client = {
        let mut b = browser.borrow_mut();
        let table = &b.track_table;
        let uses_plays = (table.shown().into_iter().chain(table.sort)).any(|c| COLUMNS[c].is_count);
        if !b.song_list_misses_plays || !b.song_list_loaded || !uses_plays {
            return;
        }
        b.song_list_misses_plays = false;
        b.client.clone()
    };
    let Some(client) = client else { return };
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let mut songs = Vec::new();
        loop {
            let part = songs_from(&client, songs.len()).await;
            let Some(ui) = weak.upgrade() else { return };
            let b = browser.borrow();
            let signed_in =
                (b.client.as_ref()).is_some_and(|current| Arc::ptr_eq(current, &client));
            // Signed out, or asked for anew from scratch meanwhile.
            if !signed_in || !b.song_list_loaded {
                return;
            }
            drop(b);
            let part = match part {
                Ok(part) => part,
                Err(e) => {
                    eprintln!("The songs' plays couldn't be asked for again: {e}");
                    browser.borrow_mut().song_list_misses_plays = true;
                    return;
                }
            };
            let done = part.is_empty() || songs.len() + part.len() >= SONGS_AT_MOST;
            songs.extend(part);
            if done {
                return take_recounted(&ui, &browser, songs);
            }
        }
    })
    .expect("the event loop is running");
}

/// Puts the songs asked for again in the place of those there, if any differ.
fn take_recounted(ui: &AppWindow, browser: &Shared, songs: Vec<Child>) {
    {
        let mut b = browser.borrow_mut();
        if b.song_list == songs {
            return;
        }
        // The order indexes into them; the page sorts anew.
        b.song_list = songs;
        b.song_view.clear();
    }
    if ui.get_page() == Page::Tracks {
        arrange_tracks(ui, browser);
    }
}

/// Puts the songs of the tracks, folders or a tag's page in its table's order, with
/// its filter, showing as many rows as before and at least a page.
fn arrange_tracks(ui: &AppWindow, browser: &Shared) {
    let page = ui.get_page();
    let filter = typed_filter(ui).unwrap_or_default();
    let shown = {
        let mut b = browser.borrow_mut();
        let (songs, order) = listed(&b, page);
        let found = filtered(songs, &filter, song_text);
        let view = sorted(songs, found, COLUMNS, song_table(&b, page));
        let shown = b.lines.row_count();
        let kept = view.get(..shown) == order.get(..shown);
        b.filter = filter;
        if !kept {
            clear_lines(ui, &mut b);
        }
        if matches!(page, Page::Folders | Page::Tag) {
            b.view = view;
        } else {
            b.song_view = view;
        }
        shown
    };
    show_lines(ui, browser, shown.max(TILES_AT_ONCE));
}

/// Shows the tracks or folders page's rows up to the `upto`th.
fn show_lines(ui: &AppWindow, browser: &Shared, upto: usize) {
    let page = ui.get_page();
    let size = pixels(ui, LINE_COVER_SIZE);
    let (more, waiting) = {
        let mut b = browser.borrow_mut();
        let (songs, order) = listed(&b, page);
        let end = upto.min(order.len());
        let start = b.lines.row_count().min(end);
        let columns = song_table(&b, page).shown();
        for &song in &order[start..end] {
            b.lines.push(track_line(&b, &songs[song], &columns, size));
        }
        // The folder's entries arrive at once, the songs a part at a time.
        let tracks = page == Page::Tracks;
        let more = end < order.len() || (tracks && !b.song_list_loaded);
        let waiting = tracks && b.song_list_loading && b.song_view.is_empty();
        ui.set_songs_listed(order.iter().any(|&song| !songs[song].is_dir));
        want_line_covers(&mut b, page, start);
        (more, waiting)
    };
    ui.set_more_tiles(more);
    ui.set_loading(waiting);
    fetch_covers(ui, browser);
}

/// `view`, indexes into `items`, in the table's order: by the column of
/// `all` it is sorted by, with the items that have nothing in it last either
/// way. Equal items, and all of them when it isn't sorted, keep their order,
/// but equal counts go by name.
fn sorted<T>(items: &[T], mut view: Vec<usize>, all: &[Column<T>], table: &Table) -> Vec<usize> {
    let Some(column) = table.sort.and_then(|c| all.get(c)) else {
        return view;
    };
    let turn = |order: Ordering| {
        if table.descending {
            order.reverse()
        } else {
            order
        }
    };
    let name = all.iter().find(|c| [TITLE, COMPOSITION].contains(&c.title));
    if let Some(name) = name.filter(|_| column.is_count) {
        let names = texts(items, name);
        view.sort_by(|&a, &b| last_if_empty(&names[a], &names[b], Ord::cmp));
    }
    if let Some(number) = column.number {
        let keys: Vec<Option<f64>> = items.iter().map(number).collect();
        view.sort_by(|&a, &b| last_if_empty(&keys[a], &keys[b], |a, b| turn(a.total_cmp(b))));
    } else {
        let keys = texts(items, column);
        view.sort_by(|&a, &b| last_if_empty(&keys[a], &keys[b], |a, b| turn(a.cmp(b))));
    }
    view
}

/// What each of `items` shows in `column`, to sort by: lower case, and none
/// for nothing.
fn texts<T>(items: &[T], column: &Column<T>) -> Vec<Option<String>> {
    items
        .iter()
        .map(|item| {
            let labels: Vec<String> = (column.cell)(item)
                .iter()
                .map(|name| name.label.to_lowercase())
                .collect();
            Some(labels.join(", ")).filter(|text| !text.is_empty())
        })
        .collect()
}

fn last_if_empty<T>(a: &Option<T>, b: &Option<T>, order: impl Fn(&T, &T) -> Ordering) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => order(a, b),
        _ => a.is_none().cmp(&b.is_none()),
    }
}

/// Sorts the tracks or compositions table by its `index`th column shown, or
/// turns the order around when it already is.
fn sort_tracks(ui: &AppWindow, browser: &Shared, index: usize) {
    let pieces = ui.get_page() == Page::Compositions;
    {
        let mut b = browser.borrow_mut();
        let table = table_mut(&mut b, ui.get_page());
        let Some(&column) = table.shown().get(index) else {
            return;
        };
        if table.sort == Some(column) {
            table.descending = !table.descending;
        } else {
            table.sort = Some(column);
            table.descending = match pieces {
                true => PIECE_COLUMNS[column].is_count,
                false => COLUMNS[column].is_count,
            };
        }
        // From the top, where the page scrolls to.
        clear_lines(ui, &mut b);
    }
    save_prefs(ui, browser);
    show_columns(ui, browser);
    if pieces {
        arrange(ui, browser);
    } else {
        arrange_tracks(ui, browser);
    }
}

/// Shows the column at `index` into `COLUMNS` or `PIECE_COLUMNS` of the
/// tracks, folders or compositions table, or hides it.
fn toggle_column(ui: &AppWindow, browser: &Shared, index: usize) {
    let size = pixels(ui, LINE_COVER_SIZE);
    let page = ui.get_page();
    {
        let mut b = browser.borrow_mut();
        let table = table_mut(&mut b, page);
        let Some(shown) = table.shown.get_mut(index) else {
            return;
        };
        *shown = !*shown;
        let columns = match page {
            Page::Compositions => piece_table(&b).shown(),
            _ => table.shown(),
        };
        let rows = b.lines.row_count();
        let lines: Vec<TrackLine> = if page == Page::Compositions {
            b.view[..rows]
                .iter()
                .map(|&i| piece_line(&b, &b.compositions[i], &columns, size))
                .collect()
        } else {
            let (songs, order) = listed(&b, page);
            order[..rows]
                .iter()
                .map(|&song| track_line(&b, &songs[song], &columns, size))
                .collect()
        };
        b.lines.set_vec(lines);
        want_line_covers(&mut b, page, 0);
    }
    save_prefs(ui, browser);
    show_columns(ui, browser);
    fetch_covers(ui, browser);
}

/// Picks up the preferences saved on this device, on sign-in.
fn load_prefs(ui: &AppWindow, browser: &Shared) {
    let prefs = prefs::load();
    let mut b = browser.borrow_mut();
    b.languages = prefs.radio_languages.clone();
    b.track_table = saved_table(
        COLUMNS,
        &prefs.track_columns,
        &prefs.track_sort,
        prefs.track_descending,
    );
    b.piece_table = saved_table(
        PIECE_COLUMNS,
        &prefs.composition_columns,
        &prefs.composition_sort,
        prefs.composition_descending,
    );
    b.entry_table = saved_table(
        COLUMNS,
        &prefs.folder_columns,
        &prefs.folder_sort,
        prefs.folder_descending,
    );
    // Each Sort menu has the most played last.
    ui.set_album_sort(prefs.album_sort.clamp(0, MOST_PLAYED_ALBUMS));
    ui.set_album_descending(prefs.album_descending);
    ui.set_artist_sort(prefs.artist_sort.clamp(0, MOST_PLAYED_ARTISTS));
    ui.set_artist_descending(prefs.artist_descending);
    ui.set_composer_sort(prefs.composer_sort.clamp(0, MOST_PLAYED_ARTISTS));
    ui.set_composer_descending(prefs.composer_descending);
}

/// A table as saved: the columns of `all` with the headings `shown`, or
/// those shown at first when none are saved, sorted by the one headed `sort`.
fn saved_table<T>(
    all: &[Column<T>],
    shown: &Option<Vec<String>>,
    sort: &Option<String>,
    descending: bool,
) -> Table {
    Table {
        shown: all
            .iter()
            .map(|column| match shown {
                Some(titles) => titles.iter().any(|title| title == column.title),
                None => column.shown,
            })
            .collect(),
        sort: sort
            .as_ref()
            .and_then(|title| all.iter().position(|column| column.title == title)),
        descending,
    }
}

/// Saves the order of the albums, artists, composers, tracks, compositions
/// and folders and the tables' columns on this device.
fn save_prefs(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    let (track_columns, track_sort) = titles(COLUMNS, &b.track_table);
    let (composition_columns, composition_sort) = titles(PIECE_COLUMNS, &b.piece_table);
    let (folder_columns, folder_sort) = titles(COLUMNS, &b.entry_table);
    prefs::save(&prefs::Prefs {
        album_sort: ui.get_album_sort(),
        album_descending: ui.get_album_descending(),
        artist_sort: ui.get_artist_sort(),
        artist_descending: ui.get_artist_descending(),
        composer_sort: ui.get_composer_sort(),
        composer_descending: ui.get_composer_descending(),
        track_columns: Some(track_columns),
        track_sort,
        track_descending: b.track_table.descending,
        composition_columns: Some(composition_columns),
        composition_sort,
        composition_descending: b.piece_table.descending,
        folder_columns: Some(folder_columns),
        folder_sort,
        folder_descending: b.entry_table.descending,
        radio_languages: b.languages.clone(),
        ..prefs::load()
    });
}

/// The headings of the table's columns shown and of the one it is sorted by.
fn titles<T>(all: &[Column<T>], table: &Table) -> (Vec<String>, Option<String>) {
    let title = |c: usize| all[c].title.to_owned();
    (
        table.shown().into_iter().map(title).collect(),
        table.sort.map(title),
    )
}

/// What is typed in the filter of the page on screen, if it has one.
fn typed_filter(ui: &AppWindow) -> Option<String> {
    let typed = match ui.get_page() {
        Page::Albums => ui.get_album_filter(),
        Page::Artists => ui.get_artist_filter(),
        Page::Composers => ui.get_composer_filter(),
        Page::Compositions => ui.get_composition_filter(),
        Page::Tracks => ui.get_track_filter(),
        Page::Folders | Page::Tag => ui.get_folder_filter(),
        _ => return None,
    };
    Some(typed.trim().to_owned())
}

/// Puts the library in the albums page's order, or the artists, composers or
/// compositions in the server's, with the page's filter. The tiles or rows
/// shown stay when they are still the first ones; otherwise as many are shown
/// again, and at least a page. On the home page, shows what has arrived.
fn arrange(ui: &AppWindow, browser: &Shared) {
    match ui.get_page() {
        Page::Home => {
            show_home(ui, browser);
            return created_for::banners(ui, browser);
        }
        Page::Genres => return show_genres(ui, browser),
        Page::Genre => show_genre(ui, browser),
        Page::Search => return search::show_lists(ui, browser),
        _ => {}
    }
    let (sort, descending) = (ui.get_album_sort(), ui.get_album_descending());
    let (artist_sort, artist_descending) = (ui.get_artist_sort(), ui.get_artist_descending());
    let (composer_sort, composer_descending) =
        (ui.get_composer_sort(), ui.get_composer_descending());
    let filter = typed_filter(ui).unwrap_or_default();
    let page = ui.get_page();
    let mut b = browser.borrow_mut();
    let genre = shown_genre(&b);
    let view = match page {
        Page::Artists if !genre.is_empty() => {
            let linked: HashSet<String> = (b.library.iter())
                .filter(|album| carries(album, genre))
                .flat_map(album_names)
                .map(|name| name.link.into())
                .collect();
            let mut view = filtered(&b.artists, &filter, |artist| artist.name.clone());
            view.retain(|&i| linked.contains(&format!("artist:{}", b.artists[i].id)));
            arranged_artists(&b, view, artist_sort, artist_descending)
        }
        Page::Artists => {
            let view = filtered(&b.artists, &filter, |artist| artist.name.clone());
            arranged_artists(&b, view, artist_sort, artist_descending)
        }
        Page::Composers => {
            let view = filtered(&b.composers, &filter, |composer| composer.name.clone());
            arranged_composers(&b, view, composer_sort, composer_descending)
        }
        Page::Compositions => {
            let view = filtered(&b.compositions, &filter, composition_text);
            sorted(&b.compositions, view, PIECE_COLUMNS, &piece_table(&b))
        }
        Page::Genre => {
            let mut view = genre_albums(&b, genre);
            view.truncate(SHELF);
            view
        }
        _ => {
            let mut view = arranged(&b.library, &filter, sort, descending);
            if !genre.is_empty() {
                view.retain(|&i| carries(&b.library[i], genre));
            }
            view
        }
    };
    b.filter = filter;
    let shown = on_page(&b, page);
    if view.get(..shown) != b.view.get(..shown) {
        b.tiles = Rc::default();
        clear_lines(ui, &mut b);
        ui.set_tiles(ModelRc::from(b.tiles.clone()));
    }
    b.view = view;
    drop(b);
    show_tiles(ui, browser, shown.max(TILES_AT_ONCE));
}

/// Asks the server for the page on screen anew with `ask`, as the user asked
/// to. When the server didn't answer the sign-in, signs in again instead, as
/// the note's "Try again" does, which goes on from the page on screen.
fn ask_anew(ui: &AppWindow, browser: &Shared, ask: fn(&AppWindow, &Shared)) {
    match sign_in_failed(browser) {
        true => ui.invoke_reconnect(),
        false => ask(ui, browser),
    }
}

/// Shows the next tiles or rows, and asks for the library, the artists, the
/// songs or the playlists again if that failed, or what the home page lacks.
fn more_tiles(ui: &AppWindow, browser: &Shared) {
    ui.set_page_problem("".into());
    if ui.get_page() == Page::Home {
        return home(ui, browser);
    }
    if ui.get_created_for() {
        return created_for::resume(ui, browser);
    }
    if ui.get_page() == Page::History {
        return show(ui, browser);
    }
    if ui.get_page() == Page::Stations {
        return more_stations(ui, browser);
    }
    if ui.get_page() == Page::Radio {
        stations(ui, browser);
        directory_shelves(ui, browser);
        return catalogue_stations(ui, browser);
    }
    if ui.get_page() == Page::Tags {
        return tags(ui, browser);
    }
    if matches!(ui.get_page(), Page::Genres | Page::Genre) {
        return genres(ui, browser);
    }
    if ui.get_page() == Page::Tracks {
        let (ask, shown) = {
            let b = browser.borrow();
            (
                !b.song_list_loaded && !b.song_list_loading,
                b.lines.row_count(),
            )
        };
        if ask {
            load_songs(ui, browser);
        }
        show_lines(ui, browser, shown + TILES_AT_ONCE);
        return;
    }
    let page = ui.get_page();
    if page == Page::Playlists {
        playlists(ui, browser);
        return;
    }
    if matches!(page, Page::Folders | Page::Tag) {
        let (link, held, shown) = {
            let b = browser.borrow();
            let link = b.history[b.at].clone();
            (link.clone(), b.folder == link, b.lines.row_count())
        };
        if held {
            show_lines(ui, browser, shown + TILES_AT_ONCE);
        } else {
            folders(ui, browser, link);
        }
        return;
    }
    let (have_artists, have_composers, have_compositions, genre) = {
        let b = browser.borrow();
        let genre = !shown_genre(&b).is_empty();
        (
            b.artists_loaded,
            b.composers_loaded,
            b.compositions_loaded,
            genre,
        )
    };
    // A genre's artists are those of its albums.
    let library = match page {
        Page::Artists => genre,
        Page::Composers | Page::Compositions => false,
        _ => true,
    };
    if library {
        want_library(ui, browser);
    }
    match page {
        Page::Artists if !have_artists => return artists(ui, browser),
        Page::Composers if !have_composers => return composers(ui, browser),
        Page::Compositions if !have_compositions => return compositions(ui, browser),
        _ => {}
    }
    let shown = on_page(&browser.borrow(), page);
    show_tiles(ui, browser, shown + TILES_AT_ONCE);
}

/// How many tiles the page shows, or rows on the compositions page.
fn on_page(b: &Browser, page: Page) -> usize {
    if page == Page::Compositions {
        b.lines.row_count()
    } else {
        b.tiles.row_count()
    }
}

/// Shows the page's albums, artists, composers or compositions up to the
/// `upto`th.
fn show_tiles(ui: &AppWindow, browser: &Shared, upto: usize) {
    let page = ui.get_page();
    let (columns, size) = (
        piece_table(&browser.borrow()).shown(),
        pixels(ui, LINE_COVER_SIZE),
    );
    let (tiles, more, waiting) = {
        let mut b = browser.borrow_mut();
        let end = upto.min(b.view.len());
        let start = on_page(&b, page).min(end);
        let mut tiles = Vec::new();
        for &i in &b.view[start..end] {
            match page {
                Page::Artists => tiles.push(artist_tile(&b.artists[i])),
                Page::Composers => tiles.push(composer_tile(&b.composers[i])),
                Page::Compositions => {
                    b.lines
                        .push(piece_line(&b, &b.compositions[i], &columns, size))
                }
                _ => tiles.push(album_tile(&b.library[i], album_names(&b.library[i]))),
            }
        }
        if page == Page::Compositions {
            want_line_covers(&mut b, page, start);
        }
        // The library arrives a part at a time, the others at once; a genre's
        // artists are those of its albums.
        let library = matches!(page, Page::Albums | Page::Genre)
            || (page == Page::Artists && !shown_genre(&b).is_empty());
        let more = end < b.view.len() || (library && !b.library_loaded);
        (
            tiles,
            more,
            library && b.library_loading && b.view.is_empty(),
        )
    };
    ui.set_more_tiles(more);
    ui.set_loading(waiting);
    add_tiles(ui, browser, tiles);
}

/// The albums where every word of `filter` starts a word of their name or
/// artists, as indexes into `library`, in the order of the Sort menu's
/// `sort`. Each order is sorted ascending and turned around for descending,
/// but the most played keep their ties by name either way; an album the
/// server has no count of was played 0 times.
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
        MOST_PLAYED_ALBUMS => {
            let plays = |i: &usize| library[*i].play_count.unwrap_or(0);
            match descending {
                true => view.sort_by_cached_key(|i| (Reverse(plays(i)), name(i))),
                false => view.sort_by_cached_key(|i| (plays(i), name(i))),
            }
            return view;
        }
        _ => view.sort_by_cached_key(name),
    }
    if descending {
        view.reverse();
    }
    view
}

/// `view`, indexes into the artists, in the order of the artists page's Sort
/// menu's `sort`: by name, or by how often their albums were played. The
/// standard has no count of an artist's plays.
fn arranged_artists(b: &Browser, view: Vec<usize>, sort: i32, descending: bool) -> Vec<usize> {
    if sort != MOST_PLAYED_ARTISTS {
        return by_name(view, descending);
    }
    let mut plays: HashMap<String, i64> = HashMap::new();
    for album in &b.library {
        for name in album_names(album) {
            *plays.entry(name.link.into()).or_default() += album.play_count.unwrap_or(0);
        }
    }
    by_plays(view, descending, |i| {
        let link = format!("artist:{}", b.artists[i].id);
        plays.get(&link).copied().unwrap_or(0)
    })
}

/// `view`, indexes into the composers, in the order of their page's Sort
/// menu's `sort`, which has the artists' entries; on a server that doesn't
/// count their plays, which shows no Sort menu there, in the server's.
fn arranged_composers(b: &Browser, view: Vec<usize>, sort: i32, descending: bool) -> Vec<usize> {
    if !counts_composition_plays(b) {
        return view;
    }
    if sort != MOST_PLAYED_ARTISTS {
        return by_name(view, descending);
    }
    by_plays(view, descending, |i| b.composers[i].play_count.unwrap_or(0))
}

/// `view`, indexes into a list the server sends by name, turned around for
/// `descending`.
fn by_name(mut view: Vec<usize>, descending: bool) -> Vec<usize> {
    if descending {
        view.reverse();
    }
    view
}

/// `view`, indexes into a list the server sends by name, by their `plays`,
/// the most first for `descending`, ties by name either way.
fn by_plays(mut view: Vec<usize>, descending: bool, plays: impl Fn(usize) -> i64) -> Vec<usize> {
    // Sorted stably, so ties keep the server's order.
    match descending {
        true => view.sort_by_cached_key(|&i| Reverse(plays(i))),
        false => view.sort_by_cached_key(|&i| plays(i)),
    }
    view
}

/// The items where every word of `filter` starts a word of their `text`, as
/// indexes into `items`, in the server's order.
fn filtered<T>(items: &[T], filter: &str, text: impl Fn(&T) -> String) -> Vec<usize> {
    let filter = filter.to_lowercase();
    let wanted: Vec<&str> = words(&filter).collect();
    (0..items.len())
        .filter(|&i| matches(&wanted, &text(&items[i]).to_lowercase()))
        .collect()
}

/// What a song is found by: its title, artists and album.
fn song_text(song: &Child) -> String {
    let album = song.album.as_deref().unwrap_or_default();
    song_names(song)
        .iter()
        .fold(format!("{} {album}", song.title), |text, name| {
            text + " " + name.label.as_str()
        })
}

/// What a composition is found by: its name and its composers'.
fn composition_text(composition: &Composition) -> String {
    composition
        .composers
        .iter()
        .fold(composition.name.clone(), |text, composer| {
            text + " " + &composer.name
        })
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
    let notes = id.clone();
    let (kept, offline) = (downloads(browser), is_offline(browser));
    if !offline {
        aside(
            ui,
            browser,
            move |client| {
                let notes = notes.clone();
                async move { client.get_album_info2(&notes).await }
            },
            |ui, _, info| show_about(ui, info.notes.as_deref()),
        );
    }
    load(
        ui,
        browser,
        move |client| {
            let (id, kept) = (id.clone(), kept.clone());
            async move {
                let call = client.get_album(&id);
                saved_or(offline, || kept.album(&id), call).await
            }
        },
        |ui, browser, album| {
            let songs = album.song;
            let album = album.album;
            let shown = album.clone();
            ui.set_header_title(album.name.as_str().into());
            ui.set_header_names(names(album_names(&album)));
            ui.set_header_facts(album_facts(&album).into());
            header_image(ui, browser, album.cover_art);
            header_motion(ui, browser, album.id);

            let mut b = browser.borrow_mut();
            let rows: Vec<Track> = songs.chunk_by(same_work).flat_map(work_rows).collect();
            show_songs(&mut b, songs, rows);
            b.album = Some(shown);
            drop(b);
            show_downloads(ui, browser);
            #[cfg(not(target_family = "wasm"))]
            crate::downloads::follow_shown(ui, browser);
        },
    );
}

/// The rows of a run of consecutive tracks of one work, as Roon draws a
/// classical album: the first is headed by a row for the work, the years it
/// was written, its composers, the performers all its movements share, and
/// when and where they were recorded, with the movements under it; a work's
/// only track is such a row itself.
fn work_rows(run: &[Child]) -> Vec<Track> {
    let mut tracks = Vec::new();
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
                (track_title(run, i), Vec::new(), Vec::new())
            }
            (true, _) => (track_title(run, i), Vec::new(), own),
            (false, true) => (song.title.clone(), composer_names(song), own),
            (false, false) => (song.title.clone(), Vec::new(), own),
        };
        let (years, performed_alone) = if alone {
            (composed(song), performed.as_str())
        } else {
            (String::new(), "")
        };
        let (work, work_composers, work_performers, work_duration) = match song.works.first() {
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
        tracks.push(Track {
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
            work_tracks: if work.is_empty() { 0 } else { run.len() as i32 },
            part: parts,
            alone,
            ..Track::default()
        });
    }
    tracks
}

/// An artist's page: their picture and name, what the server says about
/// them, their top songs and the artists like them, and their albums.
fn artist(ui: &AppWindow, browser: &Shared, id: String) {
    let info = id.clone();
    aside(
        ui,
        browser,
        move |client| {
            let info = info.clone();
            async move { client.get_artist_info2(&info, None, None).await }
        },
        |ui, _, info| {
            show_about(ui, info.biography.as_deref());
            ui.set_similar(names(info.similar_artist.iter().map(linked).collect()));
        },
    );
    let by_id = browser
        .borrow()
        .client
        .as_ref()
        .is_some_and(|client| client.supports(TOP_SONGS_BY_ID));
    if by_id {
        top_songs(ui, browser, id.clone(), true);
    }
    load(
        ui,
        browser,
        move |client| {
            let id = id.clone();
            async move { client.get_artist(&id).await }
        },
        move |ui, browser, artist| {
            if !by_id {
                top_songs(ui, browser, artist.artist.name.clone(), false);
            }
            ui.set_header_title(artist.artist.name.as_str().into());
            ui.set_header_facts(count(artist.album.len(), "album", "albums").into());
            browser.borrow_mut().own_albums = own_albums_of(&artist.artist, &artist.album)
                .map(|album| format!("album:{}", album.id))
                .collect();
            header_image(ui, browser, artist.artist.cover_art);
            clear_tiles(ui, &mut browser.borrow_mut());
            let tiles = artist.album.iter().map(|album| {
                let year = album.year.map(|year| unlinked(year.to_string()));
                album_tile(album, year.into_iter().collect())
            });
            add_tiles(ui, browser, tiles.collect());
        },
    );
}

/// The albums of `artist` that are theirs: they are among its artists and it
/// is no compilation, in the order given. The albums they only appear on, and
/// compilations, are left out.
pub(crate) fn own_albums_of<'a>(
    artist: &'a ArtistID3,
    albums: &'a [AlbumID3],
) -> impl Iterator<Item = &'a AlbumID3> {
    albums.iter().filter(|album| {
        let compilation = album.is_compilation == Some(true)
            || album
                .release_types
                .iter()
                .any(|kind| kind.eq_ignore_ascii_case("compilation"));
        let by_artist = if album.artists.is_empty() {
            album.artist_id.as_deref() == Some(artist.id.as_str())
        } else {
            album.artists.iter().any(|by| by.id == artist.id)
        };
        by_artist && !compilation
    })
}

/// A playlist's tracks: each with its artists and album, and the heart that
/// loves it.
fn playlist(ui: &AppWindow, browser: &Shared, id: String) {
    let (kept, offline) = (downloads(browser), is_offline(browser));
    load(
        ui,
        browser,
        move |client| {
            let (id, kept) = (id.clone(), kept.clone());
            async move {
                let call = client.get_playlist(&id);
                saved_or(offline, || kept.playlist(&id), call).await
            }
        },
        show_playlist,
    );
}

/// The artist's top songs on their page, asked for by their id where the
/// server takes one, else by `artist`, their name; each a row like a
/// playlist's.
fn top_songs(ui: &AppWindow, browser: &Shared, artist: String, by_id: bool) {
    aside(
        ui,
        browser,
        move |client| {
            let artist = artist.clone();
            async move {
                if by_id {
                    client
                        .get_top_songs_by_artist_id(&artist, Some(TOP_SONGS))
                        .await
                } else {
                    client.get_top_songs(&artist, Some(TOP_SONGS)).await
                }
            }
        },
        |_, browser, top| {
            let rows = top.song.iter().map(song_track).collect();
            show_songs(&mut browser.borrow_mut(), top.song, rows);
        },
    );
}

/// Like `load`, for what a page shows beside its main content: no spinner,
/// and nothing said when the server can't answer.
fn aside<T: 'static, F: Future<Output = opensubsonic::Result<T>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    task: impl Fn(Arc<Client>) -> F + 'static,
    done: impl Fn(&AppWindow, &Shared, T) + 'static,
) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let visit = browser.borrow().visit;
    let reader = Arc::new(client.reader(true));
    let first = task(reader.clone());
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = first.await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().visit != visit {
            return;
        }
        let Ok(value) = result else { return };
        done(&ui, &browser, value);
        if reader.gave_aged() {
            ask_again(weak, &browser, visit, &client, task, done).await;
        }
    })
    .expect("the event loop is running");
}

/// Shows a biography or notes as plain text, and its start until opened.
fn show_about(ui: &AppWindow, about: Option<&str>) {
    let about = plain(about.unwrap_or_default());
    ui.set_about_short(start_of(&about).into());
    ui.set_about(about.into());
}

/// Text sent as HTML, as biographies often are, without its tags and with
/// its common entities read.
fn plain(html: &str) -> String {
    let mut text = String::new();
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        text.push_str(&rest[..at]);
        let tag = &rest[at..];
        let end = tag.find('>').map_or(tag.len(), |end| end + 1);
        let name = tag[1..end].trim_start_matches('/').to_ascii_lowercase();
        if name.starts_with("br") || name.starts_with("p>") || name.starts_with("p ") {
            text.push('\n');
        }
        rest = &tag[end..];
    }
    text.push_str(rest);
    let text = [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&nbsp;", " "),
        ("&amp;", "&"),
    ]
    .iter()
    .fold(text, |text, (entity, plain)| text.replace(entity, plain));
    text.trim().to_owned()
}

/// The first paragraph of `text`, cut at a word near `ABOUT_SHORT`
/// characters when longer.
fn start_of(text: &str) -> String {
    let first = text
        .split("\n")
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if first.chars().count() <= ABOUT_SHORT {
        return first.to_owned();
    }
    let cut: String = first.chars().take(ABOUT_SHORT).collect();
    let cut = cut.rfind(' ').map_or(cut.as_str(), |space| &cut[..space]);
    format!("{}…", cut.trim_end_matches([',', ';', ':', ' ']))
}

/// A song's row on a playlist or an artist's top songs: its artists, then
/// its album, and its heart.
fn song_track(song: &Child) -> Track {
    let artists = song_names(song);
    let album = album_name(song);
    Track {
        title: song.title.as_str().into(),
        split: artists.len() as i32,
        names: names(artists.into_iter().chain(album).collect()),
        duration: song.duration.map(duration).unwrap_or_default().into(),
        loved: song.starred.is_some(),
        ..Track::default()
    }
}

/// Puts a playlist on its page, in place of the tracks it showed.
fn show_playlist(ui: &AppWindow, browser: &Shared, playlist: PlaylistWithSongs) {
    let songs = playlist.entry;
    let playlist = playlist.playlist;
    let playlist_shown = playlist.clone();
    ui.set_header_title(playlist.name.as_str().into());
    ui.set_playlist_editable(playlist.readonly != Some(true));
    let facts = [
        playlist.comment.unwrap_or_default(),
        length(playlist.song_count, playlist.duration),
    ];
    let facts: Vec<String> = facts.into_iter().filter(|f| !f.is_empty()).collect();
    ui.set_header_facts(facts.join(" · ").into());
    header_image(ui, browser, playlist.cover_art);
    playlist_moves(ui, &browser.borrow());

    // As an album: the movements of a whole work are the work's row and its
    // movements; every other song is a row of its own. A row is still one
    // song, so a row's number is its song's in what plays, loves and removes.
    let tracks: Vec<Track> = whole_works(&songs)
        .into_iter()
        .flat_map(|run| match run {
            [song] => vec![song_track(song)],
            _ => work_rows(run),
        })
        .collect();
    let mut b = browser.borrow_mut();
    show_songs(&mut b, songs, tracks);
    b.playlist = Some(playlist_shown);
    drop(b);
    show_downloads(ui, browser);
    #[cfg(not(target_family = "wasm"))]
    crate::downloads::follow_shown(ui, browser);
}

/// The id of the playlist on screen, if one is.
fn playlist_on_screen(b: &Browser) -> Option<String> {
    let link = b.history.get(b.at)?;
    link.strip_prefix("playlist:").map(str::to_owned)
}

/// Runs a change to a playlist on the server, follows it with `done` when
/// taken or says the server refused, on the page it was made on, then asks
/// for the playlists again, and for the playlist on screen when that is
/// `id`, without blanking it.
fn change_playlist<F: Future<Output = opensubsonic::Result<()>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    id: Option<String>,
    task: impl FnOnce(Arc<Client>) -> F,
    done: impl FnOnce(&AppWindow, &Shared) + 'static,
) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let task = task(client.clone());
    let visit = browser.borrow().visit;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = task.await;
        let Some(ui) = weak.upgrade() else { return };
        let signed_in = browser
            .borrow()
            .client
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &client));
        if !signed_in {
            return;
        }
        match result {
            Ok(()) => done(&ui, &browser),
            Err(e) if browser.borrow().visit == visit => {
                ui.set_page_problem(format!("Your server didn't take that: {e}").into());
            }
            Err(_) => {}
        }
        load_playlists(&ui, &browser);
        let on_screen = id.filter(|id| playlist_on_screen(&browser.borrow()).as_ref() == Some(id));
        if let Some(id) = on_screen {
            refresh_playlist(&ui, &browser, id);
        }
    })
    .expect("the event loop is running");
}

/// Asks for the playlist on screen again and puts it in place of what its
/// page shows, unless the page is left by then.
fn refresh_playlist(ui: &AppWindow, browser: &Shared, id: String) {
    aside(
        ui,
        browser,
        move |client| {
            let id = id.clone();
            async move { client.get_playlist(&id).await }
        },
        show_playlist,
    );
}

/// Makes an empty playlist with this name, in the playlist folder on
/// screen, and opens it.
fn create_playlist(ui: &AppWindow, browser: &Shared, name: String) {
    if name.is_empty() {
        return;
    }
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let folder = ui.get_shown_id().to_string();
    let folder = (ui.get_page() == Page::Playlists && !folder.is_empty()).then_some(folder);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let created = client.create_playlist(&name, &[]).await;
        let moved = match (&created, &folder) {
            (Ok(Some(playlist)), Some(folder)) => {
                let id = playlist.playlist.id.as_str();
                client.move_playlists(&[id], Some(folder), &[]).await
            }
            _ => Ok(()),
        };
        let Some(ui) = weak.upgrade() else { return };
        load_playlists(&ui, &browser);
        match created {
            Ok(Some(playlist)) => open(&ui, &browser, format!("playlist:{}", playlist.playlist.id)),
            Ok(None) => {}
            Err(e) => ui.set_page_problem(format!("Your server didn't take that: {e}").into()),
        }
        if let Err(e) = moved {
            notify(&ui, &browser, format!("It isn't in the folder: {e}"));
        }
    })
    .expect("the event loop is running");
}

/// Makes a playlist folder with this name in the one on screen.
fn create_folder(ui: &AppWindow, browser: &Shared, name: String) {
    if name.is_empty() {
        return;
    }
    let parent = ui.get_shown_id().to_string();
    change_playlist(
        ui,
        browser,
        None,
        |client| async move {
            let parent = Some(parent.as_str()).filter(|parent| !parent.is_empty());
            client
                .create_playlist_folder(&name, parent, None)
                .await
                .map(drop)
        },
        |_, _| {},
    );
}

/// Renames the playlist folder on screen, at once on its page.
fn rename_folder(ui: &AppWindow, browser: &Shared, name: String) {
    let id = ui.get_shown_id().to_string();
    if name.is_empty() || id.is_empty() {
        return;
    }
    ui.set_header_title(name.as_str().into());
    change_playlist(
        ui,
        browser,
        None,
        |client| async move {
            let update = PlaylistFolderUpdate {
                name: Some(&name),
                ..PlaylistFolderUpdate::default()
            };
            client.update_playlist_folder(&id, &update).await
        },
        |_, _| {},
    );
}

/// Moves the playlist folder on screen into the one with id `target`, or to
/// the top level for "".
fn move_folder(ui: &AppWindow, browser: &Shared, target: String) {
    let id = ui.get_shown_id().to_string();
    if id.is_empty() {
        return;
    }
    change_playlist(
        ui,
        browser,
        None,
        |client| async move {
            let update = PlaylistFolderUpdate {
                parent_id: Some(&target),
                ..PlaylistFolderUpdate::default()
            };
            client.update_playlist_folder(&id, &update).await
        },
        |_, _| {},
    );
}

/// Deletes the playlist folder on screen, which the server only does when
/// it is empty, and shows the one it was in.
fn delete_folder(ui: &AppWindow, browser: &Shared) {
    let id = ui.get_shown_id().to_string();
    if id.is_empty() {
        return;
    }
    let up = {
        let b = browser.borrow();
        let nodes = folder_nodes(&b.playlist_folders);
        let parent = nodes
            .iter()
            .find(|node| node.0 == id)
            .and_then(|node| node.2);
        parent.map_or_else(
            || "playlists".to_owned(),
            |parent| format!("playlists:{parent}"),
        )
    };
    change_playlist(
        ui,
        browser,
        None,
        |client| async move { client.delete_playlist_folder(&id).await },
        |ui, browser| open(ui, browser, up),
    );
}

/// Moves the playlist on screen into the folder with id `target`, or to the
/// top level for "".
fn move_playlist(ui: &AppWindow, browser: &Shared, target: String) {
    let Some(id) = playlist_on_screen(&browser.borrow()) else {
        return;
    };
    move_playlists(ui, browser, vec![id], target);
}

/// Moves the playlists with these ids into the folder with id `target`, or
/// to the top level for "", at once on screen. Those already there stay, as
/// the server has them: those shown may be from up to an hour ago.
fn move_playlists(ui: &AppWindow, browser: &Shared, ids: Vec<String>, target: String) {
    let (to, on_screen, shown_moving) = {
        let mut b = browser.borrow_mut();
        let to = match target.as_str() {
            "" => "the top level".to_owned(),
            folder => path(&folder_nodes(&b.playlist_folders), folder),
        };
        let mut shown_moving = false;
        for playlist in b
            .playlists
            .iter_mut()
            .filter(|playlist| ids.contains(&playlist.id))
        {
            if !is_in_folder(playlist, &target) {
                playlist.playlist_folder_id = Some(target.clone());
                shown_moving = true;
            }
        }
        let on_screen = playlist_on_screen(&b).filter(|id| ids.contains(id));
        (to, on_screen, shown_moving)
    };
    if shown_moving {
        sidebar(ui, browser);
        playlist_moves(ui, &browser.borrow());
        if ui.get_page() == Page::Playlists {
            show_playlists(ui, browser);
        }
    }
    let moved = Rc::new(std::cell::Cell::new(false));
    let moving = moved.clone();
    change_playlist(
        ui,
        browser,
        on_screen,
        |client| async move {
            let now = client
                .asking_the_server()
                .get_playlists(None)
                .await?
                .playlist;
            let ids: Vec<&str> = (ids.iter())
                .filter(|id| {
                    !(now.iter())
                        .any(|playlist| playlist.id == **id && is_in_folder(playlist, &target))
                })
                .map(String::as_str)
                .collect();
            if ids.is_empty() {
                return Ok(());
            }
            moving.set(true);
            client.move_playlists(&ids, Some(&target), &[]).await
        },
        move |ui, browser| {
            if moved.get() {
                notify(ui, browser, format!("Moved to {to}."));
            }
        },
    );
}

/// Whether `playlist` is in the folder with id `folder`, or at the top level
/// for "".
fn is_in_folder(playlist: &Playlist, folder: &str) -> bool {
    playlist.playlist_folder_id.as_deref().unwrap_or("") == folder
}

/// The playlist dropped, by id, which is then no longer in flight.
fn dropped_playlist(ui: &AppWindow) -> Option<String> {
    let drag = ui.global::<Drag>();
    let link = drag.get_link();
    drag.set_link("".into());
    link.strip_prefix("playlist:").map(str::to_owned)
}

/// Renames the playlist on screen, at once on its page.
fn rename_playlist(ui: &AppWindow, browser: &Shared, name: String) {
    let Some(id) = playlist_on_screen(&browser.borrow()) else {
        return;
    };
    if name.is_empty() {
        return;
    }
    ui.set_header_title(name.as_str().into());
    let (at, old) = (id.clone(), id.clone());
    // A server that names a playlist by its file, as Fonoteca does, gives it
    // a new id with a new name: the page and the history move to that one.
    let moved = Rc::new(RefCell::new(None::<String>));
    let found = moved.clone();
    change_playlist(
        ui,
        browser,
        Some(id),
        |client| async move {
            let update = PlaylistUpdate {
                name: Some(&name),
                ..PlaylistUpdate::default()
            };
            client.update_playlist(&at, &update).await?;
            if let Ok(list) = client.get_playlists(None).await
                && !list.playlist.iter().any(|playlist| playlist.id == at)
            {
                let renamed = list
                    .playlist
                    .into_iter()
                    .find(|playlist| playlist.name == name);
                *found.borrow_mut() = renamed.map(|playlist| playlist.id);
            }
            Ok(())
        },
        move |ui, browser| {
            let Some(new) = moved.borrow_mut().take() else {
                return;
            };
            let (old, link) = (format!("playlist:{old}"), format!("playlist:{new}"));
            let on_screen = {
                let mut b = browser.borrow_mut();
                for seen in b.history.iter_mut().filter(|seen| **seen == old) {
                    seen.clone_from(&link);
                }
                b.history.get(b.at) == Some(&link)
            };
            if on_screen {
                refresh_playlist(ui, browser, new);
            }
        },
    );
}

/// Deletes the playlist on screen and shows the playlists left.
fn delete_playlist(ui: &AppWindow, browser: &Shared) {
    let Some(id) = playlist_on_screen(&browser.borrow()) else {
        return;
    };
    change_playlist(
        ui,
        browser,
        None,
        |client| async move { client.delete_playlist(&id).await },
        |ui, browser| open(ui, browser, "playlists".into()),
    );
}

/// Takes the `row`th track off the playlist on screen, at once on its page.
/// The server takes it off by its place, and the page may show the playlist
/// as it was kept up to an hour ago: when the server's has another song
/// there, changed elsewhere since, nothing is taken off and the page shows
/// the server's.
fn remove_from_playlist(ui: &AppWindow, browser: &Shared, row: usize) {
    let Some(id) = playlist_on_screen(&browser.borrow()) else {
        return;
    };
    let picked = {
        let mut b = browser.borrow_mut();
        if row >= b.songs.len() || row >= b.tracks.row_count() {
            return;
        }
        let picked = b.songs.remove(row);
        b.tracks.remove(row);
        let removed = song_link(&picked);
        // The same song may be on the playlist twice.
        if !b.songs.iter().any(|song| song_link(song) == removed) {
            b.selected.retain(|link| *link != removed);
        }
        show_selected(ui, &b);
        picked.id
    };
    let changed_elsewhere = Rc::new(std::cell::Cell::new(false));
    let (at, changed) = (id.clone(), changed_elsewhere.clone());
    change_playlist(
        ui,
        browser,
        Some(id),
        |client| async move {
            let now = client.asking_the_server().get_playlist(&at).await?;
            if now.entry.get(row).is_none_or(|song| song.id != picked) {
                changed.set(true);
                return Ok(());
            }
            let update = PlaylistUpdate {
                song_indexes_to_remove: &[row as u32],
                ..PlaylistUpdate::default()
            };
            client.update_playlist(&at, &update).await
        },
        move |ui, browser| {
            if changed_elsewhere.get() {
                let text = "The playlist changed elsewhere, so nothing was taken off.";
                notify(ui, browser, text.to_owned());
            }
        },
    );
}

/// Adds the songs of `links` to the end of the playlist `id`, in their
/// order, and says how many went in: a song's own, an album's, an artist's
/// albums' or a playlist's. Folders and compositions add none.
// ponytail: folders and compositions are skipped; walk them when someone selects those to add.
pub(crate) fn add_to_playlist(ui: &AppWindow, browser: &Shared, id: String, links: Vec<String>) {
    let name = browser
        .borrow()
        .playlists
        .iter()
        .find(|playlist| playlist.id == id)
        .map_or_else(
            || "the playlist".to_owned(),
            |playlist| playlist.name.clone(),
        );
    let count = Rc::new(std::cell::Cell::new(0));
    let added = count.clone();
    change_playlist(
        ui,
        browser,
        Some(id.clone()),
        |client| async move {
            let songs = songs_of(&client, &links).await?;
            added.set(songs.len());
            if songs.is_empty() {
                return Ok(());
            }
            let songs: Vec<&str> = songs.iter().map(String::as_str).collect();
            let update = PlaylistUpdate {
                song_ids_to_add: &songs,
                ..PlaylistUpdate::default()
            };
            client.update_playlist(&id, &update).await
        },
        move |ui, browser| {
            let text = match count.get() {
                0 => "Nothing selected goes in a playlist.".to_owned(),
                1 => format!("Added 1 track to {name}."),
                n => format!("Added {n} tracks to {name}."),
            };
            notify(ui, browser, text);
        },
    );
}

/// The songs of these links, in their order: a song's own, an album's, an
/// artist's albums', a playlist's or those under a folder.
async fn songs_of(client: &Client, links: &[String]) -> opensubsonic::Result<Vec<String>> {
    let mut songs = Vec::new();
    for link in links {
        let (kind, id) = link.split_once(':').unwrap_or((link, ""));
        match kind {
            "song" => songs.push(id.to_owned()),
            "album" => songs.extend(client.get_album(id).await?.song.into_iter().map(|s| s.id)),
            // As the server has it now, not as kept: it may have changed
            // elsewhere.
            "playlist" => songs.extend(
                client
                    .asking_the_server()
                    .get_playlist(id)
                    .await?
                    .entry
                    .into_iter()
                    .map(|s| s.id),
            ),
            "artist" => {
                for album in client.get_artist(id).await?.album {
                    songs.extend(
                        client
                            .get_album(&album.id)
                            .await?
                            .song
                            .into_iter()
                            .map(|s| s.id),
                    );
                }
            }
            "folder" => songs.extend(
                whole_folder(client, id)
                    .await?
                    .child
                    .into_iter()
                    .map(|s| s.id),
            ),
            _ => {}
        }
    }
    Ok(songs)
}

/// Puts up a short notice for a few seconds.
pub fn notify(ui: &AppWindow, browser: &Shared, text: String) {
    ui.set_notice(text.into());
    let weak = ui.as_weak();
    browser
        .borrow()
        .notice
        .start(TimerMode::SingleShot, NOTICE_TIME, move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_notice("".into());
            }
        });
}

/// Asks for the user's tags, on servers with them, and shows them wherever
/// they are on screen.
fn load_tags(ui: &AppWindow, browser: &Shared) {
    ask_tags(ui, browser, true);
}

/// Asks for the tags as `load_tags` says, showing aged answers at once where
/// `shows_aged`, and then asking again without.
fn ask_tags(ui: &AppWindow, browser: &Shared, shows_aged: bool) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    if !client.supports(TAGS) {
        return;
    }
    let reader = client.reader(shows_aged);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let answer = reader.get_tags().await;
        let Some(ui) = weak.upgrade() else { return };
        let signed_in = browser
            .borrow()
            .client
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &client));
        if !signed_in {
            return;
        }
        let shown = ui.get_page() == Page::Tags;
        let known = browser.borrow().tags_loaded;
        if shown {
            ui.set_loading(false);
        }
        match answer {
            Ok(_) if !shows_aged && !reader.heard_news() => return,
            Ok(tags) => {
                let mut b = browser.borrow_mut();
                b.tags = tags.tag;
                b.tags_loaded = true;
            }
            // The tags shown stay when asking again fails.
            Err(e) if shown && !known => return ui.set_page_problem(e.to_string().into()),
            Err(_) => return,
        }
        show_tags(&ui, &browser);
        if reader.gave_aged() {
            ask_tags(&ui, &browser, false);
        }
    })
    .expect("the event loop is running");
}

/// The tags page: the user's tags, each under the one it is in, those known
/// at once, then asked for again.
fn tags(ui: &AppWindow, browser: &Shared) {
    if browser.borrow().tags_loaded {
        show_tags(ui, browser);
    } else {
        ui.set_loading(true);
    }
    load_tags(ui, browser);
}

/// Puts the tags known on the tags page, a tag's page and the tags dialog.
// ponytail: each row's move targets walk the whole tree, cubic in tags; build a child map if someone has thousands.
fn show_tags(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    let nodes = tag_nodes(&b.tags);
    let rows: Vec<TagRow> = tree(&nodes)
        .into_iter()
        .map(|((id, name, parent), depth)| {
            let tag = b.tags.iter().find(|tag| tag.id == id);
            let tracks = tag.map_or(0, |tag| usize::try_from(tag.song_count).unwrap_or(0));
            TagRow {
                id: id.into(),
                title: name.into(),
                details: count(tracks, "track", "tracks").into(),
                depth: depth as i32,
                targets: names(destinations(&nodes, Some(id), parent)),
            }
        })
        .collect();
    ui.set_tag_rows(ModelRc::new(VecModel::from(rows)));
    tag_header(ui, &b);
    tag_choices(ui, &b);
}

/// Runs a change to the tags on the server, follows it with `done`, with
/// nothing when the server refused, which it says, then asks for the tags
/// again.
fn change_tags<T: 'static, F: Future<Output = opensubsonic::Result<T>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    task: impl FnOnce(Arc<Client>) -> F,
    done: impl FnOnce(&AppWindow, &Shared, Option<T>) + 'static,
) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let task = task(client.clone());
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = task.await;
        let Some(ui) = weak.upgrade() else { return };
        let signed_in = browser
            .borrow()
            .client
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &client));
        if !signed_in {
            return;
        }
        if let Err(e) = &result {
            notify(&ui, &browser, refusal(e));
        }
        // A tag's page left may no longer have the songs it holds.
        {
            let mut b = browser.borrow_mut();
            if ui.get_page() != Page::Tag && b.folder.starts_with("tag:") {
                b.folder.clear();
            }
        }
        done(&ui, &browser, result.ok());
        load_tags(&ui, &browser);
    })
    .expect("the event loop is running");
}

/// Renames the tag `id`.
fn rename_tag(ui: &AppWindow, browser: &Shared, id: String, name: String) {
    if name.is_empty() {
        return;
    }
    change_tags(
        ui,
        browser,
        |client| async move {
            let update = TagUpdate {
                name: Some(&name),
                ..TagUpdate::default()
            };
            client.update_tag(&id, &update).await
        },
        |_, _, _| {},
    );
}

/// Moves the tag `id` into the one with id `target`, or to the top level for
/// "".
fn move_tag(ui: &AppWindow, browser: &Shared, id: String, target: String) {
    change_tags(
        ui,
        browser,
        |client| async move {
            let update = TagUpdate {
                parent_id: Some(&target),
                ..TagUpdate::default()
            };
            client.update_tag(&id, &update).await
        },
        |_, _, _| {},
    );
}

/// Deletes the tag `id` and those in it; the songs stay. From its page, or
/// one of those in it, goes to the tags page.
fn delete_tag(ui: &AppWindow, browser: &Shared, id: String) {
    let gone = ui.get_page() == Page::Tag && {
        let shown = ui.get_shown_id();
        let b = browser.borrow();
        shown == id.as_str() || within(&tag_nodes(&b.tags), &shown, &id)
    };
    change_tags(
        ui,
        browser,
        |client| async move { client.delete_tag(&id).await },
        move |ui, browser, deleted| {
            if deleted.is_some() && gone {
                open(ui, browser, "tags".into());
            }
        },
    );
}

/// Readies the tags dialog for the songs of these links, as add_to_playlist
/// takes them, with the tags each has: known for the songs on screen, else
/// asked for.
pub(crate) fn start_tagging(ui: &AppWindow, browser: &Shared, links: Vec<String>) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let round = {
        let mut b = browser.borrow_mut();
        b.tagging_round += 1;
        b.tagging.clear();
        b.tagging_round
    };
    let (mut known, wanted) = {
        let b = browser.borrow();
        let mut known = Vec::new();
        let mut wanted = Vec::new();
        for link in links {
            let id = link.strip_prefix("song:");
            let mut songs = b.song_list.iter().chain(&b.entries);
            match id.and_then(|id| songs.find(|song| song.id == id)) {
                Some(song) => known.push((song.id.clone(), song.tag_ids.clone())),
                None => wanted.push(link),
            }
        }
        (known, wanted)
    };
    let ready = move |ui: &AppWindow, browser: &Shared, mut songs: Vec<(String, Vec<String>)>| {
        let mut seen = HashSet::new();
        songs.retain(|(id, _)| seen.insert(id.clone()));
        browser.borrow_mut().tagging = songs;
        tag_choices(ui, &browser.borrow());
        ui.set_tag_choices_loading(false);
    };
    if wanted.is_empty() {
        return ready(ui, browser, known);
    }
    ui.set_tag_choices_loading(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let asked = children_of(&client, &wanted).await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().tagging_round != round {
            return;
        }
        match asked {
            Ok(songs) => known.extend(songs.into_iter().map(|song| (song.id, song.tag_ids))),
            Err(e) => notify(&ui, &browser, e.to_string()),
        }
        ready(&ui, &browser, known);
    })
    .expect("the event loop is running");
}

/// The songs of these links, as add_to_playlist takes them, with all the
/// server says of each.
async fn children_of(client: &Client, links: &[String]) -> opensubsonic::Result<Vec<Child>> {
    let mut songs = Vec::new();
    for link in links {
        let (kind, id) = link.split_once(':').unwrap_or((link, ""));
        match kind {
            "song" => songs.push(client.get_song(id).await?),
            "album" => songs.extend(client.get_album(id).await?.song),
            "playlist" => songs.extend(client.get_playlist(id).await?.entry),
            "artist" => {
                for album in client.get_artist(id).await?.album {
                    songs.extend(client.get_album(&album.id).await?.song);
                }
            }
            "folder" => songs.extend(whole_folder(client, id).await?.child),
            _ => {}
        }
    }
    Ok(songs)
}

/// The folder `id` with every song under it as its children, those of each
/// folder in it in its place: what playing, adding, tagging or keeping a
/// folder takes. A folder met again, as a link loop on the server makes, is
/// gone through once.
pub async fn whole_folder(client: &Client, id: &str) -> opensubsonic::Result<Directory> {
    let mut folder = client.get_music_directory(id).await?;
    let mut seen = HashSet::from([id.to_owned()]);
    let mut waiting: Vec<Child> = folder.child.drain(..).rev().collect();
    while let Some(entry) = waiting.pop() {
        if !entry.is_dir {
            folder.child.push(entry);
        } else if seen.insert(entry.id.clone()) {
            let inner = client.get_music_directory(&entry.id).await?;
            waiting.extend(inner.child.into_iter().rev());
        }
    }
    Ok(folder)
}

/// The tags dialog's tags, ticked where every song it is for has them.
fn tag_choices(ui: &AppWindow, b: &Browser) {
    let nodes = tag_nodes(&b.tags);
    let choices: Vec<TagChoice> = tree(&nodes)
        .into_iter()
        .map(|((id, name, _), depth)| TagChoice {
            title: name.into(),
            depth: depth as i32,
            ticked: tagged(&b.tagging, id),
        })
        .collect();
    ui.set_tag_choices(ModelRc::new(VecModel::from(choices)));
}

/// Whether every one of these songs, and there are some, has the tag.
fn tagged(songs: &[(String, Vec<String>)], tag: &str) -> bool {
    !songs.is_empty() && songs.iter().all(|(_, tags)| tags.iter().any(|t| t == tag))
}

/// Puts the `index`th tag of the tags dialog on the songs it is for that
/// lack it, or when they all have it takes it off them: at once in the
/// dialog and the songs shown, undone when the server refuses.
fn toggle_tag(ui: &AppWindow, browser: &Shared, index: usize) {
    let change = {
        let b = browser.borrow();
        let nodes = tag_nodes(&b.tags);
        tree(&nodes).into_iter().nth(index).map(|((id, ..), _)| {
            let on = !tagged(&b.tagging, id);
            let songs = b.tagging.iter();
            let songs = songs.filter(|(_, tags)| !on || !tags.iter().any(|tag| tag == id));
            let songs: Vec<String> = songs.map(|(song, _)| song.clone()).collect();
            (id.to_owned(), songs, on)
        })
    };
    let Some((tag, songs, on)) = change.filter(|(_, songs, _)| !songs.is_empty()) else {
        // Unticks what was ticked with nothing to tag.
        return tag_choices(ui, &browser.borrow());
    };
    mark_tagged(ui, browser, &tag, &songs, on);
    let (at, ids) = (tag.clone(), songs.clone());
    change_tags(
        ui,
        browser,
        |client| async move {
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let update = if on {
                TagUpdate {
                    song_ids_to_add: &ids,
                    ..TagUpdate::default()
                }
            } else {
                TagUpdate {
                    song_ids_to_remove: &ids,
                    ..TagUpdate::default()
                }
            };
            client.update_tag(&at, &update).await
        },
        move |ui, browser, done| match done {
            Some(()) => refresh_tag(ui, browser),
            None => mark_tagged(ui, browser, &tag, &songs, !on),
        },
    );
}

/// Makes a tag with this name at the top level and puts it on the songs of
/// the tags dialog.
fn tag_with_new(ui: &AppWindow, browser: &Shared, name: String) {
    if name.is_empty() {
        return;
    }
    let songs: Vec<String> = browser
        .borrow()
        .tagging
        .iter()
        .map(|(id, _)| id.clone())
        .collect();
    change_tags(
        ui,
        browser,
        |client| async move {
            let tag = client.create_tag(&name, None).await?;
            if !songs.is_empty() {
                let ids: Vec<&str> = songs.iter().map(String::as_str).collect();
                let update = TagUpdate {
                    song_ids_to_add: &ids,
                    ..TagUpdate::default()
                };
                client.update_tag(&tag.id, &update).await?;
            }
            Ok((tag, songs))
        },
        |ui, browser, made| {
            let Some((tag, songs)) = made else { return };
            let id = tag.id.clone();
            browser.borrow_mut().tags.push(tag);
            mark_tagged(ui, browser, &id, &songs, true);
            refresh_tag(ui, browser);
        },
    );
}

/// Records that these songs have the tag, or with `on` false don't, in the
/// tags dialog and the songs shown.
fn mark_tagged(ui: &AppWindow, browser: &Shared, tag: &str, songs: &[String], on: bool) {
    let songs: HashSet<&str> = songs.iter().map(String::as_str).collect();
    let mut b = browser.borrow_mut();
    let b = &mut *b;
    let dialog = b.tagging.iter_mut().map(|(id, tags)| (&*id, tags));
    let shown = b.song_list.iter_mut().chain(&mut b.entries);
    let shown = shown.map(|song| (&song.id, &mut song.tag_ids));
    for (_, tags) in dialog
        .chain(shown)
        .filter(|(id, _)| songs.contains(id.as_str()))
    {
        tags.retain(|t| t != tag);
        if on {
            tags.push(tag.to_owned());
        }
    }
    tag_choices(ui, b);
}

/// Asks the server how its scan of the libraries stands, for the settings
/// page. Always the server: a kept answer says nothing of the scan now.
fn scan_status(ui: &AppWindow, browser: &Shared) {
    aside(
        ui,
        browser,
        |client| async move { client.asking_the_server().get_scan_status().await },
        show_scan,
    );
}

/// Has the server scan its libraries, and says how that goes.
fn start_scan(ui: &AppWindow, browser: &Shared) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    ui.set_scanning(true);
    ui.set_page_problem("".into());
    let visit = browser.borrow().visit;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = client.start_scan().await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().visit != visit {
            return;
        }
        match result {
            Ok(status) => show_scan(&ui, &browser, status),
            Err(e) => {
                ui.set_scanning(false);
                ui.set_page_problem(format!("Your server didn't take that: {e}").into());
            }
        }
    })
    .expect("the event loop is running");
}

/// Puts a scan's status on the settings page, and asks again every few
/// seconds until the server is done.
fn show_scan(ui: &AppWindow, browser: &Shared, status: ScanStatus) {
    ui.set_scanning(status.scanning);
    ui.set_scan_status(scan_text(&status).into());
    let b = browser.borrow();
    if !status.scanning {
        b.scan.stop();
    } else if !b.scan.running() {
        let (weak, browser) = (ui.as_weak(), Rc::downgrade(browser));
        b.scan.start(TimerMode::Repeated, SCAN_POLL, move || {
            if let (Some(ui), Some(browser)) = (weak.upgrade(), browser.upgrade()) {
                scan_status(&ui, &browser);
            }
        });
    }
}

/// The roles, in the order of `role_flags`.
const ROLE_NAMES: [&str; 11] = [
    "Admin",
    "Settings",
    "Stream",
    "Download",
    "Upload",
    "Cover art",
    "Comments",
    "Podcasts",
    "Share",
    "Jukebox",
    "Video",
];

fn role_flags(user: &User) -> [bool; 11] {
    [
        user.admin_role,
        user.settings_role,
        user.stream_role,
        user.download_role,
        user.upload_role,
        user.cover_art_role,
        user.comment_role,
        user.podcast_role,
        user.share_role,
        user.jukebox_role,
        user.video_conversion_role,
    ]
}

/// "Admin · Stream · Share": the roles a user has.
fn role_text(user: &User) -> String {
    ROLE_NAMES
        .iter()
        .zip(role_flags(user))
        .filter(|&(_, on)| on)
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Asks who is signed in and what they may do, for the account page's
/// roles and the settings page's scan. A server that doesn't say leaves the
/// scan offered.
fn load_me(ui: &AppWindow, browser: &Shared) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    // Empty when signed in with an API key, whose user the server names.
    let name = ui.get_username().to_string();
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let user = async {
            let name = match name.is_empty() {
                true => client.token_info().await?.username,
                false => name,
            };
            client.get_user(&name).await
        }
        .await
        .ok();
        let Some(ui) = weak.upgrade() else { return };
        let signed_in = browser
            .borrow()
            .client
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &client));
        if !signed_in {
            return;
        }
        ui.set_roles(user.as_ref().map(role_text).unwrap_or_default().into());
        ui.set_may_administer(user.as_ref().is_none_or(|user| user.admin_role));
        ui.set_may_download(user.as_ref().is_none_or(|user| user.download_role));
        browser.borrow_mut().me = user;
    })
    .expect("the event loop is running");
}

/// Has the server change its radio stations with `task`, then shows them
/// anew with `done` as a notice, or says why it didn't.
fn change_stations<F: Future<Output = opensubsonic::Result<()>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    done: String,
    task: impl FnOnce(Arc<Client>) -> F,
) {
    change(ui, browser, done, task, show_stations_anew);
}

/// Asks for the saved stations again and shows them where they are on screen.
fn show_stations_anew(ui: &AppWindow, browser: &Shared) {
    if ui.get_page() == Page::Radio {
        stations(ui, browser);
    } else {
        saved_stations(ui, browser);
    }
}

/// Has the server make a change with `task`, then puts up `done` as a
/// notice and runs `then`, or says why it didn't.
fn change<F: Future<Output = opensubsonic::Result<()>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    done: String,
    task: impl FnOnce(Arc<Client>) -> F,
    then: fn(&AppWindow, &Shared),
) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    ui.set_page_problem("".into());
    let task = task(client);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = task.await;
        let Some(ui) = weak.upgrade() else { return };
        match result {
            Ok(()) => {
                notify(&ui, &browser, done);
                then(&ui, &browser);
            }
            Err(e) => ui.set_page_problem(refusal(&e).into()),
        }
    })
    .expect("the event loop is running");
}

/// The radio page: the server's internet radio stations as a shelf, and the
/// rows the dialogs fill in from, with their home pages and where they stream
/// from.
fn stations(ui: &AppWindow, browser: &Shared) {
    load(
        ui,
        browser,
        |client| async move { client.get_internet_radio_stations().await },
        |ui, browser, list| show_saved(ui, browser, list.internet_radio_station),
    );
}

/// A station as the dialog filled it in; an empty home page is none.
fn station<'a>(name: &'a str, stream: &'a str, homepage: &'a str) -> RadioStation<'a> {
    RadioStation {
        name,
        stream_url: stream,
        homepage_url: Some(homepage).filter(|homepage| !homepage.is_empty()),
    }
}

/// The radio directory's client, made the first time it is needed.
fn directory(browser: &Shared) -> Option<Rc<radiobrowser::Client>> {
    let mut b = browser.borrow_mut();
    if b.directory.is_none() {
        let client = radiobrowser::Client::with_root(&directory_root(), USER_AGENT);
        b.directory = client.ok().map(Rc::new);
    }
    b.directory.clone()
}

#[cfg(not(test))]
fn directory_root() -> String {
    radiobrowser::API_ROOT.into()
}

/// Where the tests' radio directory answers.
#[cfg(test)]
pub static DIRECTORY_ROOT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

#[cfg(test)]
fn directory_root() -> String {
    DIRECTORY_ROOT.lock().unwrap().clone()
}

/// The stations of a shelf's or page's id, most votes first, `limit` from
/// `offset`: "" for all of them, "@" and a country's code for its country's,
/// else a genre's tag.
async fn worldwide(
    directory: &radiobrowser::Client,
    id: &str,
    offset: u32,
    limit: u32,
) -> radiobrowser::Result<Vec<Station>> {
    match id.strip_prefix('@') {
        _ if id.is_empty() => directory.top_voted(offset, limit).await,
        Some(code) => directory.by_country(code, offset, limit).await,
        None => directory.by_tag(id, offset, limit).await,
    }
}

/// A genre's stations in each of `languages`, `limit` of each; none for the
/// other ids.
async fn in_languages(
    directory: &radiobrowser::Client,
    id: &str,
    languages: &[String],
    limit: u32,
) -> radiobrowser::Result<Vec<Vec<Station>>> {
    let mut lists = Vec::new();
    if !id.is_empty() && !id.starts_with('@') {
        for language in languages {
            lists.push(directory.by_tag_in(id, language, 0, limit).await?);
        }
    }
    Ok(lists)
}

/// The stations of `lists` taken in turn, then `rest`, each once: a station
/// counts as `seen` by its id, and by its name in its country, since the
/// directory has some twice.
fn merge(lists: &[Vec<Station>], rest: &[Station], seen: &mut HashSet<String>) -> Vec<Station> {
    let longest = lists.iter().map(Vec::len).max().unwrap_or(0);
    let turns = (0..longest).flat_map(|i| lists.iter().filter_map(move |list| list.get(i)));
    let mut merged = Vec::new();
    for station in turns.chain(rest) {
        let name = format!(
            "{}\n{}",
            station.name.trim().to_lowercase(),
            station.countrycode
        );
        if seen.insert(station.stationuuid.clone()) & seen.insert(name) {
            merged.push(station.clone());
        }
    }
    merged
}

/// A shelf's stations: a genre's in `languages` first, then worldwide ones,
/// a shelf's worth.
async fn shelf_stations(
    directory: &radiobrowser::Client,
    id: &str,
    languages: &[String],
) -> radiobrowser::Result<Vec<Station>> {
    let lists = in_languages(directory, id, languages, SHELF_STATIONS).await?;
    let mut seen = HashSet::new();
    let mut stations = merge(&lists, &[], &mut seen);
    if stations.len() < SHELF_STATIONS as usize {
        let rest = worldwide(directory, id, 0, SHELF_STATIONS).await?;
        stations.extend(merge(&[], &rest, &mut seen));
    }
    stations.truncate(SHELF_STATIONS as usize);
    Ok(stations)
}

/// Learns the countries' names and the languages, once each.
async fn learn(browser: &Shared, directory: &radiobrowser::Client) {
    if browser.borrow().countries.is_empty()
        && let Ok(countries) = directory.countries().await
    {
        browser.borrow_mut().countries = countries
            .into_iter()
            .map(|country| (country.iso_3166_1, country.name))
            .collect();
    }
    if browser.borrow().language_names.is_empty()
        && let Ok(languages) = directory.languages().await
    {
        let mut names: Vec<String> = languages
            .into_iter()
            .filter(|language| language.iso_639.is_some())
            .map(|language| language.name)
            .collect();
        names.sort();
        names.dedup();
        browser.borrow_mut().language_names = names;
    }
}

/// A country's name as the directory has it, without a leading "The".
fn country_name<'a>(b: &'a Browser, code: &'a str) -> &'a str {
    let name = b.countries.get(code).map_or(code, String::as_str);
    name.strip_prefix("The ").unwrap_or(name)
}

/// The title of a shelf's or page's id.
fn page_title(b: &Browser, id: &str) -> String {
    match (
        id.strip_prefix('@'),
        GENRES.iter().find(|(_, tag)| *tag == id),
    ) {
        (Some(code), _) => {
            let the = b
                .countries
                .get(code)
                .is_some_and(|name| name.starts_with("The "));
            let the = if the { "the " } else { "" };
            format!("Top in {the}{}", country_name(b, code))
        }
        (None, Some((title, _))) => (*title).to_owned(),
        (None, None) => id.to_owned(),
    }
}

/// A shelf's id: a genre's, or the local shelf's country.
fn shelf_id(b: &Browser, shelf: usize) -> Option<String> {
    match GENRES.get(shelf) {
        Some((_, tag)) => Some((*tag).to_owned()),
        None => Some(format!("@{}", b.local.as_ref()?)),
    }
}

/// A shelf's place on the radio page: Top worldwide, the local shelf, then
/// the genres in the order of `genre_ranks`.
fn place(b: &Browser, shelf: usize) -> usize {
    match (shelf, b.shelf_ranks.get(shelf)) {
        (LOCAL, _) => 1,
        (_, Some(0)) => 0,
        (_, Some(rank)) => rank + 1,
        (shelf, None) => shelf + 1,
    }
}

/// The languages the radio page offers, with the ones picked ticked.
fn show_languages(ui: &AppWindow, b: &Browser) {
    let capital = |word: &str| {
        let mut letters = word.chars();
        letters.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(letters).collect()
        })
    };
    let choices: Vec<ColumnChoice> = (b.language_names.iter())
        .map(|name| ColumnChoice {
            title: name
                .split(' ')
                .map(capital)
                .collect::<Vec<_>>()
                .join(" ")
                .into(),
            shown: b.languages.contains(name),
        })
        .collect();
    ui.set_language_choices(ModelRc::new(VecModel::from(choices)));
}

/// A directory station's tile: its name and country, and its picture when
/// it has been seen this session.
fn station_tile(b: &Browser, station: &Station) -> AlbumTile {
    let country = country_name(b, &station.countrycode);
    let mut tile = AlbumTile {
        title: station.name.trim().into(),
        names: names(
            (!country.is_empty())
                .then(|| unlinked(country.to_owned()))
                .into_iter()
                .collect(),
        ),
        logo: true,
        ..AlbumTile::default()
    };
    if let Some(image) = b.pictures.get(&(station.favicon.clone(), 0)) {
        show_picture(&mut tile, image.clone());
    }
    tile
}

/// Puts a picture on a tile; a station's logo also tells which plate its ink
/// wants.
fn show_picture(tile: &mut AlbumTile, image: Image) {
    if tile.logo {
        tile.dark_plate = logo_ink::of_image(&image);
    }
    tile.cover = image;
}

/// A genre's name as the radio page matches it, whatever its case and
/// punctuation.
fn genre_key(name: &str) -> String {
    (name.chars())
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The directory's tag for a library genre: the radio page's shelf whose
/// title or tag it is, such as "world music" for World, else its own name.
fn directory_tag(genre: &str) -> String {
    let wanted = genre_key(genre);
    (GENRES.iter())
        .find(|(title, tag)| {
            !tag.is_empty() && [title, tag].iter().any(|name| genre_key(name) == wanted)
        })
        .map_or_else(|| genre.to_lowercase(), |(_, tag)| (*tag).to_owned())
}

/// A genre as a shelf's title writes it: the radio page's spelling for one
/// it has ("R&B"), else the name with a capital.
fn genre_title(name: &str) -> String {
    let wanted = genre_key(name);
    let known = GENRES
        .iter()
        .find(|(title, tag)| !tag.is_empty() && genre_key(title) == wanted);
    known.map_or_else(
        || {
            let mut letters = name.chars();
            letters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(letters).collect()
            })
        },
        |(title, _)| (*title).to_owned(),
    )
}

/// How many albums the library has of each genre by `genre_key`, a genre
/// split by "/", ";" or "," counting for each of its parts.
fn albums_per_genre(library: &[Genre]) -> HashMap<String, i64> {
    let mut albums: HashMap<String, i64> = HashMap::new();
    for genre in library {
        for part in genre.value.split(['/', ';', ',']) {
            *albums.entry(genre_key(part)).or_default() += genre.album_count;
        }
    }
    albums
}

/// The genres' places on the radio page: Top worldwide first, then the
/// genres with the most albums in the library, the rest as they are. A
/// library genre counts when its name, or a part of it split by "/", ";" or
/// ",", is a shelf's title or tag, whatever its case and punctuation.
fn genre_ranks(library: &[Genre]) -> Vec<usize> {
    let key = genre_key;
    let albums = albums_per_genre(library);
    let count = |(title, tag): (&str, &str)| match tag {
        "" => i64::MAX,
        tag => [title, tag]
            .map(|name| albums.get(&key(name)).copied().unwrap_or(0))
            .into_iter()
            .max()
            .unwrap_or(0),
    };
    let mut order: Vec<usize> = (0..GENRES.len()).collect();
    order.sort_by_key(|&genre| std::cmp::Reverse(count(GENRES[genre])));
    let mut ranks = vec![0; GENRES.len()];
    for (rank, genre) in order.into_iter().enumerate() {
        ranks[genre] = rank;
    }
    ranks
}

/// The radio page's shelves of the directory, each shown once it comes, in
/// the order of `place`; asked for on the first visit, and again for those
/// that didn't come. A shelf without stations isn't shown.
fn directory_shelves(ui: &AppWindow, browser: &Shared) {
    {
        let mut b = browser.borrow_mut();
        ui.set_station_shelves(ModelRc::from(b.shelf_model.clone()));
        show_languages(ui, &b);
        if b.local.is_none() {
            b.local = zone::country().map(str::to_owned);
        }
    }
    let Some(directory) = directory(browser) else {
        return;
    };
    let (missing, client) = {
        let mut b = browser.borrow_mut();
        let local = b.local.is_some();
        let missing: Vec<usize> = (0..=LOCAL)
            .filter(|shelf| *shelf < LOCAL || local)
            .filter(|shelf| b.shelves_asked.insert(*shelf))
            .collect();
        (missing, b.client.clone())
    };
    if missing.is_empty() {
        return;
    }
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        if browser.borrow().shelf_ranks.is_empty() {
            let library = match client {
                Some(client) => client.get_genres().await.map(|g| g.genre),
                None => Ok(Vec::new()),
            };
            browser.borrow_mut().shelf_ranks = genre_ranks(&library.unwrap_or_default());
        }
        learn(&browser, &directory).await;
        let Some(ui) = weak.upgrade() else { return };
        show_languages(&ui, &browser.borrow());
        for shelf in missing {
            fill_shelf(&ui, &browser, &directory, shelf);
        }
    })
    .expect("the event loop is running");
}

/// Asks the directory for a shelf's stations and shows them, unless the
/// languages changed meanwhile.
fn fill_shelf(
    ui: &AppWindow,
    browser: &Shared,
    directory: &Rc<radiobrowser::Client>,
    shelf: usize,
) {
    let (id, languages, round) = {
        let b = browser.borrow();
        let Some(id) = shelf_id(&b, shelf) else {
            return;
        };
        (id, b.languages.clone(), b.shelf_round)
    };
    let (weak, browser, directory) = (ui.as_weak(), browser.clone(), directory.clone());
    slint::spawn_local(async move {
        let found = shelf_stations(&directory, &id, &languages).await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().shelf_round != round {
            return;
        }
        match found {
            Ok(stations) => show_shelf(&browser, shelf, &id, stations),
            Err(e) => {
                browser.borrow_mut().shelves_asked.remove(&shelf);
                if ui.get_page() == Page::Radio && ui.get_page_problem().is_empty() {
                    let problem = format!("The radio directory didn't answer: {e}");
                    ui.set_page_problem(problem.into());
                }
            }
        }
    })
    .expect("the event loop is running");
}

/// Shows a shelf's stations in its place, or in place of what it showed,
/// with the pictures it showed asked for again; one with no stations goes.
fn show_shelf(browser: &Shared, shelf: usize, id: &str, stations: Vec<Station>) {
    let mut b = browser.borrow_mut();
    let tiles: Vec<AlbumTile> = (stations.iter().enumerate())
        .map(|(row, s)| AlbumTile {
            link: format!("{STATION_LINK}shelf:{shelf}:{row}").into(),
            ..station_tile(&b, s)
        })
        .collect();
    let model = b.shelf_model.clone();
    let shown = model.iter().position(|row| row.shelf as usize == shelf);
    let at = (model.iter())
        .filter(|row| place(&b, row.shelf as usize) < place(&b, shelf))
        .count();
    let seen = (b.logos_asked.iter())
        .filter(|(asked, _)| *asked == shelf)
        .map(|(_, row)| row + 1)
        .max()
        .unwrap_or(0);
    b.logos_asked.retain(|(asked, _)| *asked != shelf);
    let row = StationShelf {
        title: page_title(&b, id).into(),
        link: format!("stations:{id}").into(),
        shelf: shelf as i32,
        tiles: ModelRc::new(VecModel::from(tiles)),
    };
    let empty = stations.is_empty();
    b.shelves.insert(shelf, stations);
    drop(b);
    match (shown, empty) {
        (Some(shown), true) => {
            model.remove(shown);
        }
        (Some(shown), false) => model.set_row_data(shown, row),
        (None, false) => model.insert(at, row),
        (None, true) => {}
    }
    shelf_logos(browser, shelf, 0..seen);
}

/// Puts a language's stations first on the genre shelves, or no longer, and
/// asks for the shelves again once the ticking pauses: all of them, since
/// the change drops those still on their way.
fn toggle_language(ui: &AppWindow, browser: &Shared, index: usize) {
    {
        let mut b = browser.borrow_mut();
        let Some(name) = b.language_names.get(index).cloned() else {
            return;
        };
        match b.languages.iter().position(|picked| *picked == name) {
            Some(picked) => {
                b.languages.remove(picked);
            }
            None => b.languages.push(name),
        }
        b.shelf_round += 1;
        show_languages(ui, &b);
    }
    show_catalogue(ui, browser);
    save_prefs(ui, browser);
    let (weak, shared) = (ui.as_weak(), Rc::downgrade(browser));
    let refill = move || {
        let (Some(ui), Some(browser)) = (weak.upgrade(), shared.upgrade()) else {
            return;
        };
        let Some(directory) = directory(&browser) else {
            return;
        };
        for shelf in 0..=LOCAL {
            browser.borrow_mut().shelves_asked.insert(shelf);
            fill_shelf(&ui, &browser, &directory, shelf);
        }
    };
    let b = browser.borrow();
    b.language_wait
        .start(TimerMode::SingleShot, LANGUAGE_WAIT, refill);
}

/// The tiles of a shelf of stations: the genre page's, the search's, or one
/// the radio page shows.
fn shelf_tiles(b: &Browser, shelf: usize) -> Option<ModelRc<AlbumTile>> {
    if shelf == GENRE_RADIO {
        return Some(ModelRc::from(b.genre_radio.clone()));
    }
    if shelf == search::RADIO {
        return Some(ModelRc::from(b.search.radio.clone()));
    }
    let shown = b.shelf_model.iter().find(|row| row.shelf as usize == shelf);
    shown.map(|row| row.tiles)
}

/// Fetches the pictures of a shelf's tiles in `rows`, the ones on screen,
/// that haven't been asked for.
fn shelf_logos(browser: &Shared, shelf: usize, rows: Range<usize>) {
    let (wanted, tiles) = {
        let mut b = browser.borrow_mut();
        let (Some(tiles), Some(stations)) = (shelf_tiles(&b, shelf), b.shelves.get(&shelf)) else {
            return;
        };
        let wanted: Vec<(usize, String)> = rows
            .filter_map(|row| Some((row, stations.get(row)?.favicon.clone())))
            .filter(|(_, favicon)| !favicon.is_empty())
            .collect();
        let wanted: Vec<_> = wanted
            .into_iter()
            .filter(|(row, _)| b.logos_asked.insert((shelf, *row)))
            .collect();
        (wanted, tiles)
    };
    if wanted.is_empty() {
        return;
    }
    let browser = browser.clone();
    slint::spawn_local(async move {
        for (row, favicon) in wanted {
            let Some(image) = logo(&browser, favicon).await else {
                continue;
            };
            if let Some(mut tile) = tiles.row_data(row) {
                show_picture(&mut tile, image);
                tiles.set_row_data(row, tile);
            }
        }
    })
    .expect("the event loop is running");
}

/// A station's picture from the address the directory gives for it, kept
/// for the session.
// ponytail: decoded at full size; scale down when big pictures cost memory.
async fn logo(browser: &Shared, address: String) -> Option<Image> {
    let key = (address, 0);
    let directory = {
        let b = browser.borrow();
        if let Some(image) = b.pictures.get(&key) {
            return Some(image.clone());
        }
        b.directory.clone()?
    };
    let bytes = directory.logo(&key.0).await.ok().flatten();
    let image = bytes.and_then(|bytes| Image::load_from_data(&bytes, None).ok());
    // One that didn't come is kept as none, so it isn't asked for again.
    browser
        .borrow_mut()
        .pictures
        .insert(key, image.clone().unwrap_or_default());
    image
}

/// The genre page's next stations, unless they are on their way or the
/// directory has no more: first a genre's stations in the languages picked,
/// in turn, then worldwide ones, each once.
fn more_stations(ui: &AppWindow, browser: &Shared) {
    let (id, languages, first, offset, visit) = {
        let mut b = browser.borrow_mut();
        if b.genre_loading || !b.genre_more {
            return;
        }
        b.genre_loading = true;
        let first = b.genre_stations.is_empty();
        (
            b.genre.clone(),
            b.languages.clone(),
            first,
            b.genre_offset,
            b.visit,
        )
    };
    let Some(directory) = directory(browser) else {
        return;
    };
    ui.set_loading(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        learn(&browser, &directory).await;
        let found = async {
            let lists = match first {
                true => in_languages(&directory, &id, &languages, GENRE_IN_LANGUAGE).await?,
                false => Vec::new(),
            };
            let rest = worldwide(&directory, &id, offset, GENRE_STATIONS).await?;
            Ok::<_, radiobrowser::Error>((lists, rest))
        }
        .await;
        let Some(ui) = weak.upgrade() else { return };
        let mut b = browser.borrow_mut();
        if b.visit != visit {
            return;
        }
        b.genre_loading = false;
        ui.set_loading(false);
        match found {
            Ok((lists, rest)) => {
                b.genre_more = rest.len() == GENRE_STATIONS as usize;
                b.genre_offset += GENRE_STATIONS;
                ui.set_more_tiles(b.genre_more);
                let mut seen = HashSet::new();
                merge(&[], &b.genre_stations, &mut seen);
                let stations = merge(&lists, &rest, &mut seen);
                let before = b.genre_stations.len();
                let tiles: Vec<_> = (stations.iter().enumerate())
                    .map(|(row, s)| {
                        let favicon = Some(s.favicon.clone()).filter(|f| !f.is_empty());
                        let link = format!("{STATION_LINK}genre:{}", before + row).into();
                        (
                            AlbumTile {
                                link,
                                ..station_tile(&b, s)
                            },
                            favicon,
                        )
                    })
                    .collect();
                b.genre_stations.extend(stations);
                drop(b);
                add_tiles(&ui, &browser, tiles);
            }
            Err(e) => {
                let problem = format!("The radio directory didn't answer: {e}");
                ui.set_page_problem(problem.into());
            }
        }
    })
    .expect("the event loop is running");
}

/// Asks the server for its curated stations, once per sign-in, where it
/// offers them, and shows them on the radio page.
fn catalogue_stations(ui: &AppWindow, browser: &Shared) {
    let client = {
        let mut b = browser.borrow_mut();
        ui.set_catalogue_shelves(ModelRc::from(b.catalogue_model.clone()));
        let client = b
            .client
            .clone()
            .filter(|client| client.supports(RADIO_CATALOGUE));
        match client {
            Some(client) if !b.catalogue_asked => {
                b.catalogue_asked = true;
                client
            }
            _ => return,
        }
    };
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let found = client.get_radio_catalogue().await;
        // The library's genres only order the shelves; without them the
        // order is by stations.
        let library = client
            .get_genres()
            .await
            .map(|g| g.genre)
            .unwrap_or_default();
        let Some(ui) = weak.upgrade() else { return };
        match found {
            Ok(list) => {
                {
                    let mut b = browser.borrow_mut();
                    b.catalogue = list.internet_radio_station;
                    b.catalogue_albums = albums_per_genre(&library);
                }
                show_catalogue(&ui, &browser);
            }
            Err(e) => {
                browser.borrow_mut().catalogue_asked = false;
                if ui.get_page() == Page::Radio && ui.get_page_problem().is_empty() {
                    ui.set_page_problem(format!("The station catalogue didn't answer: {e}").into());
                }
            }
        }
    })
    .expect("the event loop is running");
}

/// Whether the server has saved a station that streams from this address.
fn is_saved(b: &Browser, stream_url: &str) -> bool {
    b.server_stations
        .iter()
        .any(|saved| saved.stream_url == stream_url)
}

/// The catalogue as shelves on the radio page, with the pictures that were
/// shown asked for again; run again when the languages or the saved
/// stations change.
fn show_catalogue(ui: &AppWindow, browser: &Shared) {
    let size = tile_pixels(ui);
    let seen: Vec<(usize, usize)> = {
        let mut b = browser.borrow_mut();
        if b.catalogue.is_empty() {
            return;
        }
        let albums_in = |genre: &str| {
            b.catalogue_albums
                .get(&genre_key(genre))
                .copied()
                .unwrap_or(0)
        };
        let shelves = catalogue::shelves(
            &b.catalogue,
            &b.languages,
            b.local.as_deref(),
            albums_in,
            genre_title,
        );
        let rows: Vec<StationShelf> = (shelves.iter().enumerate())
            .map(|(shelf, found)| StationShelf {
                title: found.title.as_str().into(),
                link: "".into(),
                shelf: shelf as i32,
                tiles: ModelRc::new(VecModel::from(
                    (found.stations.iter().enumerate())
                        .map(|(row, &place)| AlbumTile {
                            link: format!("{STATION_LINK}catalogue:{shelf}:{row}").into(),
                            ..catalogue_tile(&b, &b.catalogue[place], size)
                        })
                        .collect::<Vec<_>>(),
                )),
            })
            .collect();
        b.catalogue_shelves = shelves;
        b.catalogue_model.set_vec(rows);
        b.catalogue_logos_asked.drain().collect()
    };
    for shelf in seen.iter().map(|(shelf, _)| *shelf).collect::<HashSet<_>>() {
        let furthest = seen
            .iter()
            .filter(|(asked, _)| *asked == shelf)
            .map(|(_, row)| row + 1)
            .max();
        catalogue_logos(ui, browser, shelf, 0..furthest.unwrap_or(0));
    }
}

/// A catalogue station's tile: its name and home page, its picture when this
/// session has it, and whether the server has it saved.
fn catalogue_tile(b: &Browser, station: &CatalogueStation, size: u32) -> AlbumTile {
    let address = station
        .home_page_url
        .as_deref()
        .map(bare_address)
        .unwrap_or_default();
    let mut tile = AlbumTile {
        title: station.name.trim().into(),
        names: names(
            (!address.is_empty())
                .then(|| unlinked(address.to_owned()))
                .into_iter()
                .collect(),
        ),
        logo: true,
        saved: is_saved(b, &station.stream_url),
        ..AlbumTile::default()
    };
    if let Some(image) = b.pictures.get(&(catalogue_cover(station), size)) {
        show_picture(&mut tile, image.clone());
    }
    tile
}

/// The id the server serves a catalogue station's logo under.
fn catalogue_cover(station: &CatalogueStation) -> String {
    station
        .cover_art
        .clone()
        .filter(|cover| !cover.is_empty())
        .unwrap_or_else(|| station.id.clone())
}

/// Fetches the logos of a catalogue shelf's tiles in `rows`, the ones on
/// screen, that haven't been asked for.
fn catalogue_logos(ui: &AppWindow, browser: &Shared, shelf: usize, rows: Range<usize>) {
    let size = tile_pixels(ui);
    let (wanted, tiles) = {
        let mut b = browser.borrow_mut();
        let tiles = (b.catalogue_model.iter())
            .find(|row| row.shelf as usize == shelf)
            .map(|row| row.tiles);
        let (Some(tiles), Some(found)) = (tiles, b.catalogue_shelves.get(shelf)) else {
            return;
        };
        let places: Vec<(usize, usize)> = rows
            .filter_map(|row| Some((row, *found.stations.get(row)?)))
            .collect();
        let places: Vec<(usize, usize)> = (places.into_iter())
            .filter(|(row, _)| b.catalogue_logos_asked.insert((shelf, *row)))
            .collect();
        let wanted: Vec<(usize, String)> = (places.into_iter())
            .map(|(row, place)| (row, catalogue_cover(&b.catalogue[place])))
            .collect();
        (wanted, tiles)
    };
    let browser = browser.clone();
    slint::spawn_local(async move {
        for (row, cover) in wanted {
            let Some(image) = cover_art(&browser, None, cover, size).await.flatten() else {
                continue;
            };
            if let Some(mut tile) = tiles.row_data(row) {
                show_picture(&mut tile, image);
                tiles.set_row_data(row, tile);
            }
        }
    })
    .expect("the event loop is running");
}

/// Saves a catalogue station on the server, unless it has it.
fn save_catalogue_station(ui: &AppWindow, browser: &Shared, shelf: usize, index: usize) {
    let station = {
        let b = browser.borrow();
        let place = b
            .catalogue_shelves
            .get(shelf)
            .and_then(|found| found.stations.get(index));
        place.map(|&place| b.catalogue[place].clone())
    };
    let Some(station) = station else { return };
    let homepage = station.home_page_url.unwrap_or_default();
    save_new_station(
        ui,
        browser,
        station.name,
        vec![station.stream_url],
        homepage,
    );
}

/// The address a directory station streams from: the stream itself, with
/// playlists and redirects followed, else the one the station gave.
fn stream_address(station: &Station) -> &str {
    match station.url_resolved.as_str() {
        "" => &station.url,
        resolved => resolved,
    }
}

/// The start of a station tile's link: playing it, rather than going to a
/// page. What follows says where the station is: `saved:<row>` among the
/// server's, `shelf:<shelf>:<row>` or `genre:<row>` among the directory's, and
/// `catalogue:<shelf>:<row>` among the curated catalogue's.
const STATION_LINK: &str = "station:";

/// The station a tile's link names, with the picture the tile shows.
pub(crate) fn tile_station(
    ui: &AppWindow,
    browser: &Shared,
    link: &str,
) -> Option<(crate::radio::Station, slint::Image)> {
    let mut place = link.strip_prefix(STATION_LINK)?.split(':');
    let kind = place.next()?;
    let mut number = || place.next()?.parse::<usize>().ok();
    let b = browser.borrow();
    let (name, stream, tile) = match kind {
        "saved" => {
            let row = number()?;
            let station = b.server_stations.get(row)?;
            let tile = b.saved.row_data(row);
            (station.name.clone(), station.stream_url.clone(), tile)
        }
        "shelf" => {
            let (shelf, row) = (number()?, number()?);
            let station = b.shelves.get(&shelf)?.get(row)?;
            let tile = shelf_tiles(&b, shelf).and_then(|tiles| tiles.row_data(row));
            (
                station.name.trim().to_owned(),
                stream_address(station).to_owned(),
                tile,
            )
        }
        "catalogue" => {
            let (shelf, row) = (number()?, number()?);
            let place = *b.catalogue_shelves.get(shelf)?.stations.get(row)?;
            let station = b.catalogue.get(place)?;
            let shown = b.catalogue_model.iter().find(|s| s.shelf as usize == shelf);
            let tile = shown.and_then(|s| s.tiles.row_data(row));
            (
                station.name.trim().to_owned(),
                station.stream_url.clone(),
                tile,
            )
        }
        "genre" => {
            let row = number()?;
            let station = b.genre_stations.get(row)?;
            let tile = ui.get_tiles().row_data(row);
            (
                station.name.trim().to_owned(),
                stream_address(station).to_owned(),
                tile,
            )
        }
        _ => return None,
    };
    let station = crate::radio::Station { name, stream };
    Some((station, tile.map(|tile| tile.cover).unwrap_or_default()))
}

/// Saves a station of a shelf, or of the genre page when `shelf` is -1, on
/// the server, unless the server has its stream already.
fn save_station(ui: &AppWindow, browser: &Shared, shelf: i32, index: usize) {
    let station = {
        let b = browser.borrow();
        let stations = match usize::try_from(shelf) {
            Ok(shelf) => b.shelves.get(&shelf),
            Err(_) => Some(&b.genre_stations),
        };
        stations.and_then(|stations| stations.get(index)).cloned()
    };
    let Some(station) = station else { return };
    let name = station.name.trim().to_owned();
    let streams = vec![stream_address(&station).to_owned(), station.url.clone()];
    let homepage = station.homepage.trim().to_owned();
    save_new_station(ui, browser, name, streams, homepage);
}

/// Saves a station on the server, streaming from the first of `streams`,
/// unless the server has one streaming from any of them: as the server has
/// them now, since those shown may be from up to an hour ago.
fn save_new_station(
    ui: &AppWindow,
    browser: &Shared,
    name: String,
    streams: Vec<String>,
    homepage: String,
) {
    let saved_already = format!("{name} is saved already.");
    if streams_one(&browser.borrow().server_stations, &streams) {
        return notify(ui, browser, saved_already);
    }
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let saved = client
            .asking_the_server()
            .get_internet_radio_stations()
            .await;
        let Some(ui) = weak.upgrade() else { return };
        if saved.is_ok_and(|saved| streams_one(&saved.internet_radio_station, &streams)) {
            notify(&ui, &browser, saved_already);
            return show_stations_anew(&ui, &browser);
        }
        let done = format!("Saved {name}.");
        change_stations(&ui, &browser, done, |client| async move {
            let station = station(&name, &streams[0], &homepage);
            client.create_internet_radio_station(&station).await
        });
    })
    .expect("the event loop is running");
}

/// Whether one of the `saved` stations streams from one of `streams`.
fn streams_one(saved: &[InternetRadioStation], streams: &[String]) -> bool {
    (saved.iter()).any(|station| streams.contains(&station.stream_url))
}

/// The stations saved on the server, kept for Save to know them, and shown
/// on the home page as a shelf. A server without radio stations shows none.
fn saved_stations(ui: &AppWindow, browser: &Shared) {
    ask_saved_stations(ui, browser, true);
}

/// Asks for the saved stations as `saved_stations` says, showing aged
/// answers at once where `shows_aged`, and then asking again without.
fn ask_saved_stations(ui: &AppWindow, browser: &Shared, shows_aged: bool) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let reader = client.reader(shows_aged);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let Ok(list) = reader.get_internet_radio_stations().await else {
            return;
        };
        let Some(ui) = weak.upgrade() else { return };
        if shows_aged || reader.heard_news() {
            show_saved(&ui, &browser, list.internet_radio_station);
        }
        if reader.gave_aged() {
            ask_saved_stations(&ui, &browser, false);
        }
    })
    .expect("the event loop is running");
}

/// The stations saved on the server as tiles, with the server's pictures of
/// them, or else the directory's, and as the rows the dialogs fill in from,
/// so the two always line up.
fn show_saved(ui: &AppWindow, browser: &Shared, stations: Vec<InternetRadioStation>) {
    let rows: Vec<StationRow> = (stations.iter())
        .map(|station| {
            let homepage = station.home_page_url.clone().unwrap_or_default();
            let details: Vec<&str> = [homepage.as_str(), station.stream_url.as_str()]
                .into_iter()
                .map(bare_address)
                .filter(|address| !address.is_empty())
                .collect();
            StationRow {
                details: details.join(" · ").into(),
                id: station.id.as_str().into(),
                name: station.name.as_str().into(),
                stream: station.stream_url.as_str().into(),
                homepage: homepage.into(),
            }
        })
        .collect();
    ui.set_station_rows(ModelRc::new(VecModel::from(rows)));
    let size = tile_pixels(ui);
    let (tiles, wanted) = {
        let mut b = browser.borrow_mut();
        b.server_stations = stations;
        b.saved = Rc::default();
        let mut wanted = Vec::new();
        for (row, station) in b.server_stations.iter().enumerate() {
            let address = station.home_page_url.as_deref().map(bare_address);
            let cover = station.cover_art.clone().filter(|cover| !cover.is_empty());
            let known = b
                .shelves
                .values()
                .flatten()
                .chain(&b.genre_stations)
                .find(|s| s.url_resolved == station.stream_url || s.url == station.stream_url)
                .map(|s| Some(s.favicon.clone()))
                .or_else(|| b.favicons.get(&station.stream_url).cloned());
            let seen = match &cover {
                Some(cover) => Some((cover.clone(), size)),
                None => known.clone().flatten().map(|favicon| (favicon, 0)),
            };
            let mut tile = AlbumTile {
                title: station.name.as_str().into(),
                names: names(
                    address
                        .filter(|a| !a.is_empty())
                        .map(|a| unlinked(a.to_owned()))
                        .into_iter()
                        .collect(),
                ),
                link: format!("{STATION_LINK}saved:{row}").into(),
                logo: true,
                ..AlbumTile::default()
            };
            if let Some(image) = seen.and_then(|key| b.pictures.get(&key).cloned()) {
                show_picture(&mut tile, image);
            }
            b.saved.push(tile);
            let (name, stream) = (station.name.clone(), station.stream_url.clone());
            wanted.push((row, name, stream, known, cover));
        }
        ui.set_saved_stations(ModelRc::from(b.saved.clone()));
        // Only the stations still saved, and still without the server's
        // picture, keep the directory's.
        let logos = b.station_logos.get_or_insert_with(prefs::station_logos);
        let before = logos.len();
        logos.retain(|kept, _| {
            (wanted.iter()).any(|(_, _, stream, _, cover)| cover.is_none() && stream == kept)
        });
        if logos.len() < before {
            prefs::save_station_logos(logos);
        }
        (b.saved.clone(), wanted)
    };
    // The catalogue shows which of its stations are saved now.
    show_catalogue(ui, browser);
    let browser = browser.clone();
    slint::spawn_local(async move {
        for (row, name, stream, known, cover) in wanted {
            let image = match cover {
                Some(cover) => cover_art(&browser, None, cover, size).await.flatten(),
                None => found_logo(&browser, &name, &stream, known).await,
            };
            if let (Some(image), Some(mut tile)) = (image, tiles.row_data(row)) {
                show_picture(&mut tile, image);
                tiles.set_row_data(row, tile);
            }
        }
    })
    .expect("the event loop is running");
}

/// A saved station's picture where its server has none: the one this device
/// kept, or else the directory's, which the device then keeps. A station the
/// directory had no picture of is asked about again after a week.
async fn found_logo(
    browser: &Shared,
    name: &str,
    stream: &str,
    known: Option<Option<String>>,
) -> Option<Image> {
    let kept = {
        let mut b = browser.borrow_mut();
        let logos = b.station_logos.get_or_insert_with(prefs::station_logos);
        logos.get(stream).cloned()
    };
    if let Some(kept) = kept {
        let image = kept
            .bytes()
            .and_then(|bytes| Image::load_from_data(&bytes, None).ok());
        if let Some(image) = image {
            let mut b = browser.borrow_mut();
            b.favicons
                .insert(stream.to_owned(), Some(kept.address.clone()));
            b.pictures.insert((kept.address, 0), image.clone());
            return Some(image);
        }
        if kept.missing_lately() {
            return None;
        }
    }
    let directory = directory(browser)?;
    let favicon = match known {
        Some(favicon) => favicon,
        // The directory's station of the same name that streams from the
        // same address; a lookup that failed is tried again later.
        None => {
            let found = directory.named(name).await.ok()?;
            let same = (found.into_iter()).find(|s| s.url == stream || s.url_resolved == stream);
            let favicon = same.map(|s| s.favicon);
            browser
                .borrow_mut()
                .favicons
                .insert(stream.to_owned(), favicon.clone());
            favicon
        }
    };
    let Some(favicon) = favicon.filter(|favicon| !favicon.is_empty()) else {
        keep_logo(browser, stream, prefs::Logo::missing());
        return None;
    };
    // A picture that didn't come is asked for again next session.
    let key = (favicon.clone(), 0);
    let failed = browser
        .borrow()
        .pictures
        .get(&key)
        .map(|image| image.size().width == 0);
    if failed == Some(true) {
        return None;
    }
    let Ok(bytes) = directory.logo(&favicon).await else {
        browser.borrow_mut().pictures.insert(key, Image::default());
        return None;
    };
    let image = bytes
        .as_deref()
        .and_then(|bytes| Image::load_from_data(bytes, None).ok());
    match (&bytes, &image) {
        (Some(bytes), Some(image)) => {
            browser.borrow_mut().pictures.insert(key, image.clone());
            keep_logo(browser, stream, prefs::Logo::found(favicon, bytes));
        }
        _ => keep_logo(browser, stream, prefs::Logo::missing()),
    }
    image
}

fn keep_logo(browser: &Shared, stream: &str, logo: prefs::Logo) {
    let mut b = browser.borrow_mut();
    let logos = b.station_logos.get_or_insert_with(prefs::station_logos);
    logos.insert(stream.to_owned(), logo);
    prefs::save_station_logos(logos);
}

/// An address as people write it: "radio.example.org/live", without
/// "https://" or a trailing slash.
fn bare_address(address: &str) -> &str {
    let address = address.trim();
    let address = address
        .strip_prefix("https://")
        .or_else(|| address.strip_prefix("http://"))
        .unwrap_or(address);
    address.strip_suffix('/').unwrap_or(address)
}

/// Why the server didn't do what was asked, in words for the page.
pub fn refusal(e: &opensubsonic::Error) -> String {
    match e {
        opensubsonic::Error::Status(501) => "Your server doesn't let apps do this.".into(),
        e => format!("Your server didn't take that: {e}"),
    }
}

/// "Scanning, 12 items so far…" or "146 items scanned.", from what the
/// server counted.
fn scan_text(status: &ScanStatus) -> String {
    let items = status
        .count
        .map(|n| count(usize::try_from(n).unwrap_or(0), "item", "items"));
    match (status.scanning, items) {
        (true, Some(items)) => format!("Scanning, {items} so far…"),
        (true, None) => "Scanning…".to_owned(),
        (false, Some(items)) => format!("{items} scanned."),
        (false, None) => String::new(),
    }
}

/// A song's page, as the server's getSong has it: its album's cover, its
/// title and artists, and under them every field it has, named as the tracks
/// table's columns are.
fn song(ui: &AppWindow, browser: &Shared, id: String) {
    let (kept, offline) = (downloads(browser), is_offline(browser));
    load(
        ui,
        browser,
        move |client| {
            let (id, kept) = (id.clone(), kept.clone());
            async move {
                let call = client.get_song(&id);
                saved_or(offline, || kept.song(&id), call).await
            }
        },
        |ui, browser, song| {
            ui.set_header_title(song.title.as_str().into());
            ui.set_header_names(names(song_names(&song)));
            let facts = [
                song.year.map(|year| year.to_string()).unwrap_or_default(),
                song.duration.map(duration).unwrap_or_default(),
            ];
            let facts: Vec<String> = facts.into_iter().filter(|f| !f.is_empty()).collect();
            ui.set_header_facts(facts.join(" · ").into());
            let mut details: Vec<Detail> = COLUMNS
                .iter()
                .filter(|column| column.title != TITLE)
                .map(|column| (column.title, (column.cell)(&song)))
                .filter(|(_, cell)| !cell.is_empty())
                .map(|(label, cell)| Detail {
                    label: label.into(),
                    names: names(cell),
                })
                .collect();
            let tags: Vec<Name> = {
                let b = browser.borrow();
                let nodes = tag_nodes(&b.tags);
                let known = song.tag_ids.iter();
                let known = known.filter(|id| nodes.iter().any(|node| node.0 == id.as_str()));
                known
                    .map(|id| Name {
                        label: path(&nodes, id).into(),
                        link: format!("tag:{id}").into(),
                    })
                    .collect()
            };
            if !tags.is_empty() {
                details.push(Detail {
                    label: "Tags".into(),
                    names: names(tags),
                });
            }
            ui.set_details(ModelRc::new(VecModel::from(details)));
            header_image(ui, browser, song.cover_art);
        },
    );
}

/// The folders page: at "folders" the music folders' top level, at
/// "folder:<id>" what is in that folder, as the server's file structure has
/// it; or at "tag:<id>" a tag's page, with the songs that have it or a tag
/// under it. Each is a table like the tracks page's, whose titles open a
/// folder or a song's page. A folder shown again, as when filtering, isn't
/// asked for again. A folder kept on this device lists its songs while the
/// server can't be reached.
fn folders(ui: &AppWindow, browser: &Shared, link: String) {
    show_columns(ui, browser);
    let tag = link.strip_prefix("tag:").map(str::to_owned);
    if let Some(tag) = &tag {
        ui.set_shown_id(tag.as_str().into());
    }
    let held = {
        let b = browser.borrow();
        (b.folder == link).then(|| b.folder_name.clone())
    };
    if let Some(name) = held {
        ui.set_header_title(name.into());
        tag_header(ui, &browser.borrow());
        arrange_tracks(ui, browser);
        return;
    }
    ui.set_folder_filter("".into());
    {
        let mut b = browser.borrow_mut();
        b.folder.clear();
        b.entries.clear();
        b.view.clear();
    }
    if tag.is_some() {
        tag_header(ui, &browser.borrow());
    }
    let id = link.strip_prefix("folder:").map(str::to_owned);
    let (kept, offline) = (downloads(browser), is_offline(browser));
    load(
        ui,
        browser,
        move |client| {
            let (tag, id, kept) = (tag.clone(), id.clone(), kept.clone());
            async move {
                if let Some(tag) = tag {
                    let tag = client.get_tag(&tag).await?;
                    return Ok((tag.tag.name, tag.song));
                }
                let Some(id) = id else {
                    let indexes = client.get_indexes(None, None).await?;
                    return Ok((
                        "Folders".to_owned(),
                        indexes.map(top_level).unwrap_or_default(),
                    ));
                };
                let call = client.get_music_directory(&id);
                let folder = saved_or(offline, || kept.folder(&id), call).await?;
                Ok((folder.name, folder.child))
            }
        },
        move |ui, browser, (name, entries)| {
            {
                let mut b = browser.borrow_mut();
                b.folder = link.clone();
                b.folder_name = name.clone();
                b.entries = entries;
            }
            ui.set_header_title(name.into());
            tag_header(ui, &browser.borrow());
            arrange_tracks(ui, browser);
        },
    );
}

/// On a tag's page, the tags under it, to open, and where it can be moved,
/// from the tags known; its name too, once they are.
fn tag_header(ui: &AppWindow, b: &Browser) {
    if ui.get_page() != Page::Tag {
        return;
    }
    let id = ui.get_shown_id();
    let nodes = tag_nodes(&b.tags);
    let Some(&(_, name, parent)) = nodes.iter().find(|node| node.0 == id.as_str()) else {
        return;
    };
    ui.set_header_title(name.into());
    let under = nodes
        .iter()
        .filter(|node| node.2 == Some(id.as_str()))
        .map(|&(child, name, _)| Name {
            label: name.into(),
            link: format!("tag:{child}").into(),
        });
    let mut under: Vec<Name> = under.collect();
    under.sort_by_key(|name| name.label.to_lowercase());
    ui.set_header_names(ModelRc::new(VecModel::from(under)));
    let targets = destinations(&nodes, Some(&id), parent);
    ui.set_move_targets(ModelRc::new(VecModel::from(targets)));
}

/// Asks for the songs of the tag on screen again, keeping those shown until
/// they come.
fn refresh_tag(ui: &AppWindow, browser: &Shared) {
    let link = browser.borrow().folder.clone();
    let Some(id) = link.strip_prefix("tag:").map(str::to_owned) else {
        return;
    };
    if ui.get_page() != Page::Tag {
        return;
    }
    load(
        ui,
        browser,
        move |client| {
            let id = id.clone();
            async move { client.get_tag(&id).await }
        },
        move |ui, browser, tag| {
            {
                let mut b = browser.borrow_mut();
                if b.folder != link {
                    return;
                }
                b.entries = tag.song;
                clear_lines(ui, &mut b);
            }
            arrange_tracks(ui, browser);
        },
    );
}

/// The music folders' top level as entries of a folder: the folders under
/// each index letter, then the files beside them.
fn top_level(indexes: Indexes) -> Vec<Child> {
    let folders = indexes.index.into_iter().flat_map(|index| index.artist);
    folders
        .map(|folder| Child {
            id: folder.id,
            is_dir: true,
            title: folder.name,
            starred: folder.starred,
            user_rating: folder.user_rating,
            average_rating: folder.average_rating,
            ..Child::default()
        })
        .chain(indexes.child)
        .collect()
}

/// A composer's page: their picture and name, and their compositions. The
/// picture comes with the composers, asked for first if they aren't here.
fn composer(ui: &AppWindow, browser: &Shared, id: String) {
    let known = browser.borrow().composers_loaded;
    let wanted = id.clone();
    load(
        ui,
        browser,
        move |client| {
            let wanted = wanted.clone();
            async move {
                let composers = if known {
                    None
                } else {
                    Some(client.get_composers().await?)
                };
                Ok((composers, client.get_compositions(Some(&wanted)).await?))
            }
        },
        move |ui, browser, (composers, compositions)| {
            let mut b = browser.borrow_mut();
            if let Some(composers) = composers {
                b.composers = composers
                    .index
                    .into_iter()
                    .flat_map(|index| index.composer)
                    .collect();
                b.composers_loaded = true;
            }
            let pieces = compositions.composition;
            let named = pieces
                .iter()
                .flat_map(|piece| &piece.composers)
                .chain(&b.composers)
                .find(|composer| composer.id == id);
            ui.set_header_title(named.map(|c| c.name.as_str()).unwrap_or_default().into());
            ui.set_header_facts(count(pieces.len(), "composition", "compositions").into());
            let picture = b
                .composers
                .iter()
                .find(|composer| composer.id == id)
                .and_then(|composer| composer.cover_art.clone());
            let rows = pieces.iter().map(|piece| composition_row(piece, Some(&id)));
            b.tracks.set_vec(rows.collect::<Vec<_>>());
            drop(b);
            header_image(ui, browser, picture);
        },
    );
}

/// A composition's page: who wrote it and when, and a row for each album
/// that holds a performance of it, which opens that album.
fn composition(ui: &AppWindow, browser: &Shared, id: String) {
    load(
        ui,
        browser,
        move |client| {
            let id = id.clone();
            async move { client.get_composition(&id).await }
        },
        |ui, browser, composition| {
            let (rows, covers): (Vec<Track>, Vec<Option<String>>) =
                performances(&composition.song).into_iter().unzip();
            ui.set_header_title(composition.name.as_str().into());
            ui.set_header_names(names(
                composition.composers.iter().map(composer_name).collect(),
            ));
            let movements = match composition.movement.len() {
                0 => String::new(),
                n => count(n, "movement", "movements"),
            };
            let facts = [
                years(composition.composed.as_ref()),
                movements,
                count(rows.len(), "performance", "performances"),
            ];
            let facts: Vec<String> = facts.into_iter().filter(|f| !f.is_empty()).collect();
            ui.set_header_facts(facts.join(" · ").into());
            header_image(ui, browser, composition.cover_art);
            {
                // The rows show first and their covers follow, all of them in
                // view as a page holds only a few recordings.
                let mut b = browser.borrow_mut();
                b.tracks.set_vec(rows);
                b.in_view = 0..covers.len();
                for (row, cover) in covers.into_iter().enumerate() {
                    if let Some(cover) = cover {
                        b.covers.insert(row, cover);
                    }
                }
            }
            fetch_covers(ui, browser);
        },
    );
}

/// A row for each album's recordings of a composition, in the order the
/// albums first come: the album and its year, the performers all of them
/// share, when and where they were recorded, and how long they last. Songs
/// the server puts on no album have no row.
// ponytail: one performance per album; split by performers if an album holds two.
fn performances(songs: &[Child]) -> Vec<(Track, Option<String>)> {
    let mut albums: Vec<&Option<String>> = Vec::new();
    for song in songs {
        if !albums.contains(&&song.album_id) {
            albums.push(&song.album_id);
        }
    }
    let mut songs = songs.to_vec();
    songs.sort_by_key(|song| albums.iter().position(|album| **album == song.album_id));
    songs
        .chunk_by(|one, next| one.album_id == next.album_id)
        .filter(|run| run[0].album_id.is_some())
        .map(|run| {
            let performers: Vec<Vec<Name>> = run.iter().map(song_names).collect();
            let shared: Vec<Name> = performers[0]
                .iter()
                .filter(|name| performers.iter().all(|them| them.contains(name)))
                .cloned()
                .collect();
            let length = if run.iter().all(|song| song.duration.is_some()) {
                duration(run.iter().filter_map(|song| song.duration).sum())
            } else {
                String::new()
            };
            let first = &run[0];
            let track = Track {
                title: first.album.clone().unwrap_or_default().into(),
                names: names(shared),
                years: first
                    .year
                    .map(|year| year.to_string())
                    .unwrap_or_default()
                    .into(),
                performed: performed(run).into(),
                duration: length.into(),
                link: first
                    .album_id
                    .as_ref()
                    .map(|id| format!("album:{id}"))
                    .unwrap_or_default()
                    .into(),
                ..Track::default()
            };
            (track, first.cover_art.clone())
        })
        .collect()
}

/// A composition's row: when it was written, its composers but `except`, and
/// how many albums hold it.
fn composition_row(composition: &Composition, except: Option<&str>) -> Track {
    let composers: Vec<Name> = composition
        .composers
        .iter()
        .filter(|composer| Some(composer.id.as_str()) != except)
        .map(composer_name)
        .collect();
    let albums = composition
        .album_count
        .and_then(|n| usize::try_from(n).ok())
        .map(|n| count(n, "album", "albums"));
    Track {
        title: composition.name.as_str().into(),
        names: names(composers),
        years: years(composition.composed.as_ref()).into(),
        duration: albums.unwrap_or_default().into(),
        link: format!("composition:{}", composition.id).into(),
        ..Track::default()
    }
}

/// A composer's tile, with how many compositions they have, and their
/// picture's id.
fn composer_tile(composer: &Composer) -> (AlbumTile, Option<String>) {
    let pieces = composer
        .composition_count
        .and_then(|n| usize::try_from(n).ok())
        .map(|n| unlinked(count(n, "composition", "compositions")));
    let tile = AlbumTile {
        title: composer.name.as_str().into(),
        names: names(pieces.into_iter().collect()),
        link: composer_name(composer).link,
        ..AlbumTile::default()
    };
    (tile, composer.cover_art.clone())
}

/// A composer that links to their page, unless the server gives no id.
fn composer_name(composer: &Composer) -> Name {
    let link = if composer.id.is_empty() {
        String::new()
    } else {
        format!("composer:{}", composer.id)
    };
    Name {
        label: composer.name.as_str().into(),
        link: link.into(),
    }
}

/// A playlist's tile, with how many tracks it has, and its cover's id.
fn playlist_tile(playlist: &Playlist) -> (AlbumTile, Option<String>) {
    let tracks = count(playlist.song_count.max(0) as usize, "track", "tracks");
    let tile = AlbumTile {
        title: playlist.name.as_str().into(),
        names: names(vec![unlinked(tracks)]),
        link: format!("playlist:{}", playlist.id).into(),
        ..AlbumTile::default()
    };
    (tile, playlist.cover_art.clone())
}

/// A playlist folder's tile, with how much is in it.
fn folder_tile(folder: &PlaylistFolder, inside: usize) -> (AlbumTile, Option<String>) {
    let tile = AlbumTile {
        title: folder.name.as_str().into(),
        names: names(vec![unlinked(count(inside, "item", "items"))]),
        link: format!("playlists:{}", folder.id).into(),
        folder: true,
        ..AlbumTile::default()
    };
    (tile, None)
}

/// A music folder's tile, as kept with every song under it, with how many
/// those are, and the cover of the first.
fn music_folder_tile(folder: &Directory) -> (AlbumTile, Option<String>) {
    let tracks = count(folder.child.len(), "track", "tracks");
    let tile = AlbumTile {
        title: folder.name.as_str().into(),
        names: names(vec![unlinked(tracks)]),
        link: format!("folder:{}", folder.id).into(),
        ..AlbumTile::default()
    };
    let cover = folder.child.iter().find_map(|song| song.cover_art.clone());
    (tile, cover)
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

/// The genre whose page is on screen, or whose albums or artists are.
fn shown_genre(b: &Browser) -> &str {
    let link = &b.history[b.at];
    (["genre:", "albums:", "artists:"].iter())
        .find_map(|page| link.strip_prefix(page))
        .unwrap_or_default()
}

/// Whether an album carries `genre`: among its genres, or as its one genre
/// where the server lists none.
fn carries(album: &AlbumID3, genre: &str) -> bool {
    if album.genres.is_empty() {
        album.genre.as_deref() == Some(genre)
    } else {
        album.genres.iter().any(|carried| carried.name == genre)
    }
}

/// An album's main genre among `siblings`: the first of them it lists.
fn main_genre<'a>(album: &'a AlbumID3, siblings: &HashSet<&str>) -> Option<&'a str> {
    if album.genres.is_empty() {
        (album.genre.as_deref()).filter(|genre| siblings.contains(genre))
    } else {
        (album.genres.iter())
            .map(|genre| genre.name.as_str())
            .find(|genre| siblings.contains(genre))
    }
}

/// The genres `genre` is told apart from, itself among them: on servers with
/// Fonoteca's genres those under a genre it is under, or like it under none;
/// else every genre.
fn siblings<'a>(b: &'a Browser, genre: &'a str) -> HashSet<&'a str> {
    let tree = offers(b, GENRE_TREE);
    let parents = (b.genres.iter())
        .find(|shown| shown.value == genre)
        .map(|shown| shown.parents.as_slice())
        .unwrap_or_default();
    (b.genres.iter())
        .filter(|other| {
            !tree
                || if parents.is_empty() {
                    other.parents.is_empty()
                } else {
                    other.parents.iter().any(|parent| parents.contains(parent))
                }
        })
        .map(|other| other.value.as_str())
        .chain([genre])
        .collect()
}

/// `genres`, those most albums carry first, then by name.
fn by_albums<'a>(genres: impl Iterator<Item = &'a Genre>) -> Vec<Genre> {
    let mut genres: Vec<Genre> = genres.cloned().collect();
    genres.sort_by_cached_key(|genre| {
        (
            std::cmp::Reverse(genre.album_count),
            genre.value.to_lowercase(),
        )
    });
    genres
}

/// The genres page: every genre, or on servers with Fonoteca's genres those
/// under none, those most albums carry first.
fn show_genres(ui: &AppWindow, browser: &Shared) {
    let size = genre_pixels(ui);
    let wanted = {
        let mut b = browser.borrow_mut();
        let tree = offers(&b, GENRE_TREE);
        let shown = by_albums((b.genres.iter()).filter(|genre| !tree || genre.parents.is_empty()));
        let tiles = genre_tiles(&b, &shown);
        let (tiles, wanted) = seen_covers(&b, tiles, size, true);
        b.genre_tiles = tiles;
        ui.set_genre_tiles(ModelRc::from(b.genre_tiles.clone()));
        wanted
    };
    side_covers(browser, |b| &b.genre_tiles, wanted, size, true);
}

/// A genre's page beside its albums: the genres it sits under, how many
/// albums and artists carry it, its artists known for it best first, the
/// banner of the best known with one, and on servers with Fonoteca's genres
/// its description and the genres under it, those most albums carry first.
fn show_genre(ui: &AppWindow, browser: &Shared) {
    let (size, genre_size) = (tile_pixels(ui), genre_pixels(ui));
    let (wanted_subgenres, wanted_artists, banner) = {
        let mut b = browser.borrow_mut();
        let genre = shown_genre(&b).to_owned();
        let tree = offers(&b, GENRE_TREE);
        let subgenres =
            by_albums((b.genres.iter()).filter(|sub| tree && sub.parents.contains(&genre)));
        let subgenres = genre_tiles(&b, &subgenres);
        let albums = (b.library.iter())
            .filter(|album| carries(album, &genre))
            .count();
        let known = known_for(&b.library, &b.artists, &genre, &siblings(&b, &genre));
        // Its own best-known artist's, even if a tile shows it too.
        let banners = artist_banners(&b.artists, offers(&b, ARTIST_BANNER));
        let banner = banner_of(&known, &banners, &HashSet::new());
        let described = offers_version(&b, GENRE_TREE, GENRE_DESCRIPTIONS);
        let description = (b.genres.iter().find(|shown| shown.value == genre))
            .and_then(|genre| description_of(genre, described));
        show_description(ui, description);
        let facts = if b.library_loaded {
            let albums = count(albums, "album", "albums");
            format!("{albums} · {}", count(known.len(), "artist", "artists"))
        } else {
            String::new()
        };
        ui.set_header_facts(facts.into());
        let pictures = artist_pictures(&b);
        let artists: Vec<_> = (known.into_iter().take(SHELF))
            .map(|(name, albums)| {
                let id = name.link.strip_prefix("artist:").unwrap_or_default();
                let picture = pictures.get(id).map(|picture| (*picture).clone());
                let tile = AlbumTile {
                    title: name.label,
                    names: names(vec![unlinked(count(albums, "album", "albums"))]),
                    link: name.link,
                    ..AlbumTile::default()
                };
                (tile, picture)
            })
            .collect();
        let parents: Vec<Name> = (b.genres.iter().find(|shown| shown.value == genre))
            .map(|shown| {
                (shown.parents.iter())
                    .map(|parent| Name {
                        label: parent.as_str().into(),
                        link: format!("genre:{parent}").into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        ui.set_header_names(names(parents));
        let (subgenres, wanted_subgenres) = seen_covers(&b, subgenres, genre_size, true);
        let (artists, wanted_artists) = seen_covers(&b, artists, size, false);
        b.genre_tiles = subgenres;
        b.genre_artists = artists;
        ui.set_genre_tiles(ModelRc::from(b.genre_tiles.clone()));
        ui.set_genre_artists(ModelRc::from(b.genre_artists.clone()));
        (wanted_subgenres, wanted_artists, banner)
    };
    genre_banner(ui, browser, banner);
    side_covers(
        browser,
        |b| &b.genre_tiles,
        wanted_subgenres,
        genre_size,
        true,
    );
    side_covers(browser, |b| &b.genre_artists, wanted_artists, size, false);
}

/// What the server says a genre is, and where the text comes from.
#[derive(Debug, PartialEq)]
struct Description {
    text: String,
    source: String,
    /// An address to open, only ever a web page.
    url: String,
}

/// A genre's description, unless the server (`offered`) gave none. The
/// source's address is kept only if it is a web page, since the app opens it.
fn description_of(genre: &Genre, offered: bool) -> Option<Description> {
    if !offered {
        return None;
    }
    let text = genre.description.as_deref().map(str::trim)?;
    if text.is_empty() {
        return None;
    }
    let url = (genre.description_url.as_deref())
        .map(str::trim)
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"));
    Some(Description {
        text: text.to_owned(),
        source: genre.description_source.clone().unwrap_or_default(),
        url: url.unwrap_or_default().to_owned(),
    })
}

fn show_description(ui: &AppWindow, description: Option<Description>) {
    let description = description.unwrap_or(Description {
        text: String::new(),
        source: String::new(),
        url: String::new(),
    });
    show_about(ui, Some(&description.text));
    ui.set_about_source(description.source.into());
    ui.set_about_url(description.url.into());
}

/// Puts the genre page's banner in the genres' tint over its title once it
/// comes; the page's text is there before it.
fn genre_banner(ui: &AppWindow, browser: &Shared, banner: Option<String>) {
    let Some(banner) = banner else { return };
    let size = pixels(
        ui,
        (ui.window()
            .size()
            .to_logical(ui.window().scale_factor())
            .width)
            .clamp(100.0, BANNER_SIZE),
    );
    let visit = browser.borrow().visit;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        if let Some(Some(image)) = tinted_art(&browser, visit, banner, size).await
            && let Some(ui) = weak.upgrade()
        {
            ui.set_genre_banner(image);
        }
    })
    .expect("the event loop is running");
}

/// The genre page's popular stations: those of the radio directory tagged
/// with the genre, in the languages picked first, as a radio page's shelf.
/// None, or a directory that doesn't answer, leaves the shelf out: the page
/// is whole without it.
fn genre_radio(ui: &AppWindow, browser: &Shared, genre: &str) {
    let Some(directory) = directory(browser) else {
        return;
    };
    let tag = directory_tag(genre);
    let (languages, visit) = {
        let b = browser.borrow();
        (b.languages.clone(), b.visit)
    };
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        learn(&browser, &directory).await;
        let found = shelf_stations(&directory, &tag, &languages).await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().visit != visit {
            return;
        }
        if let Ok(stations) = found {
            show_genre_radio(&ui, &browser, &tag, stations);
        }
    })
    .expect("the event loop is running");
}

fn show_genre_radio(ui: &AppWindow, browser: &Shared, tag: &str, stations: Vec<Station>) {
    let mut b = browser.borrow_mut();
    let tiles: Vec<AlbumTile> = stations.iter().map(|s| station_tile(&b, s)).collect();
    b.genre_radio = Rc::new(VecModel::from(tiles));
    b.logos_asked.retain(|(asked, _)| *asked != GENRE_RADIO);
    let empty = stations.is_empty();
    b.shelves.insert(GENRE_RADIO, stations);
    if empty {
        return;
    }
    ui.set_genre_radio(StationShelf {
        title: "Popular radio stations".into(),
        link: format!("stations:{tag}").into(),
        shelf: GENRE_RADIO as i32,
        tiles: ModelRc::from(b.genre_radio.clone()),
    });
}

/// The artists of the albums carrying `genre` that have a page, with how many
/// of those albums each has, those known for it best first: by how many of
/// their albums have it as their main genre among `siblings`, then by how
/// many carry it at all, each weighed by how much of their own albums that
/// is; then by name.
fn known_for(
    library: &[AlbumID3],
    artists: &[ArtistID3],
    genre: &str,
    siblings: &HashSet<&str>,
) -> Vec<(Name, usize)> {
    // Each artist's name, albums carrying the genre and albums it is the
    // main genre of.
    let mut counted: Vec<(Name, usize, usize)> = Vec::new();
    let mut at: HashMap<SharedString, usize> = HashMap::new();
    for album in library.iter().filter(|album| carries(album, genre)) {
        let main = usize::from(main_genre(album, siblings) == Some(genre));
        for name in album_names(album) {
            if name.link.is_empty() {
                continue;
            }
            match at.get(&name.link) {
                Some(&i) => {
                    counted[i].1 += 1;
                    counted[i].2 += main;
                }
                None => {
                    at.insert(name.link.clone(), counted.len());
                    counted.push((name, 1, main));
                }
            }
        }
    }
    let own: HashMap<&str, &ArtistID3> = (artists.iter())
        .map(|artist| (artist.id.as_str(), artist))
        .collect();
    let mut ranked: Vec<(f64, f64, String, Name, usize)> = counted
        .into_iter()
        .map(|(mut name, carrying, main)| {
            let artist = (name.link.strip_prefix("artist:")).and_then(|id| own.get(id).copied());
            // Its own name, not the credit of its first album.
            if let Some(artist) = artist {
                name.label = artist.name.as_str().into();
            }
            let albums = artist
                .and_then(|artist| artist.album_count)
                .unwrap_or_default();
            let all = usize::try_from(albums).unwrap_or_default().max(carrying) as f64;
            let weighed = |albums: usize| (albums * albums) as f64 / all;
            let label = name.label.to_lowercase();
            (weighed(main), weighed(carrying), label, name, carrying)
        })
        .collect();
    ranked.sort_by(|a, b| {
        (b.0.total_cmp(&a.0))
            .then(b.1.total_cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    (ranked.into_iter())
        .map(|(.., name, carrying)| (name, carrying))
        .collect()
}

/// A genre's albums as indexes into the library, by its artists known for it
/// best, an album of each in turn: those it is the main genre of first, then
/// by name.
fn genre_albums(b: &Browser, genre: &str) -> Vec<usize> {
    let siblings = siblings(b, genre);
    let known = known_for(&b.library, &b.artists, genre, &siblings);
    let rank: HashMap<SharedString, usize> = (known.into_iter().enumerate())
        .map(|(rank, (name, _))| (name.link, rank))
        .collect();
    let mut view: Vec<(bool, usize, String, usize)> = (0..b.library.len())
        .filter(|&i| carries(&b.library[i], genre))
        .map(|i| {
            let album = &b.library[i];
            let best = (album_names(album).iter())
                .filter_map(|name| rank.get(&name.link).copied())
                .min()
                .unwrap_or(usize::MAX);
            let main = main_genre(album, &siblings) == Some(genre);
            (!main, best, album.name.to_lowercase(), i)
        })
        .collect();
    view.sort();
    // Then the first of each artist's, the second, and so on.
    let mut taken: HashMap<usize, usize> = HashMap::new();
    let mut turns: Vec<(usize, usize, usize)> = (view.into_iter())
        .enumerate()
        .map(|(at, (_, best, _, i))| {
            let turn = taken.entry(best).or_default();
            *turn += 1;
            (*turn, at, i)
        })
        .collect();
    turns.sort_by_key(|&(turn, at, _)| (turn, at));
    turns.into_iter().map(|(.., i)| i).collect()
}

/// The pictures of the artists by id, those that have one.
fn artist_pictures(b: &Browser) -> HashMap<&str, &String> {
    (b.artists.iter())
        .filter_map(|artist| {
            let picture = artist.cover_art.as_ref()?;
            (!picture.is_empty()).then_some((artist.id.as_str(), picture))
        })
        .collect()
}

/// Each genre's tile, opening its page, with how many albums carry it and,
/// once the library and its artists are in, the banner of the artist known
/// for it best among `genres` that has one no tile before it shows.
fn genre_tiles(b: &Browser, genres: &[Genre]) -> Vec<(AlbumTile, Option<String>)> {
    let siblings: HashSet<&str> = genres.iter().map(|genre| genre.value.as_str()).collect();
    let banners = artist_banners(&b.artists, offers(b, ARTIST_BANNER));
    let banners = if b.library_loaded && b.artists_loaded {
        let known =
            (genres.iter()).map(|genre| known_for(&b.library, &b.artists, &genre.value, &siblings));
        distinct_banners(known, &banners)
    } else {
        vec![None; genres.len()]
    };
    (genres.iter().zip(banners))
        .map(|(genre, banner)| {
            let albums = usize::try_from(genre.album_count).unwrap_or_default();
            let tile = AlbumTile {
                title: genre.value.as_str().into(),
                names: names(vec![unlinked(count(albums, "album", "albums"))]),
                link: format!("genre:{}", genre.value).into(),
                ..AlbumTile::default()
            };
            (tile, banner)
        })
        .collect()
}

/// The banners of the artists by id, those that have one, on servers with
/// Fonoteca's (`offered`).
fn artist_banners(artists: &[ArtistID3], offered: bool) -> HashMap<&str, &String> {
    if !offered {
        return HashMap::new();
    }
    (artists.iter())
        .filter_map(|artist| {
            let banner = artist.banner_art.as_ref()?;
            (!banner.is_empty()).then_some((artist.id.as_str(), banner))
        })
        .collect()
}

/// The banner of the artist known for a genre best that has one not in
/// `taken`: the next best-known artist's where the best-known has none.
fn banner_of(
    known: &[(Name, usize)],
    banners: &HashMap<&str, &String>,
    taken: &HashSet<String>,
) -> Option<String> {
    known.iter().find_map(|(name, _)| {
        let id = name.link.strip_prefix("artist:")?;
        banners
            .get(id)
            .filter(|banner| !taken.contains(**banner))
            .map(|banner| (*banner).clone())
    })
}

/// Each genre's banner, those first in line choosing first: none shows a
/// banner an earlier genre has. `known` is each genre's artists, best known
/// first.
fn distinct_banners(
    known: impl Iterator<Item = Vec<(Name, usize)>>,
    banners: &HashMap<&str, &String>,
) -> Vec<Option<String>> {
    let mut taken = HashSet::new();
    known
        .map(|known| {
            let banner = banner_of(&known, banners, &taken);
            taken.extend(banner.clone());
            banner
        })
        .collect()
}

/// A picture in the genres' shared tint: its light alone, from a dark to a
/// pale violet grey, so their names read on any picture. Decoded here, as the
/// browser build leaves decoding to the browser, out of reach of its pixels.
fn tinted(bytes: &[u8]) -> Option<Image> {
    let mut pixels = image::load_from_memory(bytes).ok()?.into_rgba8();
    for pixel in pixels.pixels_mut() {
        let [r, g, b, _] = &mut pixel.0;
        let light = (0.299 * f32::from(*r) + 0.587 * f32::from(*g) + 0.114 * f32::from(*b)) / 255.0;
        let tone = |dark: f32, pale: f32| (dark + (pale - dark) * light) as u8;
        (*r, *g, *b) = (tone(28.0, 204.0), tone(28.0, 204.0), tone(44.0, 228.0));
    }
    let (width, height) = pixels.dimensions();
    Some(Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
        pixels.as_raw(),
        width,
        height,
    )))
}

/// A picture in the genres' shared tint, kept for the session.
async fn tinted_art(
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
        if let Some(image) = b.tinted.get(&key) {
            return Some(Some(image.clone()));
        }
        b.client.clone()?
    };
    let bytes = client.get_cover_art(&key.0, Some(size)).await;
    let image = bytes.ok().and_then(|bytes| tinted(&bytes));
    let mut b = browser.borrow_mut();
    let signed_in = (b.client.as_ref()).is_some_and(|current| Arc::ptr_eq(current, &client));
    if let (Some(image), true) = (&image, signed_in) {
        b.tinted.insert(key, image.clone());
    }
    (b.visit == visit).then_some(image)
}

/// A model of `tiles`, each with its cover where it has been seen this
/// session, in the genres' tint where `tint` says, and the covers still to
/// fetch by row.
fn seen_covers(
    b: &Browser,
    tiles: Vec<(AlbumTile, Option<String>)>,
    size: u32,
    tint: bool,
) -> (Rc<VecModel<AlbumTile>>, Vec<(usize, String)>) {
    let seen = if tint { &b.tinted } else { &b.pictures };
    let model = Rc::new(VecModel::default());
    let mut wanted = Vec::new();
    for (row, (mut tile, cover)) in tiles.into_iter().enumerate() {
        if let Some(cover) = cover {
            match seen.get(&(cover.clone(), size)) {
                Some(image) => tile.cover = image.clone(),
                None => wanted.push((row, cover)),
            }
        }
        model.push(tile);
    }
    (model, wanted)
}

/// Fetches the covers `wanted` of a model beside the page's tiles, in order,
/// in the genres' tint where `tint` says, while `model` still names it.
fn side_covers(
    browser: &Shared,
    model: fn(&Browser) -> &Rc<VecModel<AlbumTile>>,
    wanted: Vec<(usize, String)>,
    size: u32,
    tint: bool,
) {
    let tiles = model(&browser.borrow()).clone();
    let (browser, visit) = (browser.clone(), browser.borrow().visit);
    slint::spawn_local(async move {
        for (row, cover) in wanted {
            if !Rc::ptr_eq(model(&browser.borrow()), &tiles) {
                return;
            }
            let image = if tint {
                tinted_art(&browser, visit, cover, size).await
            } else {
                cover_art(&browser, Some(visit), cover, size).await
            };
            let Some(image) = image else { return };
            if let (Some(image), Some(mut tile)) = (image, tiles.row_data(row)) {
                tile.cover = image;
                tiles.set_row_data(row, tile);
            }
        }
    })
    .expect("the event loop is running");
}

/// Adds the tiles, each with its cover when it has been seen this session,
/// and fetches the others.
fn add_tiles(ui: &AppWindow, browser: &Shared, tiles: Vec<(AlbumTile, Option<String>)>) {
    let size = tile_pixels(ui);
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
            if let Some(image) = seen {
                show_picture(&mut tile, image);
            }
            tile.selected = b.selected.iter().any(|link| *link == tile.link.as_str());
            tile.downloaded = b.downloads.is_kept(&tile.link);
            b.tiles.push(tile);
        }
    }
    fetch_covers(ui, browser);
}

/// Fetches the covers of the tiles or tracks table rows in view, top to
/// bottom, a few at a time. The others wait until they are scrolled to.
fn fetch_covers(ui: &AppWindow, browser: &Shared) {
    let size = if matches!(
        ui.get_page(),
        Page::Tracks | Page::Compositions | Page::Composition | Page::Folders | Page::Tag
    ) {
        pixels(ui, LINE_COVER_SIZE)
    } else {
        tile_pixels(ui)
    };
    let mut b = browser.borrow_mut();
    let wanted = b
        .covers
        .range(b.in_view.clone())
        .count()
        .min(COVERS_AT_ONCE);
    while b.fetching < wanted {
        b.fetching += 1;
        let (tiles, lines, tracks) = (b.tiles.clone(), b.lines.clone(), b.tracks.clone());
        let (browser, visit) = (browser.clone(), b.visit);
        slint::spawn_local(async move {
            loop {
                let (row, cover) = {
                    let mut b = browser.borrow_mut();
                    let same = Rc::ptr_eq(&b.tiles, &tiles)
                        && Rc::ptr_eq(&b.lines, &lines)
                        && Rc::ptr_eq(&b.tracks, &tracks);
                    if b.visit != visit || !same {
                        return;
                    }
                    let next = b.covers.range(b.in_view.clone()).next();
                    let Some(row) = next.map(|(row, _)| *row) else {
                        b.fetching -= 1;
                        return;
                    };
                    (row, b.covers.remove(&row).expect("the row was just found"))
                };
                let Some(image) = cover_art(&browser, Some(visit), cover, size).await else {
                    return;
                };
                let Some(image) = image else { continue };
                if let Some(mut tile) = tiles.row_data(row) {
                    show_picture(&mut tile, image);
                    tiles.set_row_data(row, tile);
                } else if let Some(mut line) = lines.row_data(row) {
                    line.cover = image;
                    lines.set_row_data(row, line);
                } else if let Some(mut track) = tracks.row_data(row) {
                    track.cover = image;
                    tracks.set_row_data(row, track);
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
        if let Some(Some(image)) = cover_art(&browser, Some(visit), cover, size).await
            && let Some(ui) = weak.upgrade()
        {
            ui.set_header_image(image);
        }
    })
    .expect("the event loop is running");
}

/// Plays the album's motion cover in the header in place of its cover.
fn header_motion(ui: &AppWindow, browser: &Shared, album: String) {
    let visit = browser.borrow().visit;
    let on_page = browser.clone();
    motion_cover(
        ui,
        browser,
        album,
        HEADER_SIZE,
        |b| &b.motion,
        move || on_page.borrow().visit == visit,
        || true,
        |ui, image| ui.set_header_image(image),
    );
}

/// Starts the motion cover of the album that plays, `logical` pixels wide, to
/// go in place of the cover `show` puts on the now playing sheet, until
/// another album plays. The same album again, as the next track, goes on
/// with it.
pub fn playing_motion(
    ui: &AppWindow,
    browser: &Shared,
    album: Option<String>,
    logical: f32,
    show: impl Fn(&AppWindow, Image) + 'static,
) {
    {
        let mut b = browser.borrow_mut();
        if b.playing.album == album {
            return;
        }
        // A timer of its own, so the last album's frames can't come back
        // when this one's are started.
        b.playing = PlayingMotion {
            album: album.clone(),
            moving: b.playing.moving,
            ..PlayingMotion::default()
        };
    }
    let Some(album) = album else { return };
    let (still_playing, moving, framed) = (browser.clone(), browser.clone(), browser.clone());
    let wanted = album.clone();
    motion_cover(
        ui,
        browser,
        album,
        logical,
        |b| &b.playing.timer,
        move || still_playing.borrow().playing.album.as_ref() == Some(&wanted),
        move || moving.borrow().playing.moving,
        move |ui, image| {
            framed.borrow_mut().playing.framed = true;
            show(ui, image);
        },
    );
}

/// Whether the motion cover of the album that plays has shown a frame, which
/// stays up while it is held and until another album plays.
pub fn playing_motion_framed(browser: &Shared) -> bool {
    browser.borrow().playing.framed
}

/// Runs the motion cover of the album that plays, or holds it on the frame it
/// is at: it moves only while the sheet is open and the song plays.
pub fn move_playing_motion(browser: &Shared, moving: bool) {
    let mut b = browser.borrow_mut();
    b.playing.moving = moving;
    if moving {
        b.playing.timer.restart();
    } else {
        b.playing.timer.stop();
    }
}

/// Whether the server has Fonoteca's artist banners.
pub fn offers_artist_banners(browser: &Shared) -> bool {
    offers(&browser.borrow(), ARTIST_BANNER)
}

/// Hands `done` the banner of the artist `id` that plays, as wide as the
/// window and no wider than a genre page's, or no picture for an artist
/// without one, so the now playing page shows the album's cover in its
/// place.
pub fn playing_banner(
    ui: &AppWindow,
    browser: &Shared,
    id: String,
    done: impl FnOnce(&AppWindow, Image) + 'static,
) {
    let Some(client) = client(browser) else {
        return;
    };
    let window = ui.window();
    let width = window.size().to_logical(window.scale_factor()).width;
    let size = pixels(ui, width.clamp(100.0, BANNER_SIZE));
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let listed = browser
            .borrow()
            .artists
            .iter()
            .find(|artist| artist.id == id)
            .map(|artist| artist.banner_art.clone());
        let banner = match listed {
            Some(banner) => banner,
            None => match client.get_artist(&id).await {
                Ok(artist) => artist.artist.banner_art,
                Err(problem) => {
                    eprintln!("The banner of {id}: {problem}");
                    None
                }
            },
        };
        let image = match banner.filter(|banner| !banner.is_empty()) {
            Some(banner) => cover_art(&browser, None, banner, size).await.flatten(),
            None => None,
        };
        if let Some(ui) = weak.upgrade() {
            done(&ui, image.unwrap_or_default());
        }
    })
    .expect("the event loop is running");
}

/// Drops the motion cover of the album that plays, as when nothing plays.
pub fn stop_playing_motion(browser: &Shared) {
    let mut b = browser.borrow_mut();
    b.playing = PlayingMotion {
        moving: b.playing.moving,
        ..PlayingMotion::default()
    };
}

/// The album whose motion cover is kept, for the tests.
#[cfg(test)]
pub fn playing_motion_album(browser: &Shared) -> Option<String> {
    browser.borrow().playing.album.clone()
}

/// Plays the album's motion cover through `show`, on servers that have
/// them, for as long as `wanted` holds, one frame at a time off the timer
/// `timer` picks, which runs only while `moving` says so. Frames are decoded
/// as they are shown, so only the file stays in memory.
#[expect(
    clippy::too_many_arguments,
    reason = "the header's and the player's motion differ in each"
)]
fn motion_cover(
    ui: &AppWindow,
    browser: &Shared,
    album: String,
    logical: f32,
    timer: fn(&Browser) -> &Timer,
    wanted: impl Fn() -> bool + 'static,
    moving: impl Fn() -> bool + 'static,
    show: impl Fn(&AppWindow, Image) + 'static,
) {
    let client = browser.borrow().client.clone();
    let Some(client) = client.filter(|client| client.supports(MOTION)) else {
        return;
    };
    let size = pixels(ui, logical);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let Ok(bytes) = client.get_album_motion(&album, Some(size)).await else {
            return;
        };
        let bytes: Rc<[u8]> = bytes.into();
        let (Some(mut frames), true) = (motion_frames(&bytes), wanted()) else {
            return;
        };
        let Some(Ok(first)) = frames.next() else {
            return;
        };
        let interval = Duration::from(first.delay()).max(Duration::from_millis(10));
        if let Some(ui) = weak.upgrade() {
            show(&ui, picture(first));
        }
        let kept = browser.borrow();
        let timer = timer(&kept);
        timer.start(TimerMode::Repeated, interval, move || {
            let next = frames.next().or_else(|| {
                frames = motion_frames(&bytes)?;
                frames.next()
            });
            if let (Some(ui), Some(Ok(frame))) = (weak.upgrade(), next) {
                show(&ui, picture(frame));
            }
        });
        if !moving() {
            timer.stop();
        }
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
/// page of `visit` has been left, so there is no point fetching more; with
/// no visit the cover is wanted whichever page is shown. Covers are kept
/// for the session, so a page shown again has them at once.
async fn cover_art(
    browser: &Shared,
    visit: Option<i32>,
    cover: String,
    size: u32,
) -> Option<Option<Image>> {
    if cover.contains("://") {
        let image = logo(browser, cover).await;
        return visit
            .is_none_or(|visit| browser.borrow().visit == visit)
            .then_some(image);
    }
    let key = (cover, size);
    let (client, kept, offline) = {
        let b = browser.borrow();
        if visit.is_some_and(|visit| b.visit != visit) {
            return None;
        }
        if let Some(image) = b.pictures.get(&key) {
            return Some(Some(image.clone()));
        }
        (b.client.clone()?, b.downloads.clone(), b.offline)
    };
    // The cover kept on this device answers while the server can't.
    let saved = offline.then(|| kept.cover(&key.0)).flatten();
    let bytes = match saved {
        Some(bytes) => Some(bytes),
        None => match client.get_cover_art(&key.0, Some(size)).await {
            Ok(bytes) => Some(bytes),
            Err(_) => kept.cover(&key.0),
        },
    };
    let image = bytes.and_then(|bytes| Image::load_from_data(&bytes, None).ok());
    let mut b = browser.borrow_mut();
    let signed_in = b
        .client
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, &client));
    if let (Some(image), true) = (&image, signed_in) {
        b.pictures.insert(key, image.clone());
    }
    visit.is_none_or(|visit| b.visit == visit).then_some(image)
}

/// Loves or unloves an album's or playlist's track.
fn toggle_love(ui: &AppWindow, browser: &Shared, row: usize) {
    let mut b = browser.borrow_mut();
    let (Some(id), Some(mut track)) = (
        b.songs.get(row).map(|song| song.id.clone()),
        b.tracks.row_data(row),
    ) else {
        return;
    };
    let loved = !track.loved;
    track.loved = loved;
    b.tracks.set_row_data(row, track);
    mark_loved(&mut b, ui.get_page(), &id, loved);
    let tracks = b.tracks.clone();
    drop(b);
    let undo = {
        let id = id.clone();
        move |ui: &AppWindow, browser: &Shared| {
            if let Some(mut track) = tracks.row_data(row) {
                track.loved = !loved;
                tracks.set_row_data(row, track);
            }
            mark_loved(&mut browser.borrow_mut(), ui.get_page(), &id, !loved);
        }
    };
    love(ui, browser, id, loved, undo);
}

/// Loves or unloves the song, or folder, of a row of the tracks or folders
/// table.
fn toggle_line_love(ui: &AppWindow, browser: &Shared, row: usize) {
    let page = ui.get_page();
    if page == Page::Compositions {
        toggle_piece_love(ui, browser, row);
        return;
    }
    let mut b = browser.borrow_mut();
    let (songs, order) = listed(&b, page);
    let Some(song) = order.get(row).and_then(|&song| songs.get(song)) else {
        return;
    };
    let id = song.id.clone();
    let loved = song.starred.is_none();
    mark_loved(&mut b, page, &id, loved);
    drop(b);
    let undo = {
        let id = id.clone();
        move |ui: &AppWindow, browser: &Shared| {
            mark_loved(&mut browser.borrow_mut(), ui.get_page(), &id, !loved);
        }
    };
    love(ui, browser, id, loved, undo);
}

/// Loves the composition of a row of the compositions table on the server,
/// or stops loving it.
fn toggle_piece_love(ui: &AppWindow, browser: &Shared, row: usize) {
    let mut b = browser.borrow_mut();
    let Some(&piece) = b.view.get(row) else {
        return;
    };
    let id = b.compositions[piece].id.clone();
    let loved = b.compositions[piece].starred.is_none();
    mark_piece_loved(&mut b, Page::Compositions, &id, loved);
    drop(b);
    let undo = {
        let id = id.clone();
        move |ui: &AppWindow, browser: &Shared| {
            mark_piece_loved(&mut browser.borrow_mut(), ui.get_page(), &id, !loved);
        }
    };
    love(ui, browser, id, loved, undo);
}

/// Keeps whether a composition is loved with the compositions, and on its
/// row when `page`, the one on screen, is the compositions table.
fn mark_piece_loved(b: &mut Browser, page: Page, id: &str, loved: bool) {
    let Some(piece) = b.compositions.iter().position(|piece| piece.id == id) else {
        return;
    };
    b.compositions[piece].starred = loved.then(String::new);
    if page != Page::Compositions {
        return;
    }
    let row = b.view.iter().position(|&shown| shown == piece);
    if let Some(row) = row
        && let Some(mut line) = b.lines.row_data(row)
    {
        line.loved = loved;
        b.lines.set_row_data(row, line);
    }
}

/// Keeps whether a song or folder is loved with the tracks page's songs and
/// the folder's entries, wherever it was loved, and on its row when `page`,
/// the one on screen, is the tracks or folders table.
fn mark_loved(b: &mut Browser, page: Page, id: &str, loved: bool) {
    for song in b.song_list.iter_mut().chain(&mut b.entries) {
        if song.id == id {
            song.starred = loved.then(String::new);
        }
    }
    if !matches!(page, Page::Tracks | Page::Folders | Page::Tag) {
        return;
    }
    let (songs, order) = listed(b, page);
    let row = order.iter().position(|&shown| songs[shown].id == id);
    if let Some(row) = row
        && let Some(mut line) = b.lines.row_data(row)
    {
        line.loved = loved;
        b.lines.set_row_data(row, line);
    }
}

/// Stars or unstars a song or composition on the server. `undo` puts the
/// heart back when the server doesn't take it, on the page then on screen.
fn love(
    ui: &AppWindow,
    browser: &Shared,
    id: String,
    loved: bool,
    undo: impl FnOnce(&AppWindow, &Shared) + 'static,
) {
    let b = browser.borrow();
    let Some(client) = b.client.clone() else {
        return;
    };
    let visit = b.visit;
    drop(b);

    // The song may be the one that plays, whose sheet shows the heart too.
    ui.invoke_song_loved(id.as_str().into(), loved);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let starred = if loved {
            client.star(&[&id], &[], &[]).await
        } else {
            client.unstar(&[&id], &[], &[]).await
        };
        let Some(ui) = weak.upgrade() else { return };
        match starred {
            Ok(()) => crate::loves::sync(&ui, &browser),
            Err(e) => {
                undo(&ui, &browser);
                ui.invoke_song_loved(id.as_str().into(), !loved);
                if browser.borrow().visit == visit {
                    ui.set_page_problem(format!("Your server didn't take that: {e}").into());
                }
            }
        }
    })
    .expect("the event loop is running");
}

/// Shows the songs a sync with ListenBrainz starred and unstarred as
/// loved and not, on the page on screen and the sheet of the song playing,
/// without drawing either anew. Nothing is shown for a server the app has
/// since left.
pub(crate) fn show_synced_hearts(
    ui: &AppWindow,
    browser: &Shared,
    client: &Arc<Client>,
    starred: &[String],
    unstarred: &[String],
) {
    let is_current = (browser.borrow().client.as_ref()).is_some_and(|now| Arc::ptr_eq(now, client));
    if !is_current {
        return;
    }
    let changes = starred
        .iter()
        .map(|id| (id, true))
        .chain(unstarred.iter().map(|id| (id, false)));
    for (id, loved) in changes {
        let mut b = browser.borrow_mut();
        mark_loved(&mut b, ui.get_page(), id, loved);
        for row in (0..b.songs.len()).filter(|&row| b.songs[row].id == *id) {
            if let Some(mut track) = b.tracks.row_data(row) {
                track.loved = loved;
                b.tracks.set_row_data(row, track);
            }
        }
        drop(b);
        ui.invoke_song_loved(id.as_str().into(), loved);
    }
}

pub(crate) fn loves_state(browser: &Shared) -> std::cell::RefMut<'_, crate::loves::State> {
    std::cell::RefMut::map(browser.borrow_mut(), |b| &mut b.loves)
}

pub(crate) fn found(browser: &Shared) -> matching::Found {
    browser.borrow().found.clone()
}

/// Loves or unloves a song that plays, which no table of the page holds a
/// row of.
pub fn love_playing(ui: &AppWindow, browser: &Shared, id: String, loved: bool) {
    mark_loved(&mut browser.borrow_mut(), ui.get_page(), &id, loved);
    let song = id.clone();
    love(ui, browser, id, loved, move |ui, browser| {
        mark_loved(&mut browser.borrow_mut(), ui.get_page(), &song, !loved);
    });
}

/// `logical` pixels in physical ones, which is what the server should scale to.
fn pixels(ui: &AppWindow, logical: f32) -> u32 {
    (logical * ui.window().scale_factor()).ceil() as u32
}

/// The size genre pictures are asked for, in physical pixels: a tile's widest,
/// under half the window wide.
fn genre_pixels(ui: &AppWindow) -> u32 {
    let window = ui.window();
    let width = window.size().to_logical(window.scale_factor()).width;
    pixels(ui, (width / 2.0).clamp(100.0, GENRE_SIZE))
}

/// The size grid covers are asked for, in physical pixels.
fn tile_pixels(ui: &AppWindow) -> u32 {
    let window = ui.window();
    let width = window.size().to_logical(window.scale_factor()).width;
    pixels(ui, (width / 2.0).clamp(100.0, TILE_SIZE))
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

pub fn song_names(song: &Child) -> Vec<Name> {
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

/// Whether two neighbouring tracks belong to one work, by its name: an album
/// can link one piece's tracks to two editions of it, as Swan Lake's No. 13
/// to Drigo's.
pub(crate) fn same_work(one: &Child, next: &Child) -> bool {
    let (one, next) = (one.works.first(), next.works.first());
    one.is_some() && one.map(|work| &work.name) == next.map(|work| &work.name)
}

/// The runs a page's songs fall in: each complete work, with all its
/// movements next to each other on one album, is one run; every other song
/// stands alone, also where it is some movements of a work.
pub(crate) fn whole_works(songs: &[Child]) -> Vec<&[Child]> {
    let mut runs = Vec::new();
    for run in songs.chunk_by(|one, next| same_work(one, next) && one.album_id == next.album_id) {
        if is_complete_work(run) {
            runs.push(run);
        } else {
            runs.extend(run.chunks(1));
        }
    }
    runs
}

/// Whether `run`, neighbouring songs of one work, holds the whole of it, by
/// the movements' number and count. Where numbers come with the count, every
/// number from 1 to the count must be there (a movement may take several
/// tracks, so more songs than the count is fine). Where the count comes
/// without numbers, the songs must be as many as the count. A server that
/// sends no count (Navidrome sends movement names only) gives no ground to
/// call the run partial, so it is whole, as the album page already shows it.
fn is_complete_work(run: &[Child]) -> bool {
    if run.len() < 2 {
        return false;
    }
    let movements: Vec<Option<&Movement>> = run.iter().map(|song| song.movements.first()).collect();
    let Some(count) = movements
        .iter()
        .flatten()
        .find_map(|movement| movement.count)
    else {
        return true;
    };
    let numbers: Option<HashSet<i64>> = movements
        .iter()
        .map(|movement| movement.and_then(|movement| movement.number))
        .collect();
    match numbers {
        Some(numbers) => (1..=count).all(|number| numbers.contains(&number)),
        None => run.len() as i64 >= count,
    }
}

/// The title of a run's `i`th track within its work: the movement's name
/// where the server sends one that no neighbour shares, else the track's own
/// title without the work's name in front, or from the movement's name on,
/// so the tracks of one opera scene or one long movement keep their own
/// names.
fn track_title(run: &[Child], i: usize) -> String {
    let movement = |j: usize| {
        run.get(j)
            .and_then(|song| song.movements.first())
            .map(|movement| &movement.name)
    };
    let shared = [i.checked_sub(1), Some(i + 1)]
        .into_iter()
        .flatten()
        .any(|j| movement(j) == movement(i));
    if let Some(name) = movement(i).filter(|_| !shared) {
        return name.clone();
    }
    // The longest "X: " in front where the work's name starts with X, as
    // "Das Rheingold: " is to "Das Rheingold, WWV 86A"; else from the
    // movement's name on, as "I. Allegro …, Pt. 1" in "Piano Concerto no. 1
    // …: I. Allegro …, Pt. 1" under "Concerto for Piano and Orchestra no. 1".
    let song = &run[i];
    song.works
        .first()
        .and_then(|work| {
            song.title.rmatch_indices(": ").find_map(|(at, _)| {
                let after = work.name.strip_prefix(&song.title[..at])?;
                (!after.starts_with(char::is_alphanumeric)).then(|| &song.title[at + 2..])
            })
        })
        .filter(|rest| !rest.is_empty())
        .or_else(|| {
            let name = movement(i).filter(|name| !name.is_empty())?;
            let at = song.title.find(&format!(": {name}"))?;
            Some(&song.title[at + 2..])
        })
        .unwrap_or(&song.title)
        .to_owned()
}

/// What now playing shows of a song that is part of a classical work: the
/// work as the title, and which movement plays.
#[derive(Debug, PartialEq)]
pub(crate) struct WorkLine {
    pub work: String,
    pub movement: String,
    /// "2 of 3", empty where the server sends no number or no count, as
    /// where Fonoteca could not tell which movement a recording is.
    pub place: String,
}

/// The work line of the `index`th song of a queue, which `song_at` reads, or
/// none where the song is no part of a work. Its neighbours of the same work
/// and album tell its movement's name apart from theirs, as on the album page.
pub(crate) fn work_line<'a>(
    song_at: impl Fn(usize) -> Option<&'a Child>,
    index: usize,
) -> Option<WorkLine> {
    let song = song_at(index)?;
    let work = song.works.first()?;
    let neighbours = |one: usize, next: usize| match (song_at(one), song_at(next)) {
        (Some(one), Some(next)) => same_work(one, next) && one.album_id == next.album_id,
        _ => false,
    };
    let mut start = index;
    while start > 0 && neighbours(start - 1, start) {
        start -= 1;
    }
    let mut end = index + 1;
    while neighbours(end - 1, end) {
        end += 1;
    }
    let run: Vec<Child> = (start..end).filter_map(&song_at).cloned().collect();
    let movement = track_title(&run, index - start);
    let place = match song.movements.first() {
        Some(Movement {
            number: Some(number),
            count: Some(count),
            ..
        }) => format!("{number} of {count}"),
        _ => String::new(),
    };
    Some(WorkLine {
        movement: if movement == work.name {
            String::new()
        } else {
            movement
        },
        work: work.name.clone(),
        place,
    })
}

/// "1804 – 1805", the years a song's work was written.
fn composed(song: &Child) -> String {
    years(song.works.first().and_then(|work| work.composed.as_ref()))
}

/// "1804 – 1805", or one year, as far as `span` is known.
fn years(span: Option<&Span>) -> String {
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
    [
        album.year.map(|y| y.to_string()).unwrap_or_default(),
        genres,
        length(album.song_count, album.duration),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

/// "9 tracks, 41 minutes".
fn length(tracks: i64, seconds: i64) -> String {
    // At least a minute, so a short single doesn't say 0.
    let minutes = match seconds {
        0 => 0,
        seconds => ((seconds + 30) / 60).max(1),
    };
    format!(
        "{}, {}",
        count(tracks as usize, "track", "tracks"),
        count(minutes as usize, "minute", "minutes"),
    )
}

/// "1 track", "2 tracks".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "4:05", or "1:02:05" from an hour on.
pub fn duration(seconds: i64) -> String {
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

    /// The lengths of the runs `whole_works` makes of songs, each given as
    /// its album, its work and its movement as the server sends it.
    fn runs(songs: &[(&str, &str, &str)]) -> Vec<usize> {
        let songs: Vec<Child> = songs
            .iter()
            .map(|&(album, work, movement)| {
                serde_json::from_str(&format!(
                    r#"{{"id":"s","isDir":false,"title":"t","albumId":"{album}","works":[{{"name":"{work}"}}],"movements":[{{"name":"m"{movement}}}]}}"#
                ))
                .unwrap()
            })
            .collect();
        whole_works(&songs).iter().map(|run| run.len()).collect()
    }

    /// One work's songs, numbered as given, of `count` movements.
    fn numbered(numbers: &[i64], count: i64) -> Vec<usize> {
        let movements: Vec<String> = numbers
            .iter()
            .map(|number| format!(r#","number":{number},"count":{count}"#))
            .collect();
        let songs: Vec<(&str, &str, &str)> = movements
            .iter()
            .map(|movement| ("a", "W", movement.as_str()))
            .collect();
        runs(&songs)
    }

    /// Songs of one work and album, each given as its title, and its
    /// movement's name and numbers as the server sends them.
    fn work_songs(songs: &[(&str, &str)]) -> Vec<Child> {
        songs
            .iter()
            .map(|&(title, movement)| {
                serde_json::from_str(&format!(
                    r#"{{"id":"s","isDir":false,"title":"{title}","albumId":"a","works":[{{"name":"Symphony no. 8"}}],"movements":[{{{movement}}}]}}"#
                ))
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn a_movement_of_a_work_plays_under_the_work() {
        let queue = work_songs(&[
            ("Symphony no. 8: I", r#""name":"I","number":1,"count":3"#),
            ("Symphony no. 8: II", r#""name":"II","number":2,"count":3"#),
        ]);
        assert_eq!(
            work_line(|i| queue.get(i), 1),
            Some(WorkLine {
                work: "Symphony no. 8".into(),
                movement: "II".into(),
                place: "2 of 3".into(),
            })
        );
    }

    #[test]
    fn a_movement_without_a_number_or_a_count_shows_no_place() {
        let queue = work_songs(&[
            ("a", r#""name":"I","count":3"#),
            ("b", r#""name":"II","number":2"#),
            ("c", r#""name":"III""#),
        ]);
        for index in 0..3 {
            assert_eq!(work_line(|i| queue.get(i), index).unwrap().place, "");
        }
    }

    #[test]
    fn a_song_without_a_work_has_no_work_line() {
        let song: Child = serde_json::from_str(r#"{"id":"s","isDir":false,"title":"t"}"#).unwrap();
        assert_eq!(work_line(|i| (i == 0).then_some(&song), 0), None);
    }

    #[test]
    fn a_track_that_is_the_whole_work_names_no_movement() {
        let queue = work_songs(&[("Symphony no. 8", r#""name":"Symphony no. 8""#)]);
        assert_eq!(work_line(|i| queue.get(i), 0).unwrap().movement, "");
    }

    /// A track of a symphony's first movement, on `album`.
    fn first_movement_track(album: &str, title: &str) -> Child {
        serde_json::from_str(&format!(
            r#"{{"id":"s","isDir":false,"title":"{title}","albumId":"{album}","works":[{{"name":"Symphony no. 8"}}],"movements":[{{"name":"I. Allegro"}}]}}"#
        ))
        .unwrap()
    }

    #[test]
    fn tracks_of_one_movement_keep_their_own_titles() {
        let queue = [
            first_movement_track("a", "Symphony no. 8: I. Allegro, Pt. 1"),
            first_movement_track("a", "Symphony no. 8: I. Allegro, Pt. 2"),
            first_movement_track("a", "Symphony no. 8: I. Allegro, Pt. 3"),
        ];
        let movements: Vec<String> = (0..queue.len())
            .map(|index| work_line(|i| queue.get(i), index).unwrap().movement)
            .collect();
        assert_eq!(
            movements,
            [
                "I. Allegro, Pt. 1",
                "I. Allegro, Pt. 2",
                "I. Allegro, Pt. 3"
            ]
        );
    }

    #[test]
    fn a_neighbour_on_another_album_does_not_share_the_movement() {
        let queue = [
            first_movement_track("b", "Symphony no. 8: I. Allegro, Pt. 1"),
            first_movement_track("a", "Symphony no. 8: I. Allegro, Pt. 2"),
        ];
        assert_eq!(
            work_line(|i| queue.get(i), 1).unwrap().movement,
            "I. Allegro"
        );
    }

    #[test]
    fn a_run_is_a_work_when_it_holds_all_its_movements() {
        assert_eq!(numbered(&[1, 2, 3, 4], 4), [4]);
        assert_eq!(numbered(&[3, 4], 4), [1, 1]);
        assert_eq!(numbered(&[1, 4], 4), [1, 1]);
        assert_eq!(numbered(&[1, 2, 3, 1], 4), [1; 4]);
        assert_eq!(numbered(&[1, 2, 3, 4, 5], 6), [1; 5]);
        // A movement on several tracks, as in Das Rheingold.
        assert_eq!(numbered(&[1, 2, 2, 2, 3, 4, 4], 4), [7]);
        assert_eq!(numbered(&[1], 1), [1]);
    }

    #[test]
    fn a_run_without_numbers_is_a_work_when_it_is_as_long_as_the_count() {
        let count = r#","count":4"#;
        assert_eq!(runs(&[("a", "W", count); 4]), [4]);
        assert_eq!(runs(&[("a", "W", count); 3]), [1; 3]);
    }

    #[test]
    fn a_run_without_a_count_is_a_work() {
        // Navidrome sends movement names only.
        assert_eq!(runs(&[("a", "W", ""); 3]), [3]);
    }

    #[test]
    fn songs_of_other_works_or_albums_are_not_one_run() {
        let (first, second) = (r#","number":1,"count":2"#, r#","number":2,"count":2"#);
        assert_eq!(
            runs(&[
                ("a", "W", first),
                ("a", "W", second),
                ("b", "W", first),
                ("b", "W", second),
                ("b", "V", first),
            ]),
            [2, 2, 1]
        );
    }

    #[test]
    fn an_artists_own_albums_are_theirs_alone_in_page_order() {
        let album = |json: serde_json::Value| -> AlbumID3 {
            let mut base = serde_json::json!({
                "id": "", "name": "", "songCount": 1, "duration": 1,
                "created": "2021-01-01T00:00:00Z"
            });
            base.as_object_mut()
                .unwrap()
                .extend(json.as_object().unwrap().clone());
            serde_json::from_value(base).unwrap()
        };
        let artist: ArtistID3 =
            serde_json::from_value(serde_json::json!({ "id": "r1", "name": "Mara" })).unwrap();
        let albums = [
            album(
                serde_json::json!({ "id": "own-a", "artists": [{ "id": "r1", "name": "Mara" }] }),
            ),
            album(
                serde_json::json!({ "id": "best-of", "isCompilation": true, "artists": [{ "id": "r1", "name": "Mara" }] }),
            ),
            album(
                serde_json::json!({ "id": "appears", "artists": [{ "id": "r2", "name": "Tomas" }] }),
            ),
            album(
                serde_json::json!({ "id": "typed", "releaseTypes": ["Album", "Compilation"], "artistId": "r1" }),
            ),
            album(serde_json::json!({ "id": "own-b", "artistId": "r1" })),
            album(serde_json::json!({ "id": "elsewhere", "artistId": "r2" })),
            album(serde_json::json!({ "id": "nobody" })),
            album(
                serde_json::json!({ "id": "duet", "artists": [{ "id": "r2", "name": "Tomas" }, { "id": "r1", "name": "Mara" }] }),
            ),
        ];
        let own: Vec<&str> = own_albums_of(&artist, &albums)
            .map(|album| album.id.as_str())
            .collect();
        assert_eq!(own, ["own-a", "own-b", "duet"]);
    }

    #[test]
    fn biographies_as_plain_text() {
        let text = plain("<p>One &amp; two &lt;3</p><P>Three<br/>four <a href=\"x\">more</a></p>");
        assert_eq!(text, "One & two <3\n\nThree\nfour more");
        assert_eq!(start_of(&text), "One & two <3");
        assert_eq!(plain("No tags at all."), "No tags at all.");
        // A long paragraph is cut at a word, before the limit.
        let long = "word, ".repeat(100);
        let start = start_of(&long);
        assert!(start.ends_with("word…"), "{start}");
        assert!(start.chars().count() <= ABOUT_SHORT + 1);
    }

    #[test]
    fn durations_and_counts() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(245), "4:05");
        assert_eq!(duration(3725), "1:02:05");
        assert_eq!(count(1, "track", "tracks"), "1 track");
        assert_eq!(count(9, "track", "tracks"), "9 tracks");
    }

    #[test]
    fn home_counts_the_tracks_the_tracks_page_lists() {
        let album = r#"[{"id":"a1","name":"A","songCount":2,"duration":1,"created":""}]"#;
        let mut b = Browser {
            library: serde_json::from_str(album).unwrap(),
            library_loaded: true,
            ..Browser::default()
        };
        assert_eq!(
            track_count(&b),
            None,
            "until the server says whether it counts"
        );
        b.scanned_tracks_asked = true;
        assert_eq!(
            track_count(&b),
            Some(2),
            "the albums' songs, from one that doesn't"
        );
        b.scanned_tracks = Some(3);
        assert_eq!(track_count(&b), Some(3), "the scan's count");
        b.song_list = serde_json::from_str(r#"[{"id":"s1","isDir":false,"title":"A"}]"#).unwrap();
        b.song_list_loaded = true;
        assert_eq!(track_count(&b), Some(1), "the tracks page's songs");
    }

    #[test]
    fn a_heart_put_back_lands_only_on_its_own_table() {
        let mut b = Browser {
            song_list: serde_json::from_str(r#"[{"id":"s1","isDir":false,"title":"A"}]"#).unwrap(),
            song_view: vec![0],
            compositions: serde_json::from_str(r#"[{"id":"w1","name":"B"}]"#).unwrap(),
            view: vec![0],
            ..Browser::default()
        };
        b.lines.push(TrackLine::default());
        let hearts = |b: &Browser| b.lines.row_data(0).unwrap().loved;
        // A song's heart put back while the compositions are on screen.
        mark_loved(&mut b, Page::Compositions, "s1", true);
        assert!(b.song_list[0].starred.is_some());
        assert!(!hearts(&b));
        mark_piece_loved(&mut b, Page::Compositions, "w1", true);
        assert!(hearts(&b));
        // A composition's put back while the tracks are.
        mark_piece_loved(&mut b, Page::Tracks, "w1", false);
        assert!(b.compositions[0].starred.is_none());
        assert!(hearts(&b));
        mark_loved(&mut b, Page::Tracks, "s1", false);
        assert!(!hearts(&b));
    }

    #[test]
    fn a_work_is_selected_when_all_its_songs_are_whichever_way_they_were_picked() {
        // A work of two songs, then the first of them again on its own.
        let songs: Vec<Child> = ["a", "b", "a"]
            .iter()
            .map(|id| {
                serde_json::from_str(&format!(r#"{{"id":"{id}","isDir":false,"title":"t"}}"#))
                    .unwrap()
            })
            .collect();
        let links: Vec<String> = songs.iter().map(song_link).collect();
        let mut b = Browser::default();
        b.tracks.set_vec(vec![
            Track {
                work_tracks: 2,
                ..Track::default()
            },
            Track::default(),
            Track::default(),
        ]);
        b.songs = songs;
        let shown = |b: &Browser| -> (Vec<bool>, bool) {
            let rows = b.tracks.iter().map(|track| track.selected).collect();
            (rows, b.tracks.row_data(0).unwrap().work_selected)
        };
        // Picked one by one, the work counts once both are; the song on two
        // rows shows on both.
        pick(&mut b, &links, 0, false);
        show_track_selection(&b);
        assert_eq!(shown(&b), (vec![true, false, true], false));
        pick(&mut b, &links, 1, false);
        show_track_selection(&b);
        assert_eq!(shown(&b), (vec![true, true, true], true));
        // The work lets all go at once when all are picked, else picks all.
        pick_all(&mut b, &links[..2]);
        show_track_selection(&b);
        assert_eq!(shown(&b), (vec![false, false, false], false));
        pick(&mut b, &links, 1, false);
        pick_all(&mut b, &links[..2]);
        assert_eq!(b.selected, ["song:b", "song:a"]);
        assert_eq!(b.anchor.as_deref(), Some("song:a"));
        // A work with nothing under it is never selected.
        b.tracks.set_row_data(0, Track::default());
        show_track_selection(&b);
        assert!(!shown(&b).1);
    }

    #[test]
    fn the_selection_is_in_the_order_of_the_page_and_the_rest_in_the_order_picked() {
        let song = |id: &str| -> Child {
            serde_json::from_str(&format!(r#"{{"id":"{id}","isDir":false,"title":"t"}}"#)).unwrap()
        };
        let tile = |link: &str| AlbumTile {
            link: link.into(),
            ..AlbumTile::default()
        };
        // The tracks table lists the same songs, sorted the other way.
        let b = Browser {
            songs: vec![song("a"), song("b")],
            song_list: vec![song("a"), song("b")],
            song_view: vec![1, 0],
            selected: ["album:gone", "album:y", "song:b", "album:x", "song:a"]
                .map(String::from)
                .to_vec(),
            ..Browser::default()
        };
        b.tiles.set_vec(vec![tile("album:x"), tile("album:y")]);
        // An artist's top songs are above their albums.
        assert_eq!(
            in_page_order(&b, Page::Artist),
            ["song:a", "song:b", "album:x", "album:y", "album:gone"]
        );
        // What is kept has its albums above its songs.
        assert_eq!(
            in_page_order(&b, Page::Downloads),
            ["album:x", "album:y", "song:a", "song:b", "album:gone"]
        );
        // A table's rows go as sorted.
        assert_eq!(
            in_page_order(&b, Page::Tracks),
            ["song:b", "song:a", "album:gone", "album:y", "album:x"]
        );
    }

    #[test]
    fn selected_rows_next_to_each_other_join_and_a_gap_splits_them() {
        let (no, yes) = ((false, false), true);
        assert_eq!(joined(&[]), []);
        assert_eq!(joined(&[true]), [no]);
        assert_eq!(
            joined(&[true, true, true, false, true]),
            [(false, yes), (yes, yes), (yes, false), no, no]
        );
    }

    #[test]
    fn a_selected_work_joins_its_movements_and_the_rows_around_them() {
        let songs: Vec<Child> = ["a", "b", "c", "d", "a"]
            .iter()
            .map(|id| {
                serde_json::from_str(&format!(r#"{{"id":"{id}","isDir":false,"title":"t"}}"#))
                    .unwrap()
            })
            .collect();
        let links: Vec<String> = songs.iter().map(song_link).collect();
        let mut b = Browser::default();
        // A song on its own, a work of two movements, a song on its own, and
        // the first song again.
        b.tracks.set_vec(vec![
            Track::default(),
            Track {
                work_tracks: 2,
                ..Track::default()
            },
            Track::default(),
            Track::default(),
            Track::default(),
        ]);
        b.songs = songs;
        let joins = |b: &Browser| -> Vec<(bool, bool)> {
            b.tracks
                .iter()
                .map(|track| (track.joined_above, track.joined_below))
                .collect()
        };
        let work_above = |b: &Browser| b.tracks.row_data(1).unwrap().work_joined_above;
        // The work's songs and the song above it, which is on the last row
        // too: the work's row joins the song above, and the first movement
        // joins the work's row.
        pick_all(&mut b, &links[1..3]);
        pick(&mut b, &links, 0, false);
        show_track_selection(&b);
        assert_eq!(
            joins(&b),
            [
                (false, true),
                (true, true),
                (true, false),
                (false, false),
                (false, false)
            ]
        );
        assert!(work_above(&b));
        // With nothing selected above it, the work's row starts the block.
        pick(&mut b, &links, 0, false);
        show_track_selection(&b);
        assert_eq!(
            joins(&b),
            [
                (false, false),
                (true, true),
                (true, false),
                (false, false),
                (false, false)
            ]
        );
        assert!(!work_above(&b));
        // Letting the first movement go leaves the work unselected, so the
        // second movement stands alone.
        pick(&mut b, &links, 1, false);
        show_track_selection(&b);
        assert_eq!(joins(&b), [(false, false); 5]);
        // A first movement alone does not make the work selected, so it joins
        // nothing.
        pick(&mut b, &links, 2, false);
        pick(&mut b, &links, 1, false);
        show_track_selection(&b);
        assert_eq!(joins(&b), [(false, false); 5]);
        assert!(!work_above(&b));
    }

    #[test]
    #[cfg(not(target_family = "wasm"))]
    fn a_folder_plays_its_songs_in_the_rows_order_without_its_folders() {
        let browser = Shared::new(RefCell::new(Browser {
            entries: serde_json::from_str(
                r#"[{"id":"d1","isDir":true,"title":"CD 1"},{"id":"s1","isDir":false,"title":"A"},
                    {"id":"d2","isDir":true,"title":"CD 2"},{"id":"s2","isDir":false,"title":"B"}]"#,
            )
            .unwrap(),
            view: vec![2, 3, 0, 1],
            ..Browser::default()
        }));
        let (songs, start) = songs(&browser, Page::Folders, 3);
        let ids: Vec<&str> = songs.iter().map(|song| song.id.as_str()).collect();
        assert_eq!((ids, start), (vec!["s2", "s1"], 1));
    }

    #[test]
    fn only_a_logo_gets_a_plate_from_its_ink() {
        let white = |logo: bool| {
            let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(4, 4);
            let mut buffer = pixels.clone();
            let pixels = buffer.make_mut_slice();
            pixels.fill(slint::Rgba8Pixel::new(0, 0, 0, 0));
            for middle in [5, 6, 9, 10] {
                pixels[middle] = slint::Rgba8Pixel::new(255, 255, 255, 255);
            }
            let mut tile = AlbumTile {
                logo,
                ..AlbumTile::default()
            };
            show_picture(&mut tile, Image::from_rgba8(buffer));
            tile
        };
        assert!(white(true).dark_plate);
        assert!(!white(false).dark_plate);
        assert_eq!(white(true).cover.size().width, 4);
    }

    #[test]
    fn the_catalogue_names_music_genres_as_the_radio_page_does() {
        assert_eq!(genre_title("r&b"), "R&B");
        assert_eq!(genre_title("hip hop"), "Hip hop");
        assert_eq!(genre_title("children's music"), "Children's music");
    }

    #[test]
    fn periods_in_the_order_they_came_and_languages_by_name() {
        let pieces: Vec<Composition> = serde_json::from_str(
            r#"[{"id":"w1","name":"A","period":"Romantic"},{"id":"w2","name":"B"},
                {"id":"w3","name":"C","period":"Baroque"},{"id":"w4","name":"D","period":"Modern"}]"#,
        )
        .unwrap();
        let table = Table {
            sort: PIECE_COLUMNS.iter().position(|c| c.title == "Period"),
            ..Table::default()
        };
        let all = vec![0, 1, 2, 3];
        assert_eq!(sorted(&pieces, all, PIECE_COLUMNS, &table), [2, 0, 3, 1]);
        assert_eq!(language("lat"), "Latin");
        assert_eq!(language("xyz"), "xyz");
    }

    #[test]
    fn compositions_by_their_plays_none_counted_as_0_ties_by_name() {
        let pieces: Vec<Composition> = serde_json::from_str(
            r#"[{"id":"w1","name":"Requiem","playCount":3},{"id":"w2","name":"egmont","playCount":3},
                {"id":"w3","name":"Mass"},{"id":"w4","name":"Symphony","playCount":7}]"#,
        )
        .unwrap();
        let plays = PIECE_COLUMNS.iter().position(|c| c.title == "Plays");
        assert!(
            PIECE_COLUMNS[plays.unwrap()].is_count,
            "most first at first"
        );
        let table = |descending| Table {
            sort: plays,
            descending,
            ..Table::default()
        };
        let all = vec![0, 1, 2, 3];
        assert_eq!(
            sorted(&pieces, all.clone(), PIECE_COLUMNS, &table(true)),
            [3, 1, 0, 2]
        );
        assert_eq!(
            sorted(&pieces, all, PIECE_COLUMNS, &table(false)),
            [2, 1, 0, 3]
        );
    }

    #[test]
    fn a_server_that_counts_no_plays_shows_no_plays_column() {
        let plays = PIECE_COLUMNS.len() - 1;
        assert_eq!(PIECE_COLUMNS[plays].title, "Plays");
        let saved = Table {
            shown: PIECE_COLUMNS.iter().map(|_| true).collect(),
            sort: Some(plays),
            descending: true,
        };
        let without = offered_piece_table(&saved, false);
        assert_eq!(without.shown(), (0..plays).collect::<Vec<_>>());
        assert_eq!(without.sort, None);
        let with = offered_piece_table(&saved, true);
        assert_eq!(with.shown(), (0..=plays).collect::<Vec<_>>());
        assert_eq!((with.sort, with.descending), (Some(plays), true));
        let by_date = Table {
            sort: Some(3),
            ..saved
        };
        assert_eq!(offered_piece_table(&by_date, false).sort, Some(3));
    }

    #[test]
    fn by_name_either_way_or_by_plays_ties_in_the_servers_order() {
        let plays = [2, 5, 0, 2];
        let view = vec![0, 1, 2, 3];
        assert_eq!(by_name(view.clone(), false), [0, 1, 2, 3]);
        assert_eq!(by_name(view.clone(), true), [3, 2, 1, 0]);
        assert_eq!(by_plays(view.clone(), true, |i| plays[i]), [1, 0, 3, 2]);
        assert_eq!(by_plays(view, false, |i| plays[i]), [2, 0, 3, 1]);
    }

    #[test]
    fn a_genre_takes_the_banner_of_the_best_known_artist_that_has_one() {
        let name = |id: &str| Name {
            label: id.into(),
            link: format!("artist:{id}").into(),
        };
        let known = [(name("r1"), 3), (name("r2"), 2), (name("r3"), 1)];
        let (second, third) = ("banner-r2".to_owned(), "banner-r3".to_owned());
        let banners = HashMap::from([("r2", &second), ("r3", &third)]);
        let none = HashSet::new();
        assert_eq!(
            banner_of(&known, &banners, &none),
            Some("banner-r2".to_owned())
        );
        assert_eq!(
            banner_of(&known[2..], &banners, &none),
            Some("banner-r3".to_owned())
        );
        assert_eq!(banner_of(&known[..1], &banners, &none), None);
        assert_eq!(banner_of(&known, &HashMap::new(), &none), None);
        let taken = HashSet::from(["banner-r2".to_owned()]);
        assert_eq!(
            banner_of(&known, &banners, &taken),
            Some("banner-r3".to_owned())
        );
    }

    #[test]
    fn no_two_tiles_show_the_same_banner() {
        let name = |id: &str| Name {
            label: id.into(),
            link: format!("artist:{id}").into(),
        };
        let (b1, b2) = ("banner-r1".to_owned(), "banner-r2".to_owned());
        let banners = HashMap::from([("r1", &b1), ("r2", &b2)]);
        // Three genres whose best known is r1, the second and third also
        // know r2, and the fourth only the one before; the most albums choose
        // first, so the later ones take what is left, else plain.
        let known = vec![
            vec![(name("r1"), 3)],
            vec![(name("r1"), 3), (name("r2"), 1)],
            vec![(name("r1"), 3), (name("r2"), 1)],
            vec![(name("r3"), 1)],
        ];
        assert_eq!(
            distinct_banners(known.into_iter(), &banners),
            [Some(b1), Some(b2), None, None]
        );
    }

    #[test]
    fn only_servers_with_banners_and_artists_that_have_one_give_banners() {
        let artists: Vec<ArtistID3> = serde_json::from_str(
            r#"[{"id":"r1","name":"Has","bannerArt":"ab-r1"},
                {"id":"r2","name":"Empty","bannerArt":""},
                {"id":"r3","name":"None"}]"#,
        )
        .unwrap();
        let banner = "ab-r1".to_owned();
        assert_eq!(
            artist_banners(&artists, true),
            HashMap::from([("r1", &banner)])
        );
        assert!(artist_banners(&artists, false).is_empty());
    }

    #[test]
    fn a_genre_is_described_only_when_the_server_says_so() {
        let genre = |text: Option<&str>, source: Option<&str>, url: Option<&str>| Genre {
            value: "Jazz".into(),
            song_count: 0,
            album_count: 0,
            parents: Vec::new(),
            description: text.map(str::to_owned),
            description_source: source.map(str::to_owned),
            description_url: url.map(str::to_owned),
        };
        assert_eq!(
            description_of(&genre(None, Some("Wikipedia"), None), true),
            None
        );
        assert_eq!(description_of(&genre(Some("  "), None, None), true), None);
        let url = "https://en.wikipedia.org/wiki/Jazz";
        assert_eq!(
            description_of(&genre(Some("Music. "), Some("Wikipedia"), Some(url)), true),
            Some(Description {
                text: "Music.".into(),
                source: "Wikipedia".into(),
                url: url.into(),
            })
        );
        // Only a web page is ever opened.
        let described = description_of(
            &genre(Some("Music."), None, Some("file:///etc/passwd")),
            true,
        );
        assert_eq!(described.unwrap().url, "");
        // Only the version of the extension that has them.
        assert_eq!(
            description_of(&genre(Some("Music."), None, None), false),
            None
        );
    }

    #[test]
    fn a_library_genre_asks_the_directory_by_the_radio_pages_tag() {
        assert_eq!(directory_tag("World"), "world music");
        assert_eq!(directory_tag("R&B"), "rnb");
        assert_eq!(directory_tag("Hip-Hop"), "hip hop");
        assert_eq!(directory_tag("New Age"), "new age");
    }

    #[test]
    fn genres_by_the_librarys_albums() {
        let genre = |value: &str, album_count| Genre {
            value: value.into(),
            song_count: 0,
            album_count,
            parents: Vec::new(),
            description: None,
            description_source: None,
            description_url: None,
        };
        let ranks = genre_ranks(&[
            genre("Rock", 3),
            genre("Hip-Hop", 4),
            genre("R&B", 1),
            genre("rnb", 4),
            genre("World Music", 2),
            genre("Polka", 9),
            genre("Pop/Rock", 2),
        ]);
        let mut order: Vec<usize> = (0..GENRES.len()).collect();
        order.sort_by_key(|&genre| ranks[genre]);
        let titles: Vec<&str> = order.iter().take(6).map(|&genre| GENRES[genre].0).collect();
        assert_eq!(
            titles,
            ["Top worldwide", "Rock", "Hip hop", "R&B", "Pop", "World"]
        );
        assert_eq!(genre_ranks(&[]), Vec::from_iter(0..GENRES.len()));
    }

    #[test]
    fn each_role_under_its_own_name() {
        let spec = [
            "adminRole",
            "settingsRole",
            "streamRole",
            "downloadRole",
            "uploadRole",
            "coverArtRole",
            "commentRole",
            "podcastRole",
            "shareRole",
            "jukeboxRole",
            "videoConversionRole",
        ];
        for (i, role) in spec.iter().enumerate() {
            let roles: Vec<String> = spec
                .iter()
                .map(|other| format!(r#""{other}":{}"#, other == role))
                .collect();
            let user = format!(
                r#"{{"username":"u","scrobblingEnabled":false,"playlistRole":false,{}}}"#,
                roles.join(",")
            );
            let user: User = serde_json::from_str(&user).unwrap();
            assert_eq!(role_text(&user), ROLE_NAMES[i], "{role}");
        }
    }

    #[test]
    fn playlist_folders_in_the_users_order() {
        let folders: Vec<PlaylistFolder> = serde_json::from_str(
            r#"[{"id":"f1","name":"Work","sortOrder":2},
                {"id":"f2","name":"Road trips","sortOrder":1},
                {"id":"f3","name":"Sub","parentId":"f1"},
                {"id":"f4","name":"Orphan","parentId":"gone"}]"#,
        )
        .unwrap();
        let playlists: Vec<Playlist> = serde_json::from_str(
            r#"[{"id":"p1","name":"Late night","songCount":0,"duration":0,"created":"2026-01-01T00:00:00Z","changed":"2026-01-01T00:00:00Z"},
                {"id":"p2","name":"Commute","songCount":0,"duration":0,"created":"2026-01-01T00:00:00Z","changed":"2026-01-01T00:00:00Z","playlistFolderId":"f1"},
                {"id":"p3","name":"Zed","songCount":0,"duration":0,"created":"2026-01-01T00:00:00Z","changed":"2026-01-01T00:00:00Z","playlistFolderId":"","playlistFolderSortOrder":0}]"#,
        )
        .unwrap();
        let shown: Vec<(String, &str)> = sections(&playlists, false, true, false)
            .into_iter()
            .skip_while(|(link, ..)| link != "playlists")
            .map(|(link, label, _)| (link, label))
            .collect();
        assert_eq!(
            shown,
            [
                ("playlists".to_owned(), "Playlists"),
                ("playlist:p1".to_owned(), "Late night"),
                ("playlist:p2".to_owned(), "Commute"),
                ("playlist:p3".to_owned(), "Zed"),
            ]
        );
        let with_tags = sections(&playlists, false, true, false);
        assert!(with_tags.iter().any(|(link, ..)| link == "tags"));
        let inside: Vec<&str> = placed(&playlists, Some(&folders), Some("f1"))
            .into_iter()
            .map(|item| match item {
                Placed::Folder(folder) => folder.name.as_str(),
                Placed::Playlist(playlist) => playlist.name.as_str(),
            })
            .collect();
        assert_eq!(inside, ["Commute", "Sub"]);
        let flat = placed(&playlists, None, None).len();
        assert_eq!(flat, 3);

        let nodes = folder_nodes(&folders);
        let depths: Vec<(&str, usize)> = tree(&nodes).iter().map(|&(n, d)| (n.1, d)).collect();
        assert_eq!(
            depths,
            [("Orphan", 0), ("Road trips", 0), ("Work", 0), ("Sub", 1)]
        );
        assert_eq!(path(&nodes, "f3"), "Work / Sub");
        let labels = |targets: Vec<Name>| -> Vec<String> {
            targets.iter().map(|name| name.label.to_string()).collect()
        };
        // A folder can't go into itself or what is in it.
        assert_eq!(
            labels(destinations(&nodes, Some("f1"), None)),
            ["Orphan", "Road trips"]
        );
        let from_sub = destinations(&nodes, None, Some("f3"));
        assert_eq!(
            labels(from_sub.clone()),
            ["Top level", "Orphan", "Road trips", "Work"]
        );
        assert_eq!(from_sub[0].link, "");
        assert!(within(&nodes, "f3", "f1"));
        assert!(!within(&nodes, "f1", "f3"));
    }

    #[test]
    fn a_kept_copy_answers_when_the_server_cant_or_isnt_asked() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let unreachable = || async {
                let client = Client::new("http://127.0.0.1:1", opensubsonic::Auth::None).unwrap();
                client.ping().await.map(|_| "from the server")
            };
            let kept = || Some("kept");
            let never = async { panic!("the server was asked") };
            assert_eq!(saved_or(true, kept, never).await.unwrap(), "kept");
            assert_eq!(saved_or(false, kept, unreachable()).await.unwrap(), "kept");
            assert!(saved_or(false, || None, unreachable()).await.is_err());
            assert!(saved_or(true, || None, unreachable()).await.is_err());
            let refused = async { Err(opensubsonic::Error::Status(403)) };
            assert!(matches!(
                saved_or::<&str>(false, kept, refused).await,
                Err(opensubsonic::Error::Status(403))
            ));
            let answered = async { Ok("from the server") };
            assert_eq!(
                saved_or(false, kept, answered).await.unwrap(),
                "from the server"
            );
        });
    }

    #[test]
    fn composers_only_where_the_server_has_them() {
        let labels = |compositions| -> Vec<String> {
            sections(&[], compositions, false, false)
                .into_iter()
                .map(|(_, label, _)| label.to_owned())
                .collect()
        };
        assert_eq!(
            labels(false),
            [
                "Home",
                "Genres",
                "Radio",
                "History",
                "Albums",
                "Artists",
                "Tracks",
                "Folders",
                "Playlists"
            ]
        );
        assert_eq!(
            labels(true),
            [
                "Home",
                "Genres",
                "Radio",
                "History",
                "Albums",
                "Artists",
                "Tracks",
                "Composers",
                "Compositions",
                "Folders",
                "Playlists"
            ]
        );
        let kept: Vec<(&str, &str)> = sections(&[], false, false, true)
            .into_iter()
            .map(|(_, label, group)| (label, group))
            .take_while(|&(_, group)| group == "Browse")
            .collect();
        assert_eq!(
            kept,
            [
                ("Home", "Browse"),
                ("Genres", "Browse"),
                ("Radio", "Browse"),
                ("History", "Browse"),
                ("Downloads", "Browse")
            ]
        );
    }

    #[test]
    fn tags_in_browse_after_genres_where_the_server_has_them() {
        let entries = sections(&[], false, true, true);
        let browse: Vec<&str> = entries
            .iter()
            .filter(|(.., group)| *group == "Browse")
            .map(|(_, label, _)| *label)
            .collect();
        assert_eq!(
            browse,
            ["Home", "Genres", "Tags", "Radio", "History", "Downloads"]
        );
        assert!(
            entries
                .iter()
                .all(|(_, label, group)| *label != "Tags" || *group == "Browse")
        );
        assert!(
            sections(&[], false, false, true)
                .iter()
                .all(|(link, ..)| link != "tags")
        );
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

    #[test]
    fn works_and_their_tracks() {
        let song = |title: &str, work: &str, extra: &str| -> Child {
            serde_json::from_str(&format!(
                r#"{{"id":"s","isDir":false,"title":"{title}","works":[{{"name":"{work}"{extra}}}]}}"#
            ))
            .unwrap()
        };
        let moved = |title: &str, work: &str, movement: &str| {
            song(
                title,
                work,
                &format!(r#"}}],"movements":[{{"name":"{movement}""#),
            )
        };

        // Two editions of one ballet are one work; no works is none.
        let swan = "Swan Lake, op. 20";
        assert!(same_work(
            &song("a", swan, r#","musicBrainzId":"11f48c5e""#),
            &song("b", swan, r#","musicBrainzId":"13867eb1""#)
        ));
        assert!(!same_work(
            &song("a", swan, ""),
            &song("b", "Raymonda, op. 57", "")
        ));
        let bare: Child = serde_json::from_str(r#"{"id":"s","isDir":false,"title":"t"}"#).unwrap();
        assert!(!same_work(&bare, &bare));

        // A movement of its own names the track.
        let concerto = "Violin Concerto in D minor, op. 47";
        let run = [
            moved("x", concerto, "I. Allegro moderato"),
            moved("y", concerto, "II. Adagio di molto"),
        ];
        assert_eq!(track_title(&run, 0), "I. Allegro moderato");
        assert_eq!(track_title(&run, 1), "II. Adagio di molto");

        // Tracks sharing one scene keep their own titles, without the
        // opera's name in front.
        let opera = "Das Rheingold, WWV 86A";
        let run = [
            moved(
                "Das Rheingold: Vorspiel - Prelude",
                opera,
                "Scene I. Vorspiel",
            ),
            moved(
                "Das Rheingold: Weia! Waga! Woge, du Welle!",
                opera,
                "Scene I",
            ),
            moved(
                "Das Rheingold: Garstig glatter glitschriger Glimmer!",
                opera,
                "Scene I",
            ),
        ];
        assert_eq!(track_title(&run, 0), "Scene I. Vorspiel");
        assert_eq!(track_title(&run, 1), "Weia! Waga! Woge, du Welle!");
        assert_eq!(
            track_title(&run, 2),
            "Garstig glatter glitschriger Glimmer!"
        );

        // The whole work's name in front, colons and all.
        let score = "Star Wars, Episode I: The Phantom Menace";
        let run = [song(&format!("{score}: Duel of the Fates"), score, "")];
        assert_eq!(track_title(&run, 0), "Duel of the Fates");

        // Another work's name in front stays, as does a title with
        // nothing after the name.
        let run = [song("Symphony no. 1: I. Allegro", "Symphony no. 10", "")];
        assert_eq!(track_title(&run, 0), "Symphony no. 1: I. Allegro");
        let run = [song("Raymonda: ", "Raymonda, op. 57", "")];
        assert_eq!(track_title(&run, 0), "Raymonda: ");

        // One movement over several tracks, under another name for the work.
        let part = |n: u8| {
            moved(
                &format!("Piano Concerto no. 1, op. 23: I. Allegro, Pt. {n}"),
                "Concerto for Piano and Orchestra no. 1, op. 23",
                "I. Allegro",
            )
        };
        assert_eq!(track_title(&[part(1), part(2)], 1), "I. Allegro, Pt. 2");
    }

    #[test]
    fn genres_from_the_library() {
        let album = |json: &str| -> AlbumID3 {
            serde_json::from_str(&format!(
                r#"{{"id":"a","name":"n","songCount":0,"duration":0,{json}}}"#
            ))
            .unwrap()
        };
        let library = [
            album(
                r#""coverArt":"old","created":"2020-01-01","artistId":"r1","artist":"Zed","genres":[{"name":"Blues"},{"name":"Blues Rock"}]"#,
            ),
            album(
                r#""coverArt":"new","created":"2024-01-01","artistId":"r2","artist":"amy","genres":[{"name":"Blues"},{"name":"Blues Rock"},{"name":"Rock"}]"#,
            ),
            album(
                r#""coverArt":"one","created":"2022-01-01","artistId":"r1","artist":"Zed","genre":"Blues""#,
            ),
            album(r#""created":"2023-01-01","artist":"Nobody","genre":"Rock""#),
            album(
                r#""created":"2021-01-01","artistId":"r1","artist":"Zed","genres":[{"name":"Rock"}]"#,
            ),
            album(r#""created":"2021-01-01","artistId":"r2","artist":"amy","genre":"Blues""#),
        ];

        // A list of genres, else the one genre.
        assert!(carries(&library[0], "Blues Rock"));
        assert!(!carries(&library[0], "Rock"));
        assert!(carries(&library[2], "Blues"));
        assert!(!carries(&library[2], "Blues Rock"));

        // A tile opens its genre and has how many albums carry it.
        let genre = |value: &str| Genre {
            value: value.into(),
            song_count: 0,
            album_count: 2,
            parents: Vec::new(),
            description: None,
            description_source: None,
            description_url: None,
        };
        let genres = [
            genre("Blues"),
            genre("Blues Rock"),
            genre("Rock"),
            genre("Polka"),
        ];
        let artist = |id: &str, albums: i64| -> ArtistID3 {
            serde_json::from_str(&format!(
                r#"{{"id":"{id}","name":"{id}","coverArt":"ar-{id}","albumCount":{albums}}}"#
            ))
            .unwrap()
        };
        let mut b = Browser {
            library: library.to_vec(),
            artists: vec![artist("r1", 10), artist("r2", 1)],
            genres: genres.to_vec(),
            ..Browser::default()
        };
        let tiles = genre_tiles(&b, &genres);
        let (tile, picture) = &tiles[1];
        assert_eq!(tile.link, "genre:Blues Rock");
        assert_eq!(tile.names.row_data(0).unwrap().label, "2 albums");
        assert_eq!(*picture, None, "not before the library and its artists");

        // Those most albums carry first, then by name.
        let counted = |value: &str, albums: i64| Genre {
            album_count: albums,
            ..genre(value)
        };
        let sorted: Vec<String> =
            by_albums([counted("b", 1), counted("A", 1), counted("c", 3)].iter())
                .into_iter()
                .map(|genre| genre.value)
                .collect();
        assert_eq!(sorted, ["c", "A", "b"]);

        // The artists known for a genre best: the most albums with it as
        // their main genre among its siblings, the first of them they list,
        // then the most carrying it; each weighed by how much of their own
        // albums that is.
        let known = |genre: &str, siblings: &[&str]| -> Vec<(String, usize)> {
            let siblings: HashSet<&str> = siblings.iter().copied().collect();
            known_for(&b.library, &b.artists, genre, &siblings)
                .into_iter()
                .map(|(name, albums)| (name.link.into(), albums))
                .collect()
        };
        let tops = ["Blues", "Rock"];
        assert_eq!(
            known("Blues", &tops),
            [("artist:r2".to_owned(), 2), ("artist:r1".to_owned(), 2)]
        );
        assert_eq!(
            known("Rock", &tops),
            [("artist:r1".to_owned(), 1), ("artist:r2".to_owned(), 1)]
        );
        assert!(known("Polka", &tops).is_empty());
        // Under its own name, not its albums' credit.
        let siblings: HashSet<&str> = tops.into_iter().collect();
        let rock = known_for(&b.library, &b.artists, "Rock", &siblings);
        assert_eq!(
            (rock[0].0.label.as_str(), b.library[4].artist.as_deref()),
            ("r1", Some("Zed"))
        );

        // Its albums by those artists, one of each in turn.
        assert_eq!(genre_albums(&b, "Blues"), [1, 0, 5, 2]);

        // A server without Fonoteca's banners has none, whatever the artists
        // say.
        (b.library_loaded, b.artists_loaded) = (true, true);
        assert!(
            genre_tiles(&b, &genres)
                .iter()
                .all(|(_, banner)| banner.is_none())
        );

        // A picture seen untinted, as on the artists page, is fetched again
        // for the tint.
        let untinted = Image::from_rgba8(SharedPixelBuffer::new(1, 1));
        b.pictures.insert(("ar-r2".to_owned(), 10), untinted);
        let tiles = vec![(AlbumTile::default(), Some("ar-r2".to_owned()))];
        let (model, wanted) = seen_covers(&b, tiles, 10, true);
        assert_eq!(wanted, [(0, "ar-r2".to_owned())]);
        assert_eq!(model.row_data(0).unwrap().cover.size().width, 0);

        // Their light alone, in the tint.
        let tint = |r: u8, g: u8, b: u8| {
            let mut png = Cursor::new(Vec::new());
            let pixel = image::Rgba([r, g, b, 255]);
            image::RgbaImage::from_pixel(1, 1, pixel)
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            tinted(png.get_ref())
                .unwrap()
                .to_rgba8()
                .unwrap()
                .as_slice()[0]
        };
        let red = tint(255, 0, 0);
        assert!(red.r == red.g && red.b > red.r);
        let (dark, pale) = (tint(0, 0, 0), tint(255, 255, 255));
        assert_eq!((dark.r, dark.g, dark.b), (28, 28, 44));
        assert_eq!((pale.r, pale.g, pale.b), (204, 204, 228));
    }
}
