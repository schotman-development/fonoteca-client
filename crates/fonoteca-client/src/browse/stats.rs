//! The home page's lists of what the user played most on ListenBrainz: albums,
//! artists and genres, for a period they pick. They show with a token set.
//!
//! The lists come first, as text. What of them is in the library gets the
//! library's picture and a link to its page once the library has arrived;
//! an album that isn't gets its cover from the cover art archive, which
//! ListenBrainz's statistics point to, and an artist that isn't, none.

use std::collections::HashMap;
use std::rc::Rc;

use listenbrainz::{Range, TopAlbum, TopArtist, TopGenre};
use opensubsonic::{AlbumID3, ArtistID3};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, VecModel};

use super::{Browser, Shared, count, pixels};
use crate::{AppWindow, RankedEntry, RankedList, store};

/// The periods to pick from, with the range of ListenBrainz's each is.
const PERIODS: [(&str, Range); 4] = [
    ("This week", Range::ThisWeek),
    ("This month", Range::ThisMonth),
    ("This year", Range::ThisYear),
    ("All time", Range::AllTime),
];
/// The period shown first.
const FIRST_PERIOD: usize = 2;
/// How many places each list has.
const TOP: usize = 10;
/// The biggest a picture is drawn, in logical pixels.
const PICTURE_SIZE: f32 = 56.0;

/// Where the tests' ListenBrainz and cover art archive answer.
#[cfg(test)]
pub static TEST_ROOTS: std::sync::Mutex<(String, String)> =
    std::sync::Mutex::new((String::new(), String::new()));

#[cfg(not(test))]
fn api_root() -> String {
    listenbrainz::API_ROOT.into()
}

#[cfg(test)]
fn api_root() -> String {
    TEST_ROOTS.lock().unwrap().0.clone()
}

#[cfg(not(test))]
fn cover_art_root() -> String {
    listenbrainz::COVER_ART_ROOT.into()
}

#[cfg(test)]
fn cover_art_root() -> String {
    TEST_ROOTS.lock().unwrap().1.clone()
}

/// One list as ListenBrainz sent it.
enum Fetched<T> {
    Heard(Vec<T>),
    /// ListenBrainz has not worked this period out yet.
    NotCalculated,
    Failed(String),
}

impl<T> Fetched<T> {
    fn new(result: listenbrainz::Result<Option<Vec<T>>>) -> Self {
        match result {
            Ok(Some(places)) => Fetched::Heard(places),
            Ok(None) => Fetched::NotCalculated,
            Err(e) => Fetched::Failed(e.to_string()),
        }
    }

    fn places(&self) -> &[T] {
        match self {
            Fetched::Heard(places) => places,
            _ => &[],
        }
    }

    /// What a list without places says in their stead.
    fn note(&self) -> String {
        match self {
            Fetched::Heard(places) if places.is_empty() => {
                "Nothing was played in this period.".into()
            }
            Fetched::Heard(_) => String::new(),
            Fetched::NotCalculated => "ListenBrainz hasn't worked this period out yet.".into(),
            Fetched::Failed(problem) => problem.clone(),
        }
    }
}

struct Answer {
    albums: Fetched<TopAlbum>,
    artists: Fetched<TopArtist>,
    genres: Fetched<TopGenre>,
}

impl Answer {
    fn is_complete(&self) -> bool {
        !matches!(self.albums, Fetched::Failed(_))
            && !matches!(self.artists, Fetched::Failed(_))
            && !matches!(self.genres, Fetched::Failed(_))
    }
}

pub(super) struct Stats {
    /// The ListenBrainz user the answer is for, and the period picked.
    user: String,
    period: usize,
    /// Counts the asks, so an answer for a period left behind is dropped; and
    /// whether the last ask is still on its way.
    round: u32,
    loading: bool,
    /// Told apart from the one of a Browser that signing out replaced, whose
    /// asks restart at round 1 too.
    session: Rc<()>,
    answer: Option<Answer>,
    client: Option<Rc<listenbrainz::Client>>,
    /// Whether the library and the artists had arrived when the lists on
    /// screen were made, which are made again when that changes, as more of
    /// them can be matched; none when the answer is new.
    drawn: Option<(bool, bool)>,
    /// Counts the times the lists were made, so pictures fetched for lists
    /// since replaced are dropped.
    drawing: u32,
    lists: Rc<VecModel<RankedList>>,
    /// What the lists are matched against, made once the library and the
    /// artists have arrived and dropped when they are asked for anew.
    library_index: Option<LibraryIndex>,
    artist_index: Option<ArtistIndex>,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            user: String::new(),
            period: FIRST_PERIOD,
            round: 0,
            loading: false,
            session: Rc::new(()),
            answer: None,
            client: None,
            drawn: None,
            drawing: 0,
            lists: Rc::default(),
            library_index: None,
            artist_index: None,
        }
    }
}

impl Stats {
    /// Forgets the indexes of the library and the artists, which are asked
    /// for anew or have changed, and matches the lists against them anew
    /// when next shown.
    pub(super) fn forget_indexes(&mut self) {
        self.library_index = None;
        self.artist_index = None;
        self.drawn = None;
    }
}

pub(super) fn wire(ui: &AppWindow, browser: &Shared) {
    let periods: Vec<SharedString> = PERIODS.iter().map(|(name, _)| (*name).into()).collect();
    ui.set_stat_periods(ModelRc::new(VecModel::from(periods)));
    ui.on_pick_stat_period({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| pick(&weak.unwrap(), &browser, index as usize)
    });
}

/// Asks for the lists on a visit to the home page, unless they are here or
/// on their way. A list that failed is asked for again.
pub(super) fn ask(ui: &AppWindow, browser: &Shared) {
    let user = ui.get_listenbrainz_user();
    let is_current = {
        let b = browser.borrow();
        // What the window shows may be of a Browser signing out replaced.
        ui.set_stat_loading(b.stats.loading);
        ui.set_stat_period(b.stats.period as i32);
        if user.is_empty() || b.stats.loading {
            return;
        }
        b.stats.user == user.as_str() && b.stats.answer.as_ref().is_some_and(Answer::is_complete)
    };
    if !is_current {
        fetch(ui, browser);
    }
}

fn pick(ui: &AppWindow, browser: &Shared, index: usize) {
    if index >= PERIODS.len() || index == browser.borrow().stats.period {
        return;
    }
    browser.borrow_mut().stats.period = index;
    ui.set_stat_period(index as i32);
    fetch(ui, browser);
}

/// Asks ListenBrainz for the three lists of the period picked. The lists
/// shown stay until all three are here.
fn fetch(ui: &AppWindow, browser: &Shared) {
    let (round, session, range, user) = {
        let mut b = browser.borrow_mut();
        b.stats.round += 1;
        b.stats.loading = true;
        // Another account's lists are not shown while its own are on the way.
        if b.stats.user != ui.get_listenbrainz_user().as_str() {
            b.stats.lists.set_vec(Vec::new());
            b.stats.answer = None;
        }
        ui.set_stat_lists(ModelRc::from(b.stats.lists.clone()));
        b.stats.user = ui.get_listenbrainz_user().into();
        (
            b.stats.round,
            b.stats.session.clone(),
            PERIODS[b.stats.period].1,
            b.stats.user.clone(),
        )
    };
    ui.set_stat_loading(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let client = client().await.map(Rc::new);
        let answer = match &client {
            Some(client) => Some(answer(client, &user, range).await),
            None => None,
        };
        let Some(ui) = weak.upgrade() else { return };
        {
            let mut b = browser.borrow_mut();
            // Another ask has taken over, or the Browser is another one.
            if b.stats.round != round || !Rc::ptr_eq(&b.stats.session, &session) {
                return;
            }
            b.stats.loading = false;
            b.stats.answer = answer;
            b.stats.client = client;
            b.stats.drawn = None;
        }
        ui.set_stat_loading(false);
        show(&ui, &browser);
    })
    .expect("the event loop is running");
}

/// The client for the saved token. ListenBrainz asks for one call a second
/// or so; the three lists go one after the other.
async fn client() -> Option<listenbrainz::Client> {
    let token = store::load_listenbrainz().await?.token;
    listenbrainz::Client::with_root(token, &api_root())
        .inspect_err(|e| eprintln!("ListenBrainz: no client: {e}"))
        .ok()
}

async fn answer(client: &listenbrainz::Client, user: &str, range: Range) -> Answer {
    Answer {
        albums: Fetched::new(client.top_albums(user, range, TOP as u32).await),
        artists: Fetched::new(client.top_artists(user, range, TOP as u32).await),
        genres: Fetched::new(client.top_genres(user, range, TOP).await),
    }
}

/// Shows the lists, with what of them the library can name.
pub(super) fn show(ui: &AppWindow, browser: &Shared) {
    let size = pixels(ui, PICTURE_SIZE);
    let (models, wanted, drawing) = {
        let mut b = browser.borrow_mut();
        let Browser {
            stats,
            library,
            library_loaded,
            artists,
            artists_loaded,
            pictures,
            ..
        } = &mut *b;
        let Some(answer) = &stats.answer else { return };
        let state = (*library_loaded, *artists_loaded);
        if stats.drawn == Some(state) {
            return;
        }
        stats.drawn = Some(state);
        stats.drawing += 1;
        if *library_loaded && stats.library_index.is_none() {
            stats.library_index = Some(LibraryIndex::new(library));
        }
        if *artists_loaded && stats.artist_index.is_none() {
            stats.artist_index = Some(ArtistIndex::new(artists));
        }
        let index = stats.library_index.as_ref().filter(|_| *library_loaded);
        let library = index.map(|index| (library.as_slice(), index));
        let artists = (stats.artist_index.as_ref().filter(|_| *artists_loaded))
            .map(|index| (artists.as_slice(), index));
        let drawn = [
            draw(
                "Top albums",
                &answer.albums.note(),
                album_entries(answer.albums.places(), library),
                Look::Cover,
                pictures,
                size,
            ),
            draw(
                "Top artists",
                &answer.artists.note(),
                artist_entries(answer.artists.places(), artists),
                Look::Photo,
                pictures,
                size,
            ),
            draw(
                "Top genres",
                &answer.genres.note(),
                genre_entries(answer.genres.places(), index),
                Look::Text,
                pictures,
                size,
            ),
        ];
        let (lists, rest): (Vec<_>, Vec<_>) = drawn
            .into_iter()
            .map(|(list, model, wanted)| (list, (model, wanted)))
            .unzip();
        stats.lists.set_vec(lists);
        ui.set_stat_lists(ModelRc::from(stats.lists.clone()));
        let (models, wanted): (Vec<_>, Vec<_>) = rest.into_iter().unzip();
        (models, wanted, stats.drawing)
    };
    for (model, wanted) in models.into_iter().zip(wanted) {
        fetch_pictures(browser, model, wanted, size, drawing);
    }
}

/// How a list's places look.
#[derive(Clone, Copy)]
enum Look {
    Cover,
    Photo,
    Text,
}

/// A list, the model of its places for pictures to arrive in, and the
/// pictures it wants by place.
fn draw(
    title: &str,
    note: &str,
    entries: Vec<Entry>,
    look: Look,
    pictures: &HashMap<(String, u32), Image>,
    size: u32,
) -> (RankedList, Rc<VecModel<RankedEntry>>, Vec<(usize, Source)>) {
    let mut wanted = Vec::new();
    let mut places = Vec::new();
    for (row, entry) in entries.into_iter().enumerate() {
        let seen = entry
            .source
            .as_ref()
            .and_then(|source| pictures.get(&source.key(size)).cloned());
        if let (None, Some(source)) = (&seen, entry.source) {
            wanted.push((row, source));
        }
        places.push(RankedEntry {
            rank: row as i32 + 1,
            title: entry.title.into(),
            detail: entry.detail.into(),
            plays: count(entry.plays as usize, "play", "plays").into(),
            picture: seen.unwrap_or_default(),
            link: entry.link.into(),
        });
    }
    let model = Rc::new(VecModel::from(places));
    let list = RankedList {
        title: title.into(),
        entries: ModelRc::from(model.clone()),
        note: note.into(),
        pictured: !matches!(look, Look::Text),
        round: matches!(look, Look::Photo),
    };
    (list, model, wanted)
}

/// Fetches the pictures of one list, one after the other, unless the lists
/// are made again meanwhile.
fn fetch_pictures(
    browser: &Shared,
    model: Rc<VecModel<RankedEntry>>,
    wanted: Vec<(usize, Source)>,
    size: u32,
    drawing: u32,
) {
    if wanted.is_empty() {
        return;
    }
    let browser = browser.clone();
    slint::spawn_local(async move {
        for (row, source) in wanted {
            if browser.borrow().stats.drawing != drawing {
                return;
            }
            let Some(image) = picture(&browser, source, size).await else {
                continue;
            };
            if let Some(mut entry) = model.row_data(row) {
                entry.picture = image;
                model.set_row_data(row, entry);
            }
        }
    })
    .expect("the event loop is running");
}

async fn picture(browser: &Shared, source: Source, size: u32) -> Option<Image> {
    match source {
        Source::Library(cover) => super::cover_art(browser, None, cover, size).await.flatten(),
        Source::Archive { release, image } => archived_cover(browser, &release, image).await,
    }
}

/// An album's cover from the cover art archive, kept for the session. One the
/// archive no longer has, or that doesn't come, stays the placeholder.
async fn archived_cover(browser: &Shared, release: &str, image: u64) -> Option<Image> {
    let key = Source::archive_key(release, image);
    let client = {
        let b = browser.borrow();
        if let Some(cover) = b.pictures.get(&key) {
            return Some(cover.clone());
        }
        b.stats.client.clone()?
    };
    let bytes = client
        .cover_art(&cover_art_root(), release, image)
        .await
        .ok()?;
    let cover = Image::load_from_data(&bytes, None).ok()?;
    browser.borrow_mut().pictures.insert(key, cover.clone());
    Some(cover)
}

/// Where the picture of a place comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// The library's cover or picture, by its id.
    Library(String),
    /// The cover art archive's image of a release.
    Archive { release: String, image: u64 },
}

impl Source {
    fn archive_key(release: &str, image: u64) -> (String, u32) {
        (format!("archive:{release}/{image}"), 0)
    }

    /// Where `Browser::pictures` keeps it, at `size`.
    fn key(&self, size: u32) -> (String, u32) {
        match self {
            Source::Library(cover) => (cover.clone(), size),
            Source::Archive { release, image } => Source::archive_key(release, *image),
        }
    }
}

/// A place in a list: what it shows, the page it opens, if the library has
/// it, and where its picture is from.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    title: String,
    detail: String,
    plays: u64,
    link: String,
    source: Option<Source>,
}

fn album_entries(top: &[TopAlbum], library: Option<(&[AlbumID3], &LibraryIndex)>) -> Vec<Entry> {
    top.iter()
        .map(|top| {
            let found = library.and_then(|(albums, index)| Some(&albums[index.find(top)?]));
            let source = match (found, library) {
                (Some(album), _) => album.cover_art.clone().map(Source::Library),
                // Only once the library is here is an album known to be
                // missing from it.
                (None, Some(_)) => archived(top),
                (None, None) => None,
            };
            Entry {
                title: top.release_group_name.clone(),
                detail: top.artist_name.clone(),
                plays: top.listen_count,
                link: found.map_or_else(String::new, |album| format!("album:{}", album.id)),
                source,
            }
        })
        .collect()
}

fn archived(top: &TopAlbum) -> Option<Source> {
    Some(Source::Archive {
        release: top.caa_release_mbid.clone()?,
        image: top.caa_id?,
    })
}

fn artist_entries(top: &[TopArtist], library: Option<(&[ArtistID3], &ArtistIndex)>) -> Vec<Entry> {
    top.iter()
        .map(|top| {
            let found = library.and_then(|(artists, index)| Some(&artists[index.find(top)?]));
            Entry {
                title: top.artist_name.clone(),
                detail: String::new(),
                plays: top.listen_count,
                link: found.map_or_else(String::new, |artist| format!("artist:{}", artist.id)),
                source: found
                    .and_then(|artist| artist.cover_art.clone())
                    .map(Source::Library),
            }
        })
        .collect()
}

/// The genres, each linked to the library's page of the genre of the same
/// name, as the library spells it.
fn genre_entries(top: &[TopGenre], library: Option<&LibraryIndex>) -> Vec<Entry> {
    top.iter()
        .map(|top| {
            let known = folded(&top.genre).and_then(|key| library?.genres.get(&key));
            Entry {
                title: known.map_or_else(|| capitalized(&top.genre), Clone::clone),
                detail: String::new(),
                plays: top.listen_count,
                link: known.map_or_else(String::new, |name| format!("genre:{name}")),
                source: None,
            }
        })
        .collect()
}

fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The library's albums by MusicBrainz ID, and by title with artist, as
/// places in the library's list, and its genres. Of albums that share either
/// ID or names, the first in the library's order is the one. It is made once
/// the library has arrived, and kept until the library is asked for anew.
pub(super) struct LibraryIndex {
    by_mbid: HashMap<String, usize>,
    by_name: HashMap<(String, String), usize>,
    /// The genres by their folded names, as the library spells them.
    genres: HashMap<String, String>,
}

impl LibraryIndex {
    fn new(library: &[AlbumID3]) -> Self {
        let mut index = LibraryIndex {
            by_mbid: HashMap::new(),
            by_name: HashMap::new(),
            genres: HashMap::new(),
        };
        for (place, album) in library.iter().enumerate() {
            if let Some(mbid) = &album.music_brainz_id {
                index.by_mbid.entry(mbid.to_lowercase()).or_insert(place);
            }
            if let Some(title) = folded(&album.name) {
                let artists = [album.artist.as_deref(), album.display_artist.as_deref()];
                for artist in artists.into_iter().flatten().filter_map(folded) {
                    let key = (title.clone(), artist);
                    index.by_name.entry(key).or_insert(place);
                }
            }
            let listed = album.genres.iter().map(|genre| genre.name.as_str());
            for name in listed.chain(album.genre.as_deref()) {
                if let Some(key) = folded(name) {
                    index.genres.entry(key).or_insert_with(|| name.to_owned());
                }
            }
        }
        index
    }

    /// The album of the same release group or release, else of the same
    /// title and artist: titles alone name too many albums.
    fn find(&self, top: &TopAlbum) -> Option<usize> {
        let ids = [&top.release_group_mbid, &top.caa_release_mbid];
        let by_id = ids
            .into_iter()
            .flatten()
            .find_map(|mbid| self.by_mbid.get(&mbid.to_lowercase()));
        let name = folded(&top.release_group_name).zip(folded(&top.artist_name));
        by_id.or_else(|| self.by_name.get(&name?)).copied()
    }
}

/// The library's artists by MusicBrainz ID and by name, as places in its list.
pub(super) struct ArtistIndex {
    by_mbid: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
}

impl ArtistIndex {
    fn new(library: &[ArtistID3]) -> Self {
        let mut index = ArtistIndex {
            by_mbid: HashMap::new(),
            by_name: HashMap::new(),
        };
        for (place, artist) in library.iter().enumerate() {
            if let Some(mbid) = &artist.music_brainz_id {
                index.by_mbid.entry(mbid.to_lowercase()).or_insert(place);
            }
            if let Some(name) = folded(&artist.name) {
                index.by_name.entry(name).or_insert(place);
            }
        }
        index
    }

    fn find(&self, top: &TopArtist) -> Option<usize> {
        let by_id =
            (top.artist_mbid.as_ref()).and_then(|mbid| self.by_mbid.get(&mbid.to_lowercase()));
        by_id
            .or_else(|| self.by_name.get(&folded(&top.artist_name)?))
            .copied()
    }
}

/// The plain letter an accented letter is read as.
fn unaccented(letter: char) -> char {
    match letter {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ł' | 'ľ' | 'ĺ' | 'ļ' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'ţ' | 'ť' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

/// A name as it is compared: without case, accents and punctuation, so that
/// "Röyksopp" is "royksopp" and "Hip-Hop" is "hiphop".
fn fold(name: &str) -> String {
    (name.chars().flat_map(char::to_lowercase))
        .map(unaccented)
        .filter(|letter| letter.is_alphanumeric())
        .collect()
}

/// The folded name, unless nothing of it is left: names of punctuation alone
/// ("!!!") would all be the same name.
fn folded(name: &str) -> Option<String> {
    Some(fold(name)).filter(|key| !key.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top_album(name: &str, artist: &str) -> TopAlbum {
        TopAlbum {
            release_group_name: name.into(),
            artist_name: artist.into(),
            release_group_mbid: None,
            caa_id: None,
            caa_release_mbid: None,
            listen_count: 3,
        }
    }

    fn album(id: &str, name: &str, artist: &str) -> AlbumID3 {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "artist": artist, "coverArt": format!("al-{id}"),
            "songCount": 0, "duration": 0, "created": "2020-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    fn with_mbid(mut album: AlbumID3, mbid: &str) -> AlbumID3 {
        album.music_brainz_id = Some(mbid.into());
        album
    }

    fn artist(id: &str, name: &str) -> ArtistID3 {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "coverArt": format!("ar-{id}")
        }))
        .unwrap()
    }

    fn top_artist(name: &str, mbid: Option<&str>) -> TopArtist {
        TopArtist {
            artist_name: name.into(),
            artist_mbid: mbid.map(str::to_owned),
            listen_count: 5,
        }
    }

    fn albums_matched(top: &[TopAlbum], library: &[AlbumID3]) -> Vec<Entry> {
        album_entries(top, Some((library, &LibraryIndex::new(library))))
    }

    fn artists_matched(top: &[TopArtist], library: &[ArtistID3]) -> Vec<Entry> {
        artist_entries(top, Some((library, &ArtistIndex::new(library))))
    }

    fn genres_matched(top: &[TopGenre], library: &[AlbumID3]) -> Vec<Entry> {
        genre_entries(top, Some(&LibraryIndex::new(library)))
    }

    fn links(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.link.as_str()).collect()
    }

    #[test]
    fn names_are_compared_without_case_accents_or_punctuation() {
        assert_eq!(fold("Röyksopp"), fold("ROYKSOPP"));
        assert_eq!(fold("Hip-Hop"), "hiphop");
        assert_eq!(fold("Beyoncé & Jay-Z"), "beyoncejayz");
        assert_eq!(fold("Mötley Crüe"), fold("motley crue"));
        assert_ne!(fold("Cell"), fold("Cells"));
    }

    #[test]
    fn an_album_is_found_by_its_ids_before_its_names() {
        let library = [
            album("a1", "Seeker", "Carbon Based Lifeforms"),
            with_mbid(album("a2", "Other Name", "Someone"), "C08736E7"),
            with_mbid(album("a3", "Third", "Someone"), "7c45fd16"),
        ];
        let mut by_group = top_album("Seeker", "Carbon Based Lifeforms");
        by_group.release_group_mbid = Some("c08736e7".into());
        let mut by_release = top_album("Unrelated", "Nobody");
        by_release.caa_release_mbid = Some("7C45FD16".into());
        let by_name = top_album("seeker", "carbon based lifeforms");
        let entries = albums_matched(&[by_group, by_release, by_name], &library);
        assert_eq!(links(&entries), ["album:a2", "album:a3", "album:a1"]);
        assert_eq!(entries[0].source, Some(Source::Library("al-a2".into())));
    }

    #[test]
    fn an_album_needs_its_artist_too_to_be_found_by_name() {
        let library = [album("a1", "Greatest Hits", "Queen")];
        let entries = albums_matched(
            &[
                top_album("Greatest Hits", "Abba"),
                top_album("Greatest Hits", "QUEEN"),
            ],
            &library,
        );
        assert_eq!(links(&entries), ["", "album:a1"]);
    }

    #[test]
    fn of_two_albums_that_match_the_first_in_the_library_is_the_one() {
        let library = [album("a1", "Live", "Band"), album("a2", "Live", "Band")];
        let entries = albums_matched(&[top_album("Live", "Band")], &library);
        assert_eq!(links(&entries), ["album:a1"]);
    }

    #[test]
    fn an_album_missing_from_the_library_has_the_archives_cover() {
        let mut top = top_album("Away", "Cell");
        top.caa_id = Some(42);
        top.caa_release_mbid = Some("7c45".into());
        let archive = Some(Source::Archive {
            release: "7c45".into(),
            image: 42,
        });
        let library = [album("a1", "Home", "Cell")];
        let missing = albums_matched(std::slice::from_ref(&top), &library);
        assert_eq!(
            (missing[0].link.as_str(), &missing[0].source),
            ("", &archive)
        );
        // Before the library is here nothing is known, so nothing is fetched.
        let unknown = album_entries(std::slice::from_ref(&top), None);
        assert_eq!((unknown[0].link.as_str(), &unknown[0].source), ("", &None));
        // Without cover art ids there is no cover to fetch.
        let bare = albums_matched(&[top_album("Away", "Cell")], &library);
        assert_eq!(bare[0].source, None);
    }

    #[test]
    fn an_artist_is_found_by_id_before_name_and_has_no_picture_otherwise() {
        let mut library = vec![artist("r1", "Röyksopp"), artist("r2", "Different Name")];
        library[1].music_brainz_id = Some("1C70A3FC".into());
        let entries = artists_matched(
            &[
                top_artist("Royksopp", None),
                top_artist("Röyksopp", Some("1c70a3fc")),
                top_artist("Unknown", Some("ffff")),
            ],
            &library,
        );
        assert_eq!(links(&entries), ["artist:r1", "artist:r2", ""]);
        assert_eq!(entries[1].source, Some(Source::Library("ar-r2".into())));
        assert_eq!(entries[2].source, None);
        let unknown = artist_entries(&[top_artist("Röyksopp", None)], None);
        assert_eq!(links(&unknown), [""]);
    }

    #[test]
    fn a_genre_links_to_the_librarys_genre_of_the_same_name() {
        let mut tagged = album("a1", "Seeker", "CBL");
        tagged.genres =
            vec![serde_json::from_value(serde_json::json!({"name": "Trip-Hop"})).unwrap()];
        let mut single = album("a2", "Other", "CBL");
        single.genre = Some("Ambient".into());
        let top = |genre: &str| TopGenre {
            genre: genre.into(),
            listen_count: 9,
        };
        let entries = genres_matched(
            &[top("trip hop"), top("ambient"), top("shoegaze")],
            &[tagged, single],
        );
        assert_eq!(links(&entries), ["genre:Trip-Hop", "genre:Ambient", ""]);
        let titles: Vec<_> = entries.iter().map(|entry| entry.title.as_str()).collect();
        assert_eq!(titles, ["Trip-Hop", "Ambient", "Shoegaze"]);
        assert_eq!(links(&genre_entries(&[top("ambient")], None)), [""]);
    }

    #[test]
    fn asking_for_the_library_anew_forgets_what_was_made_of_it() {
        let mut stats = Stats {
            library_index: Some(LibraryIndex::new(&[album("a1", "Live", "Band")])),
            artist_index: Some(ArtistIndex::new(&[artist("r1", "Band")])),
            drawn: Some((true, true)),
            ..Stats::default()
        };
        stats.forget_indexes();
        assert!(stats.library_index.is_none() && stats.artist_index.is_none());
        assert_eq!(stats.drawn, None, "drawn anew");
    }

    #[test]
    fn names_of_punctuation_alone_match_nothing() {
        let top = |genre: &str| TopGenre {
            genre: genre.into(),
            listen_count: 1,
        };
        let mut punctuated = album("a1", "!!!", "!!!");
        punctuated.genre = Some("???".into());
        let albums = albums_matched(&[top_album("???", "???")], &[punctuated.clone()]);
        assert_eq!(links(&albums), [""]);
        let artists = artists_matched(&[top_artist("???", None)], &[artist("r1", "!!!")]);
        assert_eq!(links(&artists), [""]);
        let genres = genres_matched(&[top("!!!")], &[punctuated]);
        assert_eq!(links(&genres), [""]);
    }

    #[test]
    fn every_accented_letter_is_read_as_its_plain_one() {
        let groups = [
            ("àáâãäåāăą", "a"),
            ("çćĉċč", "c"),
            ("ďđ", "d"),
            ("èéêëēĕėęě", "e"),
            ("ĝğġģ", "g"),
            ("ìíîïĩīĭįı", "i"),
            ("łľĺļ", "l"),
            ("ñńņňŉ", "n"),
            ("òóôõöøōŏő", "o"),
            ("ŕŗř", "r"),
            ("śŝşš", "s"),
            ("ţť", "t"),
            ("ùúûüũūŭůűų", "u"),
            ("ýÿŷ", "y"),
            ("źżž", "z"),
        ];
        for (accented, plain) in groups {
            for letter in accented.chars() {
                assert_eq!(fold(&letter.to_string()), plain, "{letter}");
                // One that capitalises to two letters, as ŉ does, is left out.
                if letter.to_uppercase().count() == 1 {
                    assert_eq!(fold(&letter.to_uppercase().to_string()), plain, "{letter}");
                }
            }
        }
    }

    /// A server that answers each request by its path with a status and
    /// body, and keeps the request lines.
    fn serve(
        answer: fn(&str) -> (u16, &'static str),
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let root = format!("http://{}/1/", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = BufReader::new(stream.unwrap());
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                let mut header = String::new();
                while stream.read_line(&mut header).unwrap_or(0) > 2 {
                    header.clear();
                }
                let target = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                kept.lock().unwrap().push(target.clone());
                let (status, body) = answer(&target);
                let _ = write!(
                    stream.get_mut(),
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (root, seen)
    }

    fn period_answer(
        range: Range,
        answer: fn(&str) -> (u16, &'static str),
    ) -> (Answer, Vec<String>) {
        let (root, seen) = serve(answer);
        let client = listenbrainz::Client::with_root("t", &root).unwrap();
        let answer = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(super::answer(&client, "rob", range));
        let seen = seen.lock().unwrap().clone();
        (answer, seen)
    }

    #[test]
    fn the_three_lists_are_asked_for_the_range_picked() {
        let (answer, seen) = period_answer(Range::ThisMonth, |target| {
            if target.contains("release-groups") {
                (
                    200,
                    r#"{"payload":{"release_groups":[{"artist_name":"Cell","listen_count":2,"release_group_name":"Away"}]}}"#,
                )
            } else if target.contains("artists") {
                (200, r#"{"payload":{"artists":[]}}"#)
            } else {
                (
                    200,
                    r#"{"payload":{"genre_activity":[{"genre":"rock","hour":1,"listen_count":4},{"genre":"rock","hour":2,"listen_count":1}]}}"#,
                )
            }
        });
        assert_eq!(
            seen,
            [
                "/1/stats/user/rob/release-groups?range=this_month&count=10",
                "/1/stats/user/rob/artists?range=this_month&count=10",
                "/1/stats/user/rob/genre-activity?range=this_month",
            ]
        );
        assert_eq!(answer.albums.places()[0].release_group_name, "Away");
        assert_eq!(answer.artists.note(), "Nothing was played in this period.");
        assert_eq!(answer.genres.places()[0].listen_count, 5);
        assert!(answer.is_complete());
    }

    #[test]
    fn a_period_not_worked_out_yet_says_so_and_a_failure_says_why() {
        let (answer, _) = period_answer(Range::ThisWeek, |target| {
            if target.contains("artists") {
                (404, r#"{"error":"User not found"}"#)
            } else {
                (204, "")
            }
        });
        assert!(matches!(answer.albums, Fetched::NotCalculated));
        assert_eq!(
            answer.albums.note(),
            "ListenBrainz hasn't worked this period out yet."
        );
        assert!(answer.albums.places().is_empty());
        assert!(
            answer.artists.note().contains("User not found"),
            "{}",
            answer.artists.note()
        );
        assert!(!answer.is_complete(), "a failure is asked for again");
    }
}
