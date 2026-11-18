//! The Focus of the overview pages: the facets each page can be narrowed
//! by, what is picked on each, and the counts the panel and the phone's
//! sheet show. The picks live as long as the account is signed in; which
//! facets the columns show and whether the panel is open are kept on this
//! device. The counts are made when the panel opens, a pick changes or more
//! of the page's items have arrived, never as the panel is drawn.

use std::borrow::Cow;

use opensubsonic::ItemGenre;

use crate::focus::{
    self as model, Count, Counted, Facet, Focus, decade, descending, facet, linkable, most_played,
    recently_added,
};
use crate::{FocusColumn, FocusState, FocusValue};

use super::*;

/// How many columns the panel shows at first, and how many values of a
/// facet a column lists before View more.
const FIRST_COLUMNS: usize = 6;
const VALUES_IN_COLUMN: usize = 6;

const LOSSLESS_SUFFIXES: [&str; 9] = [
    "flac", "alac", "wav", "aif", "aiff", "ape", "wv", "dsf", "dff",
];
/// What Roon calls hi-res: more than CD's depth or rate.
const HI_RES_DEPTH: i64 = 16;
const HI_RES_RATE: i64 = 48_000;
const PRODUCTION_ROLES: [&str; 3] = ["producer", "engineer", "mixer"];

const ALBUM_FACETS: &[Facet<AlbumID3>] = &[
    linkable("Genres", album_genres),
    descending("Release date", album_decades),
    facet("Artists", album_artists),
    facet("Record labels", album_record_labels),
    facet("Moods", album_moods),
    facet("Release type", album_release_types),
    descending("Rating", album_rating),
    recently_added("Recently added", album_added),
    most_played("Most played", album_plays),
];

const SONG_FACETS: &[Facet<Child>] = &[
    linkable("Genres", song_genres),
    descending("Release date", song_decades),
    linkable("Performers", song_performers),
    facet("Composers", song_composers),
    facet("Production", song_production),
    most_played("Most played", song_plays),
    facet("Moods", song_moods),
    facet("Format", song_formats),
];

const ARTIST_FACETS: &[Facet<ArtistFacts>] = &[
    linkable("Genres", |artist| artist.genres.clone()),
    most_played("Most played", |artist| artist.plays),
];

const COMPOSER_FACETS: &[Facet<Composer>] = &[most_played("Most played", |composer| {
    composer.play_count.unwrap_or(0)
})];

const COMPOSITION_FACETS: &[Facet<Composition>] = &[
    facet("Composers", composition_composers),
    descending("Era", composition_eras),
    facet("Period", |piece| piece.period.iter().cloned().collect()),
    facet("Form", |piece| piece.form.iter().cloned().collect()),
    facet("Key", |piece| piece.key.iter().cloned().collect()),
    most_played("Most played", |piece| piece.play_count.unwrap_or(0)),
];

/// What the Focus keeps: the picks of each page, which last until sign-out,
/// and what the panel last counted.
#[derive(Default)]
pub(super) struct State {
    albums: Focus,
    tracks: Focus,
    artists: Focus,
    composers: Focus,
    compositions: Focus,
    /// Each page's panel as saved on this device, by `key`.
    pages: BTreeMap<String, prefs::FocusPage>,
    /// The page the panel was last shown for, so that another page starts
    /// without a sheet or a list.
    last_page: Option<Page>,
    /// The counts of the page on screen, and the page and number of items
    /// they were made for.
    counts: Vec<Counted>,
    counted: Option<Made>,
    /// The facet whose values are listed whole, by title.
    listing: Option<String>,
}

impl State {
    fn of(&self, page: Page) -> Option<&Focus> {
        match page {
            Page::Albums => Some(&self.albums),
            Page::Tracks => Some(&self.tracks),
            Page::Artists => Some(&self.artists),
            Page::Composers => Some(&self.composers),
            Page::Compositions => Some(&self.compositions),
            _ => None,
        }
    }

    fn of_mut(&mut self, page: Page) -> Option<&mut Focus> {
        match page {
            Page::Albums => Some(&mut self.albums),
            Page::Tracks => Some(&mut self.tracks),
            Page::Artists => Some(&mut self.artists),
            Page::Composers => Some(&mut self.composers),
            Page::Compositions => Some(&mut self.compositions),
            _ => None,
        }
    }
}

/// What an artist is told apart by on the artists page: the genres of their
/// albums and how often those were played.
#[derive(Clone, Default)]
struct ArtistFacts {
    genres: Vec<String>,
    plays: i64,
    starred: bool,
}

/// What the counts were made for, which they hold until it changes: the page,
/// the items it and the artists' albums held, and how many were hearted.
#[derive(PartialEq)]
struct Made {
    page: Page,
    items: usize,
    albums: usize,
    hearted: usize,
}

/// The page's items, with the facets they are told apart by.
trait Surface {
    fn len(&self) -> usize;
    fn hearted(&self) -> usize;
    fn matching(&self, focus: &Focus, today: i64) -> Vec<bool>;
    fn count(&self, focus: &Focus, today: i64) -> Vec<Counted>;
}

struct Source<'a, T: Clone + 'static> {
    items: Cow<'a, [T]>,
    facets: &'static [Facet<T>],
    starred: Option<fn(&T) -> bool>,
}

impl<T: Clone + 'static> Surface for Source<'_, T> {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn hearted(&self) -> usize {
        let starred = self.starred;
        starred.map_or(0, |starred| {
            self.items.iter().filter(|item| starred(item)).count()
        })
    }

    fn matching(&self, focus: &Focus, today: i64) -> Vec<bool> {
        model::matching(&self.items, self.facets, focus, self.starred, today)
    }

    fn count(&self, focus: &Focus, today: i64) -> Vec<Counted> {
        model::count(&self.items, self.facets, focus, self.starred, today)
    }
}

fn surface(b: &Browser, page: Page) -> Option<Box<dyn Surface + '_>> {
    Some(match page {
        Page::Albums => Box::new(Source {
            items: Cow::Borrowed(b.library.as_slice()),
            facets: ALBUM_FACETS,
            starred: Some(|album| album.starred.is_some()),
        }),
        Page::Tracks => Box::new(Source {
            items: Cow::Borrowed(b.song_list.as_slice()),
            facets: SONG_FACETS,
            starred: Some(|song| song.starred.is_some()),
        }),
        Page::Artists => Box::new(Source {
            items: Cow::Owned(artist_facts(b)),
            facets: ARTIST_FACETS,
            starred: Some(|artist| artist.starred),
        }),
        Page::Composers => Box::new(Source {
            items: Cow::Borrowed(b.composers.as_slice()),
            facets: COMPOSER_FACETS,
            starred: None,
        }),
        Page::Compositions => Box::new(Source {
            items: Cow::Borrowed(b.compositions.as_slice()),
            facets: COMPOSITION_FACETS,
            starred: Some(|piece| piece.starred.is_some()),
        }),
        _ => return None,
    })
}

/// The page's name in the saved preferences, for the pages that have a Focus.
fn key(page: Page) -> Option<&'static str> {
    match page {
        Page::Albums => Some("albums"),
        Page::Tracks => Some("tracks"),
        Page::Artists => Some("artists"),
        Page::Composers => Some("composers"),
        Page::Compositions => Some("compositions"),
        _ => None,
    }
}

/// What the page lists, for a value's count: "1 album", "198 albums".
fn nouns(page: Page) -> (&'static str, &'static str) {
    match page {
        Page::Tracks => ("track", "tracks"),
        Page::Artists => ("artist", "artists"),
        Page::Composers => ("composer", "composers"),
        Page::Compositions => ("composition", "compositions"),
        _ => ("album", "albums"),
    }
}

fn today() -> i64 {
    model::today(prefs::now())
}

/// The `view` of the page's items without those the picks and the heart
/// leave out.
pub(super) fn narrow(b: &Browser, page: Page, mut view: Vec<usize>) -> Vec<usize> {
    let wanted = b.focus.of(page).filter(|focus| focus.is_narrowing());
    let Some((focus, items)) = wanted.zip(surface(b, page)) else {
        return view;
    };
    let allowed = items.matching(focus, today());
    view.retain(|&i| allowed[i]);
    view
}

/// Picks up what is saved on this device, on sign-in.
pub(super) fn load_prefs(b: &mut Browser, prefs: &prefs::Prefs) {
    b.focus.pages = prefs.focus.clone();
}

pub(super) fn wire(ui: &AppWindow, browser: &Shared) {
    let state = ui.global::<FocusState>();
    state.on_visibility_changed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |open, sheet| change_visibility(&weak.unwrap(), &browser, open, sheet)
    });
    state.on_picked({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |facet, value| {
            let page = weak.unwrap().get_page();
            if let Some(focus) = browser.borrow_mut().focus.of_mut(page) {
                focus.toggle(&facet, &value);
            }
            repick(&weak.unwrap(), &browser);
        }
    });
    state.on_link_toggled({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |facet| {
            let page = weak.unwrap().get_page();
            if let Some(focus) = browser.borrow_mut().focus.of_mut(page) {
                focus.toggle_all_of(&facet);
            }
            repick(&weak.unwrap(), &browser);
        }
    });
    state.on_chip_dropped({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| {
            let page = weak.unwrap().get_page();
            if let Some(focus) = browser.borrow_mut().focus.of_mut(page) {
                let picked = focus.picked();
                if let Some(&(facet, value)) = picked.get(index as usize) {
                    let (facet, value) = (facet.to_owned(), value.to_owned());
                    focus.toggle(&facet, &value);
                }
            }
            repick(&weak.unwrap(), &browser);
        }
    });
    state.on_favourites_toggled({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let page = weak.unwrap().get_page();
            if let Some(focus) = browser.borrow_mut().focus.of_mut(page) {
                focus.favourites = !focus.favourites;
            }
            repick(&weak.unwrap(), &browser);
        }
    });
    state.on_cleared({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let page = weak.unwrap().get_page();
            if let Some(focus) = browser.borrow_mut().focus.of_mut(page) {
                focus.clear();
            }
            repick(&weak.unwrap(), &browser);
        }
    });
    state.on_facet_chosen({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |column, facet| choose_facet(&weak.unwrap(), &browser, column as usize, &facet)
    });
    state.on_facets_searched({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            show_facet_matches(&ui.global::<FocusState>(), &browser.borrow().focus.counts);
        }
    });
    state.on_listed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |facet| {
            let ui = weak.unwrap();
            browser.borrow_mut().focus.listing = Some(facet.to_string());
            ui.global::<FocusState>().set_query("".into());
            show(&ui, &browser);
        }
    });
    state.on_list_searched({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || show(&weak.unwrap(), &browser)
    });
    state.on_list_closed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            browser.borrow_mut().focus.listing = None;
            ui.global::<FocusState>().set_query("".into());
            show(&ui, &browser);
        }
    });
}

/// The picks changed: the page is shown again with them, and counts anew.
fn repick(ui: &AppWindow, browser: &Shared) {
    browser.borrow_mut().focus.counted = None;
    super::show(ui, browser);
}

/// The panel or the sheet opened or closed. A panel's state is kept; a sheet
/// starts with the first facet's values listed.
fn change_visibility(ui: &AppWindow, browser: &Shared, open: bool, sheet: bool) {
    {
        let mut b = browser.borrow_mut();
        b.focus.listing = None;
        if !sheet && let Some(key) = key(ui.get_page()) {
            b.focus.pages.entry(key.to_owned()).or_default().open = open;
            keep(&b);
        }
    }
    ui.global::<FocusState>().set_query("".into());
    show(ui, browser);
}

/// Has the column take another facet, the one that had it taking the
/// column's.
fn choose_facet(ui: &AppWindow, browser: &Shared, column: usize, facet: &str) {
    let page = ui.get_page();
    {
        let mut b = browser.borrow_mut();
        let Some(key) = key(page) else { return };
        let saved = b.focus.pages.get(key).cloned().unwrap_or_default();
        let mut titles = chosen_titles(&b.focus.counts, &saved.columns);
        let Some(chosen) = (b.focus.counts.iter()).find(|counted| counted.title == facet) else {
            return;
        };
        match titles.iter().position(|title| *title == chosen.title) {
            Some(at) => titles.swap(at, column),
            None => titles[column] = chosen.title,
        }
        b.focus.pages.entry(key.to_owned()).or_default().columns =
            titles.into_iter().map(str::to_owned).collect();
        keep(&b);
    }
    show(ui, browser);
}

fn keep(b: &Browser) {
    prefs::save(&prefs::Prefs {
        focus: b.focus.pages.clone(),
        ..prefs::load()
    });
}

/// Shows the Focus of the page on screen: its picks as chips, and where the
/// panel or the sheet is open, its columns and list, with counts made first
/// if the page's items or picks changed since.
pub(super) fn show(ui: &AppWindow, browser: &Shared) {
    let page = ui.get_page();
    let state = ui.global::<FocusState>();
    let Some(key) = key(page) else {
        return state.set_offered(false);
    };
    let mut b = browser.borrow_mut();
    if b.focus.last_page != Some(page) {
        b.focus.last_page = Some(page);
        b.focus.listing = None;
        state.set_sheet(false);
        state.set_query("".into());
    }
    let saved = b.focus.pages.get(key).cloned().unwrap_or_default();
    let focus = b.focus.of(page).cloned().unwrap_or_default();
    let plural = nouns(page).1;
    // Without the plays of the composers there is nothing to narrow them by.
    state.set_offered(page != Page::Composers || narrows_composers(&b));
    state.set_heart(page != Page::Composers);
    state.set_open(saved.open);
    state.set_favourites(focus.favourites);
    state.set_narrowing(focus.is_narrowing());
    state.set_noun(plural.into());
    let chips = (focus.picked().into_iter())
        .map(|(_, value)| SharedString::from(value))
        .collect::<Vec<_>>();
    state.set_chips(ModelRc::new(VecModel::from(chips)));
    if saved.open || state.get_sheet() {
        count_if_stale(&mut b, page, &focus);
        publish(&state, &b, page, &focus, &saved.columns);
    }
}

/// The composers' only facet is the steps of the most played, which the
/// first step narrows only where there are more composers than it holds.
fn narrows_composers(b: &Browser) -> bool {
    counts_composition_plays(b) && b.composers.len() > model::FIRST_TOP_STEP
}

fn count_if_stale(b: &mut Browser, page: Page, focus: &Focus) {
    let Some(items) = surface(b, page) else {
        return;
    };
    let counted = Some(Made {
        page,
        items: items.len(),
        albums: b.library.len(),
        hearted: items.hearted(),
    });
    if b.focus.counted == counted {
        return;
    }
    let counts = items.count(focus, today());
    drop(items);
    b.focus.counts = counts;
    b.focus.counted = counted;
}

/// Gives the panel the facets on offer, its columns and the list.
fn publish(
    state: &FocusState<'_>,
    b: &Browser,
    page: Page,
    focus: &Focus,
    saved_columns: &[String],
) {
    let counts = &b.focus.counts;
    let nouns = nouns(page);
    let titles = (counts.iter())
        .map(|counted| SharedString::from(counted.title))
        .collect::<Vec<_>>();
    state.set_facets(ModelRc::new(VecModel::from(titles)));
    show_facet_matches(state, counts);
    let column_of = |title: &str| {
        let at = counts.iter().position(|counted| counted.title == title)?;
        Some((at, &counts[at]))
    };
    let columns = (chosen_titles(counts, saved_columns).into_iter())
        .filter_map(column_of)
        .map(|(at, counted)| column(counted, at, focus, nouns, Some(VALUES_IN_COLUMN), None))
        .collect::<Vec<_>>();
    state.set_columns(ModelRc::new(VecModel::from(columns)));
    let listing = (b.focus.listing.as_deref())
        .or_else(|| state.get_sheet().then(|| counts.first().map(|c| c.title))?);
    let list = (listing.and_then(column_of))
        .map(|(at, counted)| {
            let query = state.get_query();
            column(counted, at, focus, nouns, None, Some(query.trim()))
        })
        .unwrap_or_default();
    state.set_list(list);
}

/// The facets the search over them leaves for a column's chooser. Only this
/// is replaced as one types, for the columns, and the chooser open in one,
/// would be made anew with the rest.
fn show_facet_matches(state: &FocusState<'_>, counts: &[Counted]) {
    let titles = counts
        .iter()
        .map(|counted| counted.title)
        .collect::<Vec<_>>();
    let matches = filtered(&titles, state.get_facet_query().trim(), |title| {
        title.to_string()
    })
    .into_iter()
    .map(|at| SharedString::from(titles[at]))
    .collect::<Vec<_>>();
    state.set_facet_matches(ModelRc::new(VecModel::from(matches)));
}

/// The titles of the facets the columns show: those saved that are on offer,
/// else the first ones.
fn chosen_titles(counts: &[Counted], saved: &[String]) -> Vec<&'static str> {
    let kept: Vec<&'static str> = (saved.iter())
        .filter_map(|title| counts.iter().find(|counted| counted.title == title))
        .map(|counted| counted.title)
        .collect();
    if !kept.is_empty() {
        return kept;
    }
    (counts.iter())
        .take(FIRST_COLUMNS)
        .map(|counted| counted.title)
        .collect()
}

/// A facet as a column: its first `limit` values and those picked, or, with
/// a `query`, those that match it.
fn column(
    counted: &Counted,
    at: usize,
    focus: &Focus,
    nouns: (&str, &str),
    limit: Option<usize>,
    query: Option<&str>,
) -> FocusColumn {
    let found = filtered(&counted.values, query.unwrap_or_default(), |value| {
        value.label.clone()
    });
    let listed: Vec<FocusValue> = (found.iter().enumerate())
        .filter(|&(rank, &i)| {
            limit.is_none_or(|limit| rank < limit)
                || focus.is_picked(counted.title, &counted.values[i].label)
        })
        .map(|(_, &i)| value(counted.title, &counted.values[i], focus, nouns))
        .collect();
    FocusColumn {
        title: counted.title.into(),
        facet: at as i32,
        hidden: (found.len() - listed.len()) as i32,
        values: ModelRc::new(VecModel::from(listed)),
        link: counted.all_of_offered,
        all_of: focus.is_all_of(counted.title),
    }
}

/// A value, said as a screen reader should: "Blues, 198 albums, selected".
fn value(facet: &str, count: &Count, focus: &Focus, nouns: (&str, &str)) -> FocusValue {
    let picked = focus.is_picked(facet, &count.label);
    let selected = if picked { ", selected" } else { "" };
    FocusValue {
        label: count.label.as_str().into(),
        count: count.count as i32,
        picked,
        spoken: format!(
            "{}, {}{selected}",
            count.label,
            super::count(count.count, nouns.0, nouns.1)
        )
        .into(),
    }
}

fn labels(names: Vec<Name>) -> Vec<String> {
    names.into_iter().map(|name| name.label.into()).collect()
}

fn genres_or_one(genres: &[ItemGenre], genre: &Option<String>) -> Vec<String> {
    if genres.is_empty() {
        return genre.iter().cloned().collect();
    }
    genres.iter().map(|genre| genre.name.clone()).collect()
}

fn decade_of(year: Option<i64>) -> Vec<String> {
    year.and_then(decade).into_iter().collect()
}

fn album_genres(album: &AlbumID3) -> Vec<String> {
    genres_or_one(&album.genres, &album.genre)
}

fn album_decades(album: &AlbumID3) -> Vec<String> {
    let year_of = |date: &Option<ItemDate>| date.as_ref().and_then(|date| date.year);
    decade_of(
        (album.year)
            .or_else(|| year_of(&album.original_release_date))
            .or_else(|| year_of(&album.release_date)),
    )
}

fn album_artists(album: &AlbumID3) -> Vec<String> {
    labels(album_names(album))
}

fn album_record_labels(album: &AlbumID3) -> Vec<String> {
    (album.record_labels.iter())
        .map(|label| label.name.clone())
        .collect()
}

fn album_moods(album: &AlbumID3) -> Vec<String> {
    album.moods.clone()
}

fn album_release_types(album: &AlbumID3) -> Vec<String> {
    album.release_types.clone()
}

fn album_rating(album: &AlbumID3) -> Vec<String> {
    (album.user_rating.into_iter())
        .filter(|&stars| stars > 0)
        .map(|stars| super::count(stars as usize, "star", "stars"))
        .collect()
}

fn album_added(album: &AlbumID3) -> Option<i64> {
    model::day_number(&album.created)
}

fn album_plays(album: &AlbumID3) -> i64 {
    album.play_count.unwrap_or(0)
}

fn song_genres(song: &Child) -> Vec<String> {
    genres_or_one(&song.genres, &song.genre)
}

fn song_decades(song: &Child) -> Vec<String> {
    decade_of(song.year)
}

fn song_performers(song: &Child) -> Vec<String> {
    labels(song_names(song))
}

fn song_composers(song: &Child) -> Vec<String> {
    labels(composer_names(song))
}

fn song_production(song: &Child) -> Vec<String> {
    (song.contributors.iter())
        .filter(|contributor| PRODUCTION_ROLES.contains(&contributor.role.as_str()))
        .map(|contributor| contributor.artist.name.clone())
        .collect()
}

fn song_plays(song: &Child) -> i64 {
    song.play_count.unwrap_or(0)
}

fn song_moods(song: &Child) -> Vec<String> {
    song.moods.clone()
}

/// Lossless for a file that loses nothing, and hi-res as well for one with
/// more than CD's depth or rate.
fn song_formats(song: &Child) -> Vec<String> {
    let suffix = song.suffix.as_deref().unwrap_or_default().to_lowercase();
    if !LOSSLESS_SUFFIXES.contains(&suffix.as_str()) {
        return Vec::new();
    }
    let mut formats = vec!["Lossless".to_owned()];
    let deep = song.bit_depth.is_some_and(|depth| depth > HI_RES_DEPTH);
    let fast = song.sampling_rate.is_some_and(|rate| rate > HI_RES_RATE);
    if deep || fast {
        formats.push("Hi-res".to_owned());
    }
    formats
}

fn composition_composers(piece: &Composition) -> Vec<String> {
    (piece.composers.iter())
        .map(|composer| composer.name.clone())
        .collect()
}

/// The decade a work was begun in, or else finished in.
fn composition_eras(piece: &Composition) -> Vec<String> {
    let year_of = |date: &Option<ItemDate>| date.as_ref().and_then(|date| date.year);
    let span = piece.composed.as_ref();
    decade_of(span.and_then(|span| year_of(&span.from).or_else(|| year_of(&span.to))))
}

/// The genres of each artist's albums, and how often they were played.
fn artist_facts(b: &Browser) -> Vec<ArtistFacts> {
    let mut by_link: HashMap<String, ArtistFacts> = HashMap::new();
    for album in &b.library {
        let genres = album_genres(album);
        for name in album_names(album) {
            let facts = by_link.entry(name.link.into()).or_default();
            facts.plays += album.play_count.unwrap_or(0);
            for genre in &genres {
                if !facts.genres.contains(genre) {
                    facts.genres.push(genre.clone());
                }
            }
        }
    }
    (b.artists.iter())
        .map(|artist| ArtistFacts {
            starred: artist.starred.is_some(),
            ..by_link
                .remove(&format!("artist:{}", artist.id))
                .unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(id: &str, genre: &str, starred: bool) -> AlbumID3 {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "songCount": 1, "duration": 1,
            "created": "2021-01-01T00:00:00Z", "genre": genre,
            "starred": starred.then_some("2021-01-01T00:00:00Z"),
        }))
        .unwrap()
    }

    fn browser_with_albums() -> Browser {
        Browser {
            library: vec![
                album("a", "Blues", false),
                album("b", "Jazz", true),
                album("c", "Blues", true),
            ],
            ..Browser::default()
        }
    }

    #[test]
    fn a_pick_leaves_the_albums_of_the_view_that_have_it() {
        let mut b = browser_with_albums();
        assert_eq!(narrow(&b, Page::Albums, vec![2, 0, 1]), [2, 0, 1]);
        b.focus.albums.toggle("Genres", "Blues");
        assert_eq!(narrow(&b, Page::Albums, vec![2, 0, 1]), [2, 0]);
        b.focus.albums.favourites = true;
        assert_eq!(narrow(&b, Page::Albums, vec![2, 0, 1]), [2]);
    }

    #[test]
    fn the_picks_of_one_page_do_not_narrow_another() {
        let mut b = browser_with_albums();
        b.focus.albums.toggle("Genres", "Blues");
        assert_eq!(narrow(&b, Page::Genre, vec![0, 1, 2]), [0, 1, 2]);
        assert_eq!(narrow(&b, Page::Tracks, vec![0, 1, 2]), [0, 1, 2]);
    }

    #[test]
    fn the_hearted_count_changes_when_an_album_is_hearted() {
        let mut b = browser_with_albums();
        let before = surface(&b, Page::Albums).unwrap().hearted();
        b.library[0].starred = Some("2021-01-01T00:00:00Z".into());
        assert_eq!(surface(&b, Page::Albums).unwrap().hearted(), before + 1);
    }

    #[test]
    fn a_pages_saved_panel_survives_a_round_trip() {
        let page = prefs::FocusPage {
            open: true,
            columns: vec!["Genres".into(), "Moods".into()],
        };
        let saved = prefs::Prefs {
            focus: BTreeMap::from([("albums".to_owned(), page.clone())]),
            ..Default::default()
        };
        let read: prefs::Prefs =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert_eq!(read.focus["albums"], page);
    }
}
