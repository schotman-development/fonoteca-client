//! The library pages after sign-in: what each page shows, where its links lead,
//! and the history behind back and forward. A link is a page's address, such
//! as "home", "albums", "artists", "tracks", "playlists", "album:<id>",
//! "artist:<id>", "playlist:<id>" or "song:<id>", the files' "folders" and
//! "folder:<id>", on servers with playlist folders "playlists:<id>", on
//! servers with Fonoteca's compositions "composers", "compositions",
//! "composer:<id>" or "composition:<id>", on servers with Fonoteca's tags
//! "tags" and "tag:<id>", "radio", and "account" and, for admins, "users".

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frame, Frames};
use opensubsonic::{
    AlbumID3, AlbumListType, ArtistID3, Child, Client, Composer, Composition, ExplicitStatus,
    GenericMediaType, Genre, Indexes, InternetRadioStation, ItemDate, ListOptions, MediaType,
    Playlist, PlaylistFolder, PlaylistFolderUpdate, PlaylistUpdate, PlaylistWithSongs,
    RadioStation, Roles, ScanStatus, SearchOptions, Span, Tag, TagUpdate, User, UserUpdate,
};
use radiobrowser::Station;
use slint::{
    ComponentHandle, Image, Model, ModelRc, SharedPixelBuffer, SharedString, Timer, TimerMode,
    VecModel,
};

use crate::{
    AlbumTile, AppWindow, ColumnChoice, Detail, Icons, Name, Page, Stat, StationRow, StationShelf,
    TagChoice, TagRow, Track, TrackColumn, TrackLine, UserRow, prefs, zone,
};

/// The newest albums on the home page.
const RECENT: usize = 24;
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
/// How many stations a shelf shows, and a genre's page asks for at a time;
/// how many of each language its first page has.
const SHELF_STATIONS: u32 = 12;
const GENRE_STATIONS: u32 = 60;
const GENRE_IN_LANGUAGE: u32 = 100;
/// How the app names itself to the radio directory.
const USER_AGENT: &str = concat!("fonoteca-client/", env!("CARGO_PKG_VERSION"));
/// Tiles added to the albums or artists page at a time.
const TILES_AT_ONCE: usize = 100;
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
const HEADER_SIZE: f32 = 220.0;
/// The size of a cover in a row of the tracks table.
const LINE_COVER_SIZE: f32 = 42.0;
/// Fonoteca's extension for motion covers.
const MOTION: &str = "fonotecaAlbumMotion";
/// Fonoteca's extension for composers and compositions.
const COMPOSITIONS: &str = "fonotecaCompositions";
/// OpenSubsonic's extension for an artist's top songs by id, not name.
const TOP_SONGS_BY_ID: &str = "topSongsByArtistId";
/// The proposed OpenSubsonic extension for folders of playlists.
const FOLDERS: &str = "playlistFolders";
/// Fonoteca's extension for the user's own tags.
const TAGS: &str = "fonotecaTags";
/// How many of an artist's top songs their page shows.
const TOP_SONGS: u32 = 5;
/// About how long the start of a biography or notes is, before More.
const ABOUT_SHORT: usize = 320;

#[derive(Default)]
pub struct Browser {
    client: Option<Rc<Client>>,
    history: Vec<String>,
    at: usize,
    /// Counts the pages shown, so an answer for a page left behind is dropped.
    visit: i32,
    tiles: Rc<VecModel<AlbumTile>>,
    tracks: Rc<VecModel<Track>>,
    /// Song id of each track row.
    songs: Vec<String>,
    /// Tile or tracks table rows whose covers are still to fetch, the rows on
    /// screen or just below it, and how many fetches run.
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
    /// Every composer and every composition, on servers with Fonoteca's
    /// compositions, once they have arrived.
    composers: Vec<Composer>,
    composers_loaded: bool,
    compositions: Vec<Composition>,
    compositions_loaded: bool,
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
    /// Filters the albums or artists once typing pauses.
    typing: Timer,
    /// The links of the albums selected, in the order they were picked. They
    /// stay selected through sorting, filtering and other pages.
    selected: Vec<String>,
    /// The link last picked or let go on the page on screen, where a range
    /// picked with Shift starts.
    anchor: Option<String>,
    /// Takes the notice down.
    notice: Timer,
    /// Asks how the server's scan is going while the account page is up.
    scan: Timer,
    /// The account signed in, once the server has said what it may do, and
    /// the users on the users page.
    me: Option<User>,
    users: Vec<User>,
    /// The stations saved on the server as last asked, and the home page's
    /// shelf of them.
    server_stations: Vec<InternetRadioStation>,
    saved: Rc<VecModel<AlbumTile>>,
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
    /// The pictures of saved stations by stream address, where the
    /// directory has them.
    favicons: HashMap<String, Option<String>>,
}

pub type Shared = Rc<RefCell<Browser>>;

/// Opens the home page with a signed-in client.
pub fn start(ui: &AppWindow, browser: &Shared, client: Client) {
    {
        let mut b = browser.borrow_mut();
        b.client = Some(Rc::new(client));
        b.history.clear();
    }
    load_prefs(ui, browser);
    ui.set_album_filter("".into());
    ui.set_artist_filter("".into());
    ui.set_composer_filter("".into());
    ui.set_composition_filter("".into());
    ui.set_track_filter("".into());
    ui.set_folder_filter("".into());
    ui.set_roles("".into());
    ui.set_may_administer(true);
    sidebar(ui, browser);
    load_playlists(ui, browser);
    load_tags(ui, browser);
    load_me(ui, browser);
    open(ui, browser, "home".into());
}

/// The client signed in with, for calls made outside the pages.
pub fn client(browser: &Shared) -> Option<Rc<Client>> {
    browser.borrow().client.clone()
}

/// Goes on with `client` instead, signed in anew after a password change,
/// on the page on screen.
pub fn swap(browser: &Shared, client: Client) {
    browser.borrow_mut().client = Some(Rc::new(client));
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
    ui.on_show_more({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || more_tiles(&weak.unwrap(), &browser)
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
    ui.on_sort_albums({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            save_prefs(&ui, &browser);
            show(&ui, &browser);
        }
    });
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
    ui.set_role_names(ModelRc::new(VecModel::from(
        ROLE_NAMES.map(SharedString::from).to_vec(),
    )));
    ui.on_prepare_user({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name| {
            let ui = weak.unwrap();
            let b = browser.borrow();
            let user = b.users.iter().find(|user| user.username == name.as_str());
            let flags = user.map_or(NEW_USER_ROLES, role_flags);
            let email = user
                .and_then(|user| user.email.as_deref())
                .unwrap_or_default();
            ui.set_asked_email(email.into());
            ui.set_asked_roles(ModelRc::new(VecModel::from(flags.to_vec())));
        }
    });
    ui.on_create_user({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name, email, password, flags| {
            let (name, email, password) = (
                name.trim().to_owned(),
                email.trim().to_owned(),
                password.to_string(),
            );
            let roles = roles_from(&flags.iter().collect::<Vec<_>>());
            change_users(
                &weak.unwrap(),
                &browser,
                format!("Added {name}."),
                |client| async move {
                    client
                        .create_user(&name, &password, &email, &roles, &[])
                        .await
                },
            );
        }
    });
    ui.on_update_user({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name, email, flags| {
            let (name, email) = (name.to_string(), email.trim().to_owned());
            let roles = roles_from(&flags.iter().collect::<Vec<_>>());
            change_users(
                &weak.unwrap(),
                &browser,
                format!("Saved {name}."),
                |client| async move {
                    let update = UserUpdate {
                        email: Some(email.as_str()).filter(|email| !email.is_empty()),
                        roles,
                        ..UserUpdate::default()
                    };
                    client.update_user(&name, &update).await
                },
            );
        }
    });
    ui.on_set_password({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name, password| {
            let (name, password) = (name.to_string(), password.to_string());
            let done = format!("Changed the password of {name}.");
            change_users(&weak.unwrap(), &browser, done, |client| async move {
                client.change_password(&name, &password).await
            });
        }
    });
    ui.on_station_shelf_in_view({
        let browser = browser.clone();
        move |shelf, first, last| {
            let rows = first.max(0) as usize..(last + 1).max(0) as usize;
            shelf_logos(&browser, shelf as usize, rows);
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
    ui.on_delete_user({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name| {
            let name = name.to_string();
            change_users(
                &weak.unwrap(),
                &browser,
                format!("Deleted {name}."),
                |client| async move { client.delete_user(&name).await },
            );
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
            let song = browser.borrow().songs.get(row as usize).cloned();
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
            let song = browser.borrow().songs.get(row as usize).cloned();
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
    ui.set_selected_tiles(browser.borrow().selected.len() as i32);
}

/// Selects the song or composition of a row of the tracks or compositions
/// table, or lets it go if it is selected. With `range`, selects every row
/// from the one last picked to it instead.
fn select_line(ui: &AppWindow, browser: &Shared, row: usize, range: bool) {
    let page = ui.get_page();
    let mut b = browser.borrow_mut();
    let links: Vec<String> = if page == Page::Compositions {
        b.view
            .iter()
            .map(|&piece| piece_link(&b.compositions[piece]))
            .collect()
    } else {
        let (songs, order) = listed(&b, page);
        order.iter().map(|&song| song_link(&songs[song])).collect()
    };
    let lines = b.lines.clone();
    for (row, selected) in pick(&mut b, &links, row, range) {
        if let Some(mut line) = lines.row_data(row) {
            line.selected = selected;
            lines.set_row_data(row, line);
        }
    }
    ui.set_selected_tiles(b.selected.len() as i32);
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
        b.lines = Rc::default();
        b.songs.clear();
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
    ui.set_details(ModelRc::default());
    ui.set_playlist_editable(false);
    ui.set_shown_id("".into());
    ui.set_move_targets(ModelRc::default());
    ui.set_about("".into());
    ui.set_about_short("".into());
    ui.set_about_open(false);
    ui.set_similar(ModelRc::default());

    let (page, id) = link.split_once(':').unwrap_or((&link, ""));
    let id = id.to_owned();
    match page {
        "home" => {
            ui.set_page(Page::Home);
            home(ui, browser);
        }
        "artists" => {
            ui.set_page(Page::Artists);
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
            composers(ui, browser);
        }
        "compositions" => {
            ui.set_page(Page::Compositions);
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
        "account" => {
            ui.set_page(Page::Account);
            scan_status(ui, browser);
            // Asks again when the server didn't say at sign-in.
            if browser.borrow().me.is_none() {
                load_me(ui, browser);
            }
        }
        "users" => {
            ui.set_page(Page::Users);
            // Cleared here, not on a reload after a change, which keeps them
            // on screen until the new ones come.
            ui.set_user_rows(ModelRc::default());
            users(ui, browser);
        }
        "radio" => {
            ui.set_page(Page::Radio);
            ui.set_station_rows(ModelRc::default());
            stations(ui, browser);
            directory_shelves(ui, browser);
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

/// Whether the server has this extension.
fn offers(b: &Browser, extension: &str) -> bool {
    b.client
        .as_ref()
        .is_some_and(|client| client.supports(extension))
}

/// Whether the account signed in is one of the server's admins.
fn admin(b: &Browser) -> bool {
    b.me.as_ref().is_some_and(|me| me.admin_role)
}

/// The playlist folders, on servers with them.
fn folders_of(b: &Browser) -> Option<&[PlaylistFolder]> {
    offers(b, FOLDERS).then_some(&b.playlist_folders[..])
}

/// The sidebar's entries for what the server has.
fn sections_of(b: &Browser) -> Vec<(String, &str, &str)> {
    sections(
        &b.playlists,
        folders_of(b),
        offers_compositions(b),
        offers(b, TAGS),
        admin(b),
    )
}

/// The sidebar's entries, as links, labels and groups: the home page and the
/// radio, the library's pages, with the tags, composers and compositions
/// where the server has them, the playlists, or with `folders` the top level
/// of them, the account and for admins the users.
fn sections<'a>(
    playlists: &'a [Playlist],
    folders: Option<&'a [PlaylistFolder]>,
    compositions: bool,
    tags: bool,
    admin: bool,
) -> Vec<(String, &'a str, &'a str)> {
    let library = [
        ("albums", "Albums", true),
        ("artists", "Artists", true),
        ("tracks", "Tracks", true),
        ("tags", "Tags", tags),
        ("composers", "Composers", compositions),
        ("compositions", "Compositions", compositions),
        ("folders", "Folders", true),
    ]
    .into_iter()
    .filter(|&(.., shown)| shown)
    .map(|(link, label, _)| (link.to_owned(), label, "Library"));
    let playlists = placed(playlists, folders, None)
        .into_iter()
        .map(|item| match item {
            Placed::Folder(folder) => (
                format!("playlists:{}", folder.id),
                folder.name.as_str(),
                "Playlists",
            ),
            Placed::Playlist(playlist) => (
                format!("playlist:{}", playlist.id),
                playlist.name.as_str(),
                "Playlists",
            ),
        });
    [
        ("home".to_owned(), "Home", "Browse"),
        ("radio".to_owned(), "Radio", "Browse"),
    ]
    .into_iter()
    .chain(library)
    .chain([("playlists".to_owned(), "Playlists", "Playlists")])
    .chain(playlists)
    .chain([("account".to_owned(), "Account", "")])
    .chain(admin.then(|| ("users".to_owned(), "Users", "")))
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

/// The sidebar entry of the page on screen, or -1 when it has none.
fn section(b: &Browser) -> i32 {
    let link = b.history.get(b.at);
    let at = sections_of(b)
        .iter()
        .position(|section| Some(&section.0) == link);
    at.map_or(-1, |at| at as i32)
}

/// Lists the sidebar's entries, the page on screen's current, the playlists
/// songs can be added to: those the server doesn't call read-only, and
/// whether the server has playlist folders and tags.
fn sidebar(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    let icons = ui.global::<Icons>();
    let (mut labels, mut groups, mut pictures, mut nested) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (link, label, group) in sections_of(&b) {
        labels.push(SharedString::from(label));
        groups.push(SharedString::from(group));
        pictures.push(section_icon(&icons, &link));
        nested.push(link.starts_with("playlists:"));
    }
    ui.set_sections(ModelRc::new(VecModel::from(labels)));
    ui.set_section_groups(ModelRc::new(VecModel::from(groups)));
    ui.set_section_icons(ModelRc::new(VecModel::from(pictures)));
    ui.set_section_nested(ModelRc::new(VecModel::from(nested)));
    ui.set_section(section(&b));
    ui.set_folders_offered(offers(&b, FOLDERS));
    ui.set_tags_offered(offers(&b, TAGS));
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
        "users" => icons.get_people(),
        "radio" => icons.get_radio(),
        _ => icons.get_person(),
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
    saved_stations(ui, browser);
    artists(ui, browser);
    if more {
        composers(ui, browser);
        compositions(ui, browser);
    }
}

/// The home page's counts, each once all of it has arrived, and the newest
/// albums once the library has.
fn show_home(ui: &AppWindow, browser: &Shared) {
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
            stat(
                icons.get_note(),
                "Tracks",
                library.map(|albums| albums.iter().map(|a| a.song_count.max(0) as usize).sum()),
            ),
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
        if library.is_none() || b.tiles.row_count() > 0 {
            return;
        }
        let newest = arranged(&b.library, "", 2, true);
        newest
            .iter()
            .take(RECENT)
            .map(|&i| album_tile(&b.library[i], album_names(&b.library[i])))
            .collect()
    };
    add_tiles(ui, browser, tiles);
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
            let shown = matches!(ui.get_page(), Page::Albums | Page::Home);
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

/// Asks for the playlists, for the sidebar and the playlists page.
fn load_playlists(ui: &AppWindow, browser: &Shared) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    browser.borrow_mut().playlists_loading = true;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let answer = client.get_playlists(None).await;
        // The folders known stay when asking for them fails.
        let folders = if client.supports(FOLDERS) {
            client.get_playlist_folders(None).await.ok()
        } else {
            None
        };
        let Some(ui) = weak.upgrade() else { return };
        let signed_in = browser
            .borrow()
            .client
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, &client));
        if !signed_in {
            return;
        }
        let shown = ui.get_page() == Page::Playlists;
        let (changed, known) = {
            let mut b = browser.borrow_mut();
            b.playlists_loading = false;
            let known = b.playlists_loaded;
            let changed = match answer {
                Ok(list) => {
                    b.playlists_loaded = true;
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
/// numbers the value it sorts by; other columns sort by what they show.
struct Column<T> {
    title: &'static str,
    width: f32,
    shown: bool,
    cell: fn(&T) -> Vec<Name>,
    number: Option<fn(&T) -> Option<f64>>,
}

const fn column<T>(title: &'static str, width: f32, cell: fn(&T) -> Vec<Name>) -> Column<T> {
    Column {
        title,
        width,
        shown: false,
        cell,
        number: None,
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
    numeric("Plays", 56.0, |song| number(song.play_count), |song| float(song.play_count)),
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
/// year it was begun, periods by when they were.
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
    arrange_tracks(ui, browser);
}

/// The table's columns, the one it is sorted by, and the choices: the
/// compositions table's on that page, the folders' on theirs, else the
/// tracks table's.
fn show_columns(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    if ui.get_page() == Page::Compositions {
        set_columns(ui, PIECE_COLUMNS, &b.piece_table);
    } else {
        set_columns(ui, COLUMNS, song_table(&b, ui.get_page()));
    }
}

fn set_columns<T>(ui: &AppWindow, all: &[Column<T>], table: &Table) {
    let columns: Vec<TrackColumn> = table
        .shown()
        .into_iter()
        .map(|c| TrackColumn {
            title: all[c].title.into(),
            width: all[c].width,
            stretch: [TITLE, COMPOSITION].contains(&all[c].title),
            heart: all[c].title == LOVED,
            cover: all[c].title == COVER,
            sorted: table.sort == Some(c),
            descending: table.descending,
        })
        .collect();
    // The columns, the menu button, the gaps between them, and the row's
    // padding.
    let width = columns.iter().map(|column| column.width).sum::<f32>()
        + 32.0
        + 12.0 * columns.len() as f32
        + 16.0;
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
        cells: ModelRc::new(VecModel::from(cells)),
        loved: song.starred.is_some(),
        cover: cover.unwrap_or_default(),
        folder: song.is_dir,
        selected: b.selected.contains(&link),
        album: album.unwrap_or_default(),
        artists: names(artists),
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
    TrackLine {
        cells: ModelRc::new(VecModel::from(cells)),
        loved: piece.starred.is_some(),
        cover: cover.unwrap_or_default(),
        folder: false,
        selected: b.selected.contains(&piece_link(piece)),
        album: Name::default(),
        artists: names(composers),
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
        shows_cover(PIECE_COLUMNS, &b.piece_table)
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
            let options = SearchOptions {
                artist_count: Some(0),
                album_count: Some(0),
                song_count: Some(LIBRARY_AT_ONCE),
                song_offset: Some(browser.borrow().song_list.len() as u32),
                ..SearchOptions::default()
            };
            let answer = client.search3("", &options).await;
            let Some(ui) = weak.upgrade() else { return };
            let signed_in = browser
                .borrow()
                .client
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, &client));
            if !signed_in {
                return;
            }
            let shown = ui.get_page() == Page::Tracks;
            let songs = match answer {
                Ok(result) => result.song,
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
        want_line_covers(&mut b, page, start);
        (more, waiting)
    };
    ui.set_more_tiles(more);
    ui.set_loading(waiting);
    fetch_covers(ui, browser);
}

/// `view`, indexes into `items`, in the table's order: by the column of
/// `all` it is sorted by, with the items that have nothing in it last either
/// way. Equal items, and all of them when it isn't sorted, keep their order.
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
    if let Some(number) = column.number {
        let keys: Vec<Option<f64>> = items.iter().map(number).collect();
        view.sort_by(|&a, &b| last_if_empty(&keys[a], &keys[b], |a, b| turn(a.total_cmp(b))));
    } else {
        let keys: Vec<Option<String>> = items
            .iter()
            .map(|item| {
                let labels: Vec<String> = (column.cell)(item)
                    .iter()
                    .map(|name| name.label.to_lowercase())
                    .collect();
                Some(labels.join(", ")).filter(|text| !text.is_empty())
            })
            .collect();
        view.sort_by(|&a, &b| last_if_empty(&keys[a], &keys[b], |a, b| turn(a.cmp(b))));
    }
    view
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
            table.descending = false;
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
        let columns = table.shown();
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
    // The Sort menu has four entries.
    ui.set_album_sort(prefs.album_sort.clamp(0, 3));
    ui.set_album_descending(prefs.album_descending);
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

/// Saves the order of the albums, tracks, compositions and folders and the
/// tables' columns on this device.
fn save_prefs(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    let (track_columns, track_sort) = titles(COLUMNS, &b.track_table);
    let (composition_columns, composition_sort) = titles(PIECE_COLUMNS, &b.piece_table);
    let (folder_columns, folder_sort) = titles(COLUMNS, &b.entry_table);
    prefs::save(&prefs::Prefs {
        album_sort: ui.get_album_sort(),
        album_descending: ui.get_album_descending(),
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
    if ui.get_page() == Page::Home {
        return show_home(ui, browser);
    }
    let (sort, descending) = (ui.get_album_sort(), ui.get_album_descending());
    let filter = typed_filter(ui).unwrap_or_default();
    let page = ui.get_page();
    let mut b = browser.borrow_mut();
    let view = match page {
        Page::Artists => filtered(&b.artists, &filter, |artist| artist.name.clone()),
        Page::Composers => filtered(&b.composers, &filter, |composer| composer.name.clone()),
        Page::Compositions => {
            let view = filtered(&b.compositions, &filter, composition_text);
            sorted(&b.compositions, view, PIECE_COLUMNS, &b.piece_table)
        }
        _ => arranged(&b.library, &filter, sort, descending),
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

/// Shows the next tiles or rows, and asks for the library, the artists, the
/// songs or the playlists again if that failed, or what the home page lacks.
fn more_tiles(ui: &AppWindow, browser: &Shared) {
    ui.set_page_problem("".into());
    if ui.get_page() == Page::Home {
        return home(ui, browser);
    }
    if ui.get_page() == Page::Stations {
        return more_stations(ui, browser);
    }
    if ui.get_page() == Page::Radio {
        stations(ui, browser);
        return directory_shelves(ui, browser);
    }
    if ui.get_page() == Page::Tags {
        return tags(ui, browser);
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
    let (have_artists, have_composers, have_compositions) = {
        let b = browser.borrow();
        (b.artists_loaded, b.composers_loaded, b.compositions_loaded)
    };
    match page {
        Page::Artists if !have_artists => return artists(ui, browser),
        Page::Composers if !have_composers => return composers(ui, browser),
        Page::Compositions if !have_compositions => return compositions(ui, browser),
        Page::Artists | Page::Composers | Page::Compositions => {}
        _ => {
            let ask = {
                let b = browser.borrow();
                !b.library_loaded && !b.library_loading
            };
            if ask {
                load_library(ui, browser);
            }
        }
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
        browser.borrow().piece_table.shown(),
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
        // The library arrives a part at a time, the others at once.
        let library = page == Page::Albums;
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
    aside(
        ui,
        browser,
        |client| async move { client.get_album_info2(&notes).await },
        |ui, _, info| show_about(ui, info.notes.as_deref()),
    );
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
                        ..Track::default()
                    });
                }
            }
            b.songs = songs.into_iter().map(|song| song.id).collect();
        },
    );
}

/// An artist's page: their picture and name, what the server says about
/// them, their top songs and the artists like them, and their albums.
fn artist(ui: &AppWindow, browser: &Shared, id: String) {
    let info = id.clone();
    aside(
        ui,
        browser,
        |client| async move { client.get_artist_info2(&info, None, None).await },
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
        |client| async move { client.get_artist(&id).await },
        move |ui, browser, artist| {
            if !by_id {
                top_songs(ui, browser, artist.artist.name.clone(), false);
            }
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

/// A playlist's tracks: each with its artists and album, and the heart that
/// loves it.
fn playlist(ui: &AppWindow, browser: &Shared, id: String) {
    load(
        ui,
        browser,
        |client| async move { client.get_playlist(&id).await },
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
        move |client| async move {
            if by_id {
                client
                    .get_top_songs_by_artist_id(&artist, Some(TOP_SONGS))
                    .await
            } else {
                client.get_top_songs(&artist, Some(TOP_SONGS)).await
            }
        },
        |_, browser, top| {
            let mut b = browser.borrow_mut();
            b.tracks
                .set_vec(top.song.iter().map(song_track).collect::<Vec<_>>());
            b.songs = top.song.into_iter().map(|song| song.id).collect();
        },
    );
}

/// Like `load`, for what a page shows beside its main content: no spinner,
/// and nothing said when the server can't answer.
fn aside<T: 'static, F: Future<Output = opensubsonic::Result<T>> + 'static>(
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
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = task.await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().visit == visit
            && let Ok(value) = result
        {
            done(&ui, &browser, value);
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

    let tracks: Vec<Track> = songs.iter().map(song_track).collect();
    let mut b = browser.borrow_mut();
    b.tracks.set_vec(tracks);
    b.songs = songs.into_iter().map(|song| song.id).collect();
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
    task: impl FnOnce(Rc<Client>) -> F,
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
            .is_some_and(|current| Rc::ptr_eq(current, &client));
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
        |client| async move { client.get_playlist(&id).await },
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
/// to the top level for "", at once on screen. Those already there stay.
fn move_playlists(ui: &AppWindow, browser: &Shared, ids: Vec<String>, target: String) {
    let (ids, to, on_screen) = {
        let mut b = browser.borrow_mut();
        let to = match target.as_str() {
            "" => "the top level".to_owned(),
            folder => path(&folder_nodes(&b.playlist_folders), folder),
        };
        let ids: Vec<String> = ids
            .into_iter()
            .filter(
                |id| match b.playlists.iter_mut().find(|playlist| playlist.id == *id) {
                    Some(playlist)
                        if playlist.playlist_folder_id.as_deref().unwrap_or("") == target =>
                    {
                        false
                    }
                    Some(playlist) => {
                        playlist.playlist_folder_id = Some(target.clone());
                        true
                    }
                    None => true,
                },
            )
            .collect();
        let on_screen = playlist_on_screen(&b).filter(|id| ids.contains(id));
        (ids, to, on_screen)
    };
    if ids.is_empty() {
        return;
    }
    sidebar(ui, browser);
    playlist_moves(ui, &browser.borrow());
    if ui.get_page() == Page::Playlists {
        show_playlists(ui, browser);
    }
    change_playlist(
        ui,
        browser,
        on_screen,
        |client| async move {
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            client.move_playlists(&ids, Some(&target), &[]).await
        },
        move |ui, browser| notify(ui, browser, format!("Moved to {to}.")),
    );
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
fn remove_from_playlist(ui: &AppWindow, browser: &Shared, row: usize) {
    let Some(id) = playlist_on_screen(&browser.borrow()) else {
        return;
    };
    {
        let mut b = browser.borrow_mut();
        if row >= b.songs.len() || row >= b.tracks.row_count() {
            return;
        }
        b.songs.remove(row);
        b.tracks.remove(row);
    }
    let at = id.clone();
    change_playlist(
        ui,
        browser,
        Some(id),
        |client| async move {
            let update = PlaylistUpdate {
                song_indexes_to_remove: &[row as u32],
                ..PlaylistUpdate::default()
            };
            client.update_playlist(&at, &update).await
        },
        |_, _| {},
    );
}

/// Adds the songs of `links` to the end of the playlist `id`, in their
/// order, and says how many went in: a song's own, an album's, an artist's
/// albums' or a playlist's. Folders and compositions add none.
// ponytail: folders and compositions are skipped; walk them when someone selects those to add.
fn add_to_playlist(ui: &AppWindow, browser: &Shared, id: String, links: Vec<String>) {
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
/// artist's albums' or a playlist's.
async fn songs_of(client: &Client, links: &[String]) -> opensubsonic::Result<Vec<String>> {
    let mut songs = Vec::new();
    for link in links {
        let (kind, id) = link.split_once(':').unwrap_or((link, ""));
        match kind {
            "song" => songs.push(id.to_owned()),
            "album" => songs.extend(client.get_album(id).await?.song.into_iter().map(|s| s.id)),
            "playlist" => songs.extend(
                client
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
            _ => {}
        }
    }
    Ok(songs)
}

/// Puts up a short notice for a few seconds.
fn notify(ui: &AppWindow, browser: &Shared, text: String) {
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
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    if !client.supports(TAGS) {
        return;
    }
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let answer = client.get_tags().await;
        let Some(ui) = weak.upgrade() else { return };
        let signed_in = browser
            .borrow()
            .client
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, &client));
        if !signed_in {
            return;
        }
        let shown = ui.get_page() == Page::Tags;
        let known = browser.borrow().tags_loaded;
        if shown {
            ui.set_loading(false);
        }
        match answer {
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
    task: impl FnOnce(Rc<Client>) -> F,
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
            .is_some_and(|current| Rc::ptr_eq(current, &client));
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
fn start_tagging(ui: &AppWindow, browser: &Shared, links: Vec<String>) {
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
            _ => {}
        }
    }
    Ok(songs)
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

/// Asks the server how its scan of the libraries stands, for the account
/// page.
fn scan_status(ui: &AppWindow, browser: &Shared) {
    aside(
        ui,
        browser,
        |client| async move { client.get_scan_status().await },
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

/// Puts a scan's status on the account page, and asks again every few
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

/// The roles a user can be given, in the order of the dialog's checkboxes
/// and of `role_flags`.
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

/// A new user's roles: what the spec has createUser give one by default,
/// settings and stream.
const NEW_USER_ROLES: [bool; 11] = [
    false, true, true, false, false, false, false, false, false, false, false,
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

/// Every role, on or off, from the dialog's checkboxes.
fn roles_from(flags: &[bool]) -> Roles {
    let flag = |i: usize| flags.get(i).copied();
    Roles {
        admin: flag(0),
        settings: flag(1),
        stream: flag(2),
        download: flag(3),
        upload: flag(4),
        cover_art: flag(5),
        comment: flag(6),
        podcast: flag(7),
        share: flag(8),
        jukebox: flag(9),
        video_conversion: flag(10),
    }
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
/// roles and scan, and the sidebar's users for admins. A server that doesn't
/// say leaves the scan offered.
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
            .is_some_and(|current| Rc::ptr_eq(current, &client));
        if !signed_in {
            return;
        }
        ui.set_roles(user.as_ref().map(role_text).unwrap_or_default().into());
        ui.set_may_administer(user.as_ref().is_none_or(|user| user.admin_role));
        browser.borrow_mut().me = user;
        sidebar(&ui, &browser);
    })
    .expect("the event loop is running");
}

/// The users page: everyone on the server, with their email and roles.
fn users(ui: &AppWindow, browser: &Shared) {
    load(
        ui,
        browser,
        |client| async move { client.get_users().await },
        |ui, browser, users| {
            let me = browser.borrow().me.as_ref().map(|me| me.username.clone());
            let rows: Vec<UserRow> = users
                .user
                .iter()
                .map(|user| {
                    let email = user.email.as_deref().unwrap_or_default();
                    let details: Vec<String> = [email.to_owned(), role_text(user)]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect();
                    UserRow {
                        name: user.username.as_str().into(),
                        details: details.join(" · ").into(),
                        me: me.as_deref() == Some(user.username.as_str()),
                    }
                })
                .collect();
            ui.set_user_rows(ModelRc::new(VecModel::from(rows)));
            browser.borrow_mut().users = users.user;
        },
    );
}

/// Has the server change its users with `task`, then shows them anew with
/// `done` as a notice, or says why it didn't.
fn change_users<F: Future<Output = opensubsonic::Result<()>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    done: String,
    task: impl FnOnce(Rc<Client>) -> F,
) {
    change(ui, browser, done, task, |ui, browser| {
        if ui.get_page() == Page::Users {
            users(ui, browser);
        }
        // An admin may just have changed their own roles.
        load_me(ui, browser);
    });
}

/// The same for the radio stations.
fn change_stations<F: Future<Output = opensubsonic::Result<()>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    done: String,
    task: impl FnOnce(Rc<Client>) -> F,
) {
    change(ui, browser, done, task, |ui, browser| {
        if ui.get_page() == Page::Radio {
            stations(ui, browser);
        } else {
            saved_stations(ui, browser);
        }
    });
}

/// Has the server make a change with `task`, then puts up `done` as a
/// notice and runs `then`, or says why it didn't.
fn change<F: Future<Output = opensubsonic::Result<()>> + 'static>(
    ui: &AppWindow,
    browser: &Shared,
    done: String,
    task: impl FnOnce(Rc<Client>) -> F,
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

/// The radio page: the server's internet radio stations, by name, with
/// their home pages and where they stream from.
fn stations(ui: &AppWindow, browser: &Shared) {
    load(
        ui,
        browser,
        |client| async move { client.get_internet_radio_stations().await },
        |ui, browser, list| {
            browser.borrow_mut().server_stations = list.internet_radio_station.clone();
            let rows: Vec<StationRow> = list
                .internet_radio_station
                .into_iter()
                .map(|station| {
                    let homepage = station.home_page_url.unwrap_or_default();
                    let details: Vec<&str> = [homepage.as_str(), station.stream_url.as_str()]
                        .into_iter()
                        .map(bare_address)
                        .filter(|address| !address.is_empty())
                        .collect();
                    StationRow {
                        details: details.join(" · ").into(),
                        id: station.id.into(),
                        name: station.name.into(),
                        stream: station.stream_url.into(),
                        homepage: homepage.into(),
                    }
                })
                .collect();
            ui.set_station_rows(ModelRc::new(VecModel::from(rows)));
        },
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
    AlbumTile {
        title: station.name.trim().into(),
        names: names(
            (!country.is_empty())
                .then(|| unlinked(country.to_owned()))
                .into_iter()
                .collect(),
        ),
        cover: b
            .pictures
            .get(&(station.favicon.clone(), 0))
            .cloned()
            .unwrap_or_default(),
        ..AlbumTile::default()
    }
}

/// The genres' places on the radio page: Top worldwide first, then the
/// genres with the most albums in the library, the rest as they are. A
/// library genre counts when its name, or a part of it split by "/", ";" or
/// ",", is a shelf's title or tag, whatever its case and punctuation.
fn genre_ranks(library: &[Genre]) -> Vec<usize> {
    let key = |name: &str| -> String {
        (name.chars())
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let mut albums: HashMap<String, i64> = HashMap::new();
    for genre in library {
        for part in genre.value.split(['/', ';', ',']) {
            *albums.entry(key(part)).or_default() += genre.album_count;
        }
    }
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
    let tiles: Vec<AlbumTile> = stations.iter().map(|s| station_tile(&b, s)).collect();
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

/// Fetches the pictures of a shelf's tiles in `rows`, the ones on screen,
/// that haven't been asked for.
fn shelf_logos(browser: &Shared, shelf: usize, rows: Range<usize>) {
    let (wanted, tiles) = {
        let mut b = browser.borrow_mut();
        let shown = b.shelf_model.iter().find(|row| row.shelf as usize == shelf);
        let (Some(row), Some(stations)) = (shown, b.shelves.get(&shelf)) else {
            return;
        };
        let tiles = row.tiles;
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
                tile.cover = image;
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
                let tiles: Vec<_> = stations
                    .iter()
                    .map(|s| {
                        let favicon = Some(s.favicon.clone()).filter(|f| !f.is_empty());
                        (station_tile(&b, s), favicon)
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
    let stream = match station.url_resolved.as_str() {
        "" => station.url.clone(),
        resolved => resolved.to_owned(),
    };
    let saved = browser
        .borrow()
        .server_stations
        .iter()
        .any(|saved| saved.stream_url == stream || saved.stream_url == station.url);
    if saved {
        notify(ui, browser, format!("{name} is saved already."));
        return;
    }
    let homepage = station.homepage.trim().to_owned();
    let done = format!("Saved {name}.");
    change_stations(ui, browser, done, |client| async move {
        let station = self::station(&name, &stream, &homepage);
        client.create_internet_radio_station(&station).await
    });
}

/// The stations saved on the server, kept for Save to know them, and shown
/// on the home page as a shelf, with the directory's pictures of them. A
/// server without radio stations shows none.
fn saved_stations(ui: &AppWindow, browser: &Shared) {
    let Some(client) = browser.borrow().client.clone() else {
        return;
    };
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let Ok(list) = client.get_internet_radio_stations().await else {
            return;
        };
        let Some(ui) = weak.upgrade() else { return };
        let (tiles, wanted) = {
            let mut b = browser.borrow_mut();
            b.server_stations = list.internet_radio_station;
            b.saved = Rc::default();
            let mut wanted = Vec::new();
            for (row, station) in b.server_stations.iter().enumerate() {
                let address = station.home_page_url.as_deref().map(bare_address);
                let known = b
                    .shelves
                    .values()
                    .flatten()
                    .chain(&b.genre_stations)
                    .find(|s| s.url_resolved == station.stream_url || s.url == station.stream_url)
                    .map(|s| Some(s.favicon.clone()))
                    .or_else(|| b.favicons.get(&station.stream_url).cloned());
                let seen = known.clone().flatten().map(|favicon| (favicon, 0));
                let tile = AlbumTile {
                    title: station.name.as_str().into(),
                    names: names(
                        address
                            .filter(|a| !a.is_empty())
                            .map(|a| unlinked(a.to_owned()))
                            .into_iter()
                            .collect(),
                    ),
                    cover: seen
                        .and_then(|key| b.pictures.get(&key).cloned())
                        .unwrap_or_default(),
                    ..AlbumTile::default()
                };
                b.saved.push(tile);
                wanted.push((row, station.name.clone(), station.stream_url.clone(), known));
            }
            ui.set_saved_stations(ModelRc::from(b.saved.clone()));
            (b.saved.clone(), wanted)
        };
        let Some(directory) = directory(&browser).filter(|_| !wanted.is_empty()) else {
            return;
        };
        for (row, name, stream, known) in wanted {
            // The directory's station of the same name that streams from
            // the same address; a lookup that failed is tried again later.
            let favicon = match known {
                Some(favicon) => favicon,
                None => match directory.named(&name).await {
                    Ok(found) => {
                        let same = (found.into_iter())
                            .find(|s| s.url == stream || s.url_resolved == stream);
                        let favicon = same.map(|s| s.favicon);
                        browser
                            .borrow_mut()
                            .favicons
                            .insert(stream, favicon.clone());
                        favicon
                    }
                    Err(_) => None,
                },
            };
            let Some(favicon) = favicon.filter(|f| !f.is_empty()) else {
                continue;
            };
            if let (Some(image), Some(mut tile)) =
                (logo(&browser, favicon).await, tiles.row_data(row))
            {
                tile.cover = image;
                tiles.set_row_data(row, tile);
            }
        }
    })
    .expect("the event loop is running");
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
    load(
        ui,
        browser,
        |client| async move { client.get_song(&id).await },
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
/// asked for again.
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
    load(
        ui,
        browser,
        |client| async move {
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
            let folder = client.get_music_directory(&id).await?;
            Ok((folder.name, folder.child))
        },
        move |ui, browser, (name, entries)| {
            {
                let mut b = browser.borrow_mut();
                b.folder = link;
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
        |client| async move { client.get_tag(&id).await },
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
        |client| async move {
            let composers = if known {
                None
            } else {
                Some(client.get_composers().await?)
            };
            Ok((composers, client.get_compositions(Some(&wanted)).await?))
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
            for piece in &pieces {
                b.tracks.push(composition_row(piece, Some(&id)));
            }
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
        |client| async move { client.get_composition(&id).await },
        |ui, browser, composition| {
            let rows = performances(&composition.song);
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
            let b = browser.borrow();
            for row in rows {
                b.tracks.push(row);
            }
        },
    );
}

/// A row for each album's recordings of a composition, in the order the
/// albums first come: the album and its year, the performers all of them
/// share, when and where they were recorded, and how long they last. Songs
/// the server puts on no album have no row.
// ponytail: one performance per album; split by performers if an album holds two.
fn performances(songs: &[Child]) -> Vec<Track> {
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
            Track {
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
            }
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
            tile.cover = seen.unwrap_or_default();
            tile.selected = b.selected.iter().any(|link| *link == tile.link.as_str());
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
        Page::Tracks | Page::Compositions | Page::Folders | Page::Tag
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
        let (tiles, lines) = (b.tiles.clone(), b.lines.clone());
        let (browser, visit) = (browser.clone(), b.visit);
        slint::spawn_local(async move {
            loop {
                let (row, cover) = {
                    let mut b = browser.borrow_mut();
                    let same = Rc::ptr_eq(&b.tiles, &tiles) && Rc::ptr_eq(&b.lines, &lines);
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
                let Some(image) = cover_art(&browser, visit, cover, size).await else {
                    return;
                };
                let Some(image) = image else { continue };
                if let Some(mut tile) = tiles.row_data(row) {
                    tile.cover = image;
                    tiles.set_row_data(row, tile);
                } else if let Some(mut line) = lines.row_data(row) {
                    line.cover = image;
                    lines.set_row_data(row, line);
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
    if cover.contains("://") {
        let image = logo(browser, cover).await;
        return (browser.borrow().visit == visit).then_some(image);
    }
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

/// Loves or unloves an album's or playlist's track.
fn toggle_love(ui: &AppWindow, browser: &Shared, row: usize) {
    let mut b = browser.borrow_mut();
    let (Some(id), Some(mut track)) = (b.songs.get(row).cloned(), b.tracks.row_data(row)) else {
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

    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let starred = if loved {
            client.star(&[&id], &[], &[]).await
        } else {
            client.unstar(&[&id], &[], &[]).await
        };
        if let (Err(e), Some(ui)) = (starred, weak.upgrade()) {
            undo(&ui, &browser);
            if browser.borrow().visit == visit {
                ui.set_page_problem(format!("Your server didn't take that: {e}").into());
            }
        }
    })
    .expect("the event loop is running");
}

/// `logical` pixels in physical ones, which is what the server should scale to.
fn pixels(ui: &AppWindow, logical: f32) -> u32 {
    (logical * ui.window().scale_factor()).ceil() as u32
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
    fn genres_by_the_librarys_albums() {
        let genre = |value: &str, album_count| Genre {
            value: value.into(),
            song_count: 0,
            album_count,
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
            assert_eq!(role_text(&user), ROLE_NAMES[i]);
            let mut flags = [false; 11];
            flags[i] = true;
            let asked = roles_from(&flags);
            let asked = [
                asked.admin,
                asked.settings,
                asked.stream,
                asked.download,
                asked.upload,
                asked.cover_art,
                asked.comment,
                asked.podcast,
                asked.share,
                asked.jukebox,
                asked.video_conversion,
            ];
            assert_eq!(
                asked.iter().position(|&on| on == Some(true)),
                Some(i),
                "{role}"
            );
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
        let shown: Vec<(String, &str)> = sections(&playlists, Some(&folders), false, true, false)
            .into_iter()
            .skip_while(|(link, ..)| link != "playlists")
            .map(|(link, label, _)| (link, label))
            .collect();
        assert_eq!(
            shown,
            [
                ("playlists".to_owned(), "Playlists"),
                ("playlist:p3".to_owned(), "Zed"),
                ("playlists:f2".to_owned(), "Road trips"),
                ("playlists:f1".to_owned(), "Work"),
                ("playlist:p1".to_owned(), "Late night"),
                ("playlists:f4".to_owned(), "Orphan"),
                ("account".to_owned(), "Account"),
            ]
        );
        let library = sections(&playlists, Some(&folders), false, true, false);
        assert!(library.iter().any(|(link, ..)| link == "tags"));
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
    fn composers_only_where_the_server_has_them() {
        let labels = |compositions| -> Vec<String> {
            sections(&[], None, compositions, false, false)
                .into_iter()
                .map(|(_, label, _)| label.to_owned())
                .collect()
        };
        assert_eq!(
            labels(false),
            [
                "Home",
                "Radio",
                "Albums",
                "Artists",
                "Tracks",
                "Folders",
                "Playlists",
                "Account"
            ]
        );
        assert_eq!(
            labels(true),
            [
                "Home",
                "Radio",
                "Albums",
                "Artists",
                "Tracks",
                "Composers",
                "Compositions",
                "Folders",
                "Playlists",
                "Account"
            ]
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
}
