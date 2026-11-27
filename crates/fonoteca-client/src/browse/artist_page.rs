//! The artist page's three tabs, Overview, About and Releases, and the
//! shelves and grids they show. A tab is part of the page's link
//! (`artist:<id>/about`), so back, forward and a reload keep it, and each tab
//! asks only for what it shows.
//!
//! Band members, the groups an artist was in, and releases that cover their
//! songs are not here: they need data the server lacks. The About tab would
//! take a shelf of them after the biography, the Releases tab one grid each
//! after the collaborations (see issue #118).

use opensubsonic::{ArtistInfo2, ArtistWithAlbumsID3};

use super::*;

const ABOUT_SUFFIX: &str = "/about";
const RELEASES_SUFFIX: &str = "/releases";
/// What the biography's source is called when the server gives its address.
const BIOGRAPHY_SOURCE: &str = "Last.fm";

/// The shelves and grids the page fills, by place in `Shelves`.
const ALBUMS: usize = 0;
const SINGLES: usize = 1;
const COLLABORATIONS: usize = 2;
const COMPILATIONS: usize = 3;
const SIMILAR: usize = 4;
const SHELVES: usize = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum Tab {
    #[default]
    Overview,
    About,
    Releases,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Overview, Tab::About, Tab::Releases];

    pub(super) fn index(self) -> i32 {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0) as i32
    }

    pub(super) fn from_index(index: i32) -> Self {
        usize::try_from(index)
            .ok()
            .and_then(|index| Self::ALL.get(index))
            .copied()
            .unwrap_or_default()
    }

    fn suffix(self) -> &'static str {
        match self {
            Tab::Overview => "",
            Tab::About => ABOUT_SUFFIX,
            Tab::Releases => RELEASES_SUFFIX,
        }
    }
}

/// The artist's id and tab from what follows `artist:` in a link.
pub(super) fn parse(rest: &str) -> (&str, Tab) {
    [Tab::About, Tab::Releases]
        .into_iter()
        .find_map(|tab| rest.strip_suffix(tab.suffix()).map(|id| (id, tab)))
        .unwrap_or((rest, Tab::Overview))
}

pub(super) fn link(id: &str, tab: Tab) -> String {
    format!("artist:{id}{}", tab.suffix())
}

/// An artist's albums told apart for the Releases tab, each in one group,
/// newest first.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Releases<'a> {
    /// Theirs alone, singles and EPs apart.
    pub albums: Vec<&'a AlbumID3>,
    pub singles: Vec<&'a AlbumID3>,
    /// Shared with other artists, or the artist only appears on.
    pub collaborations: Vec<&'a AlbumID3>,
    /// Of any artists, the artist's among them or not.
    pub compilations: Vec<&'a AlbumID3>,
}

impl Releases<'_> {
    fn is_empty(&self) -> bool {
        self.albums.is_empty()
            && self.singles.is_empty()
            && self.collaborations.is_empty()
            && self.compilations.is_empty()
    }
}

pub(super) fn split_releases<'a>(artist: &ArtistID3, albums: &'a [AlbumID3]) -> Releases<'a> {
    let mut releases = Releases::default();
    for album in albums {
        let group = if is_compilation(album) {
            &mut releases.compilations
        } else if !is_by(artist, album) || album.artists.len() > 1 {
            &mut releases.collaborations
        } else if is_single(album) {
            &mut releases.singles
        } else {
            &mut releases.albums
        };
        group.push(album);
    }
    for group in [
        &mut releases.albums,
        &mut releases.singles,
        &mut releases.collaborations,
        &mut releases.compilations,
    ] {
        group.sort_by_key(|album| std::cmp::Reverse(album.year));
    }
    releases
}

fn is_single(album: &AlbumID3) -> bool {
    (album.release_types.iter()).any(|kind| {
        ["single", "ep"]
            .iter()
            .any(|one| kind.eq_ignore_ascii_case(one))
    })
}

/// The artist's banner picture, on servers that have one.
pub(super) fn banner_of(artist: &ArtistID3, offered: bool) -> Option<String> {
    let banner = artist.banner_art.as_ref()?;
    (offered && !banner.is_empty()).then(|| banner.clone())
}

/// Where the biography comes from and its address, for the About tab: only a
/// web page's, since the app opens it.
fn source_of(info: &ArtistInfo2) -> (&'static str, String) {
    let url = info.last_fm_url.as_deref().map(str::trim);
    match url.filter(|url| url.starts_with("https://") || url.starts_with("http://")) {
        Some(url) => (BIOGRAPHY_SOURCE, url.to_owned()),
        None => ("", String::new()),
    }
}

/// The models the page's shelves and grids show.
#[derive(Default)]
pub(super) struct Shelves([Rc<VecModel<AlbumTile>>; SHELVES]);

/// Empties what the last artist page showed.
pub(super) fn clear(ui: &AppWindow, browser: &Shared) {
    browser.borrow_mut().artist_page = Shelves::default();
    for place in 0..SHELVES {
        show_model(ui, place, ModelRc::default());
    }
    ui.set_artist_banner(Image::default());
    ui.set_artist_banner_shown(false);
    ui.set_artist_playable(false);
    ui.set_artist_info_answered(false);
    ui.set_top_tracks_open(false);
}

fn show_model(ui: &AppWindow, place: usize, model: ModelRc<AlbumTile>) {
    match place {
        ALBUMS => ui.set_artist_albums(model),
        SINGLES => ui.set_artist_singles(model),
        COLLABORATIONS => ui.set_artist_collaborations(model),
        COMPILATIONS => ui.set_artist_compilations(model),
        _ => ui.set_similar_artists(model),
    }
}

/// Puts `tiles` in the shelf at `PLACE` with the covers seen so far, and
/// fetches the others.
fn fill<const PLACE: usize>(
    ui: &AppWindow,
    browser: &Shared,
    mut tiles: Vec<(AlbumTile, Option<String>)>,
) {
    let size = tile_pixels(ui);
    let wanted = {
        let mut b = browser.borrow_mut();
        for (tile, _) in &mut tiles {
            tile.downloaded = b.downloads.is_kept(&tile.link);
        }
        let (model, wanted) = seen_covers(&b, tiles, size, false);
        show_model(ui, PLACE, ModelRc::from(model.clone()));
        b.artist_page.0[PLACE] = model;
        wanted
    };
    side_covers(browser, |b| &b.artist_page.0[PLACE], wanted, size, false);
}

fn year_tiles(albums: &[&AlbumID3]) -> Vec<(AlbumTile, Option<String>)> {
    (albums.iter())
        .map(|album| {
            let year = album.year.map(|year| unlinked(year.to_string()));
            album_tile(album, year.into_iter().collect())
        })
        .collect()
}

fn artist_named_tiles(albums: &[&AlbumID3]) -> Vec<(AlbumTile, Option<String>)> {
    (albums.iter())
        .map(|album| album_tile(album, album_names(album)))
        .collect()
}

/// What `tab` shows of the artist's albums, once the server has given them.
pub(super) fn show_albums(
    ui: &AppWindow,
    browser: &Shared,
    artist: &ArtistWithAlbumsID3,
    tab: Tab,
) {
    ui.set_artist_playable(!artist.album.is_empty());
    let releases = split_releases(&artist.artist, &artist.album);
    match tab {
        Tab::Overview => fill::<ALBUMS>(ui, browser, year_tiles(&releases.albums)),
        Tab::About => {}
        Tab::Releases => {
            fill::<ALBUMS>(ui, browser, year_tiles(&releases.albums));
            fill::<SINGLES>(ui, browser, year_tiles(&releases.singles));
            fill::<COLLABORATIONS>(ui, browser, artist_named_tiles(&releases.collaborations));
            fill::<COMPILATIONS>(ui, browser, artist_named_tiles(&releases.compilations));
        }
    }
    ui.set_artist_no_releases(tab == Tab::Releases && releases.is_empty());
}

/// Shows the banner over the page's top once it comes; the page's text is
/// there before it.
pub(super) fn show_banner(ui: &AppWindow, browser: &Shared, banner: Option<String>) {
    let Some(banner) = banner else { return };
    ui.set_artist_banner_shown(true);
    let width = ui
        .window()
        .size()
        .to_logical(ui.window().scale_factor())
        .width;
    let size = pixels(ui, width.clamp(100.0, BANNER_SIZE));
    let visit = browser.borrow().visit;
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        if let Some(Some(image)) = cover_art(&browser, Some(visit), banner, size).await
            && let Some(ui) = weak.upgrade()
        {
            ui.set_artist_banner(image);
        }
    })
    .expect("the event loop is running");
}

/// Asks what the server says about the artist, which the Overview and About
/// tabs show.
pub(super) fn ask_info(ui: &AppWindow, browser: &Shared, id: String, tab: Tab) {
    aside(
        ui,
        browser,
        move |client| {
            let id = id.clone();
            async move { client.get_artist_info2(&id, None, None).await }
        },
        move |ui, browser, info| {
            show_about(ui, info.biography.as_deref());
            let (source, url) = source_of(&info);
            ui.set_about_source(source.into());
            ui.set_about_url(url.into());
            ui.set_artist_info_answered(true);
            if tab == Tab::Overview {
                show_similar(ui, browser, &info.similar_artist);
            }
        },
    );
}

/// The Similar artists shelf: round tiles, each with the picture the server
/// gives or else the one of the library's artist of that id, linking where
/// the artist has a page.
fn show_similar(ui: &AppWindow, browser: &Shared, similar: &[ArtistID3]) {
    let tiles = {
        let b = browser.borrow();
        let pictures = artist_pictures(&b);
        (similar.iter())
            .map(|artist| {
                let (mut tile, cover) = artist_tile(artist);
                if artist.id.is_empty() {
                    tile.link = "".into();
                }
                let cover = cover.filter(|cover| !cover.is_empty()).or_else(|| {
                    pictures
                        .get(artist.id.as_str())
                        .map(|cover| (*cover).clone())
                });
                (tile, cover)
            })
            .collect()
    };
    fill::<SIMILAR>(ui, browser, tiles);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(json: &str) -> AlbumID3 {
        serde_json::from_str(&format!(
            r#"{{"name":"A","songCount":1,"duration":1,"created":"2020-01-01",{json}}}"#
        ))
        .unwrap()
    }

    fn artist() -> ArtistID3 {
        serde_json::from_str(r#"{"id":"r1","name":"Mara"}"#).unwrap()
    }

    fn ids(albums: &[&AlbumID3]) -> Vec<String> {
        albums.iter().map(|album| album.id.clone()).collect()
    }

    #[test]
    fn a_tab_is_part_of_the_link_and_comes_back_from_it() {
        for tab in Tab::ALL {
            let link = link("r1", tab);
            assert_eq!(parse(link.strip_prefix("artist:").unwrap()), ("r1", tab));
        }
        assert_eq!(link("r1", Tab::About), "artist:r1/about");
        assert_eq!(link("r1", Tab::Overview), "artist:r1");
        assert_eq!(parse("odd/id"), ("odd/id", Tab::Overview));
        assert_eq!(Tab::from_index(Tab::Releases.index()), Tab::Releases);
        assert_eq!(Tab::from_index(-1), Tab::Overview);
        assert_eq!(Tab::from_index(9), Tab::Overview);
    }

    #[test]
    fn releases_go_to_one_group_each_newest_first() {
        let albums = [
            album(r#""id":"old","artistId":"r1","year":1999"#),
            album(r#""id":"new","artists":[{"id":"r1","name":"Mara"}],"year":2020"#),
            album(r#""id":"live","artistId":"r1","year":2005,"releaseTypes":["Album","Live"]"#),
            album(r#""id":"one","artistId":"r1","year":2010,"releaseTypes":["Single"]"#),
            album(r#""id":"ep","artistId":"r1","year":2011,"releaseTypes":["EP"]"#),
            album(
                r#""id":"duo","year":2015,"artists":[{"id":"r1","name":"Mara"},{"id":"r2","name":"Ove"}]"#,
            ),
            album(r#""id":"guest","artistId":"r2","year":2012"#),
            album(r#""id":"best","artistId":"r1","isCompilation":true,"year":2018"#),
            album(r#""id":"mix","artistId":"r2","releaseTypes":["compilation"]"#),
            album(r#""id":"undated","artistId":"r1""#),
        ];
        let releases = split_releases(&artist(), &albums);
        assert_eq!(ids(&releases.albums), ["new", "live", "old", "undated"]);
        assert_eq!(ids(&releases.singles), ["ep", "one"]);
        assert_eq!(ids(&releases.collaborations), ["duo", "guest"]);
        assert_eq!(ids(&releases.compilations), ["best", "mix"]);
        assert!(!releases.is_empty());
        assert!(split_releases(&artist(), &[]).is_empty());
    }

    #[test]
    fn a_banner_is_shown_only_where_the_server_gives_banners_and_one_is_there() {
        let with = |json: &str| -> ArtistID3 { serde_json::from_str(json).unwrap() };
        let banner = with(r#"{"id":"r1","name":"M","bannerArt":"ban-1"}"#);
        assert_eq!(banner_of(&banner, true).as_deref(), Some("ban-1"));
        assert_eq!(banner_of(&banner, false), None);
        assert_eq!(banner_of(&artist(), true), None);
        let empty = with(r#"{"id":"r1","name":"M","bannerArt":""}"#);
        assert_eq!(banner_of(&empty, true), None);
    }

    #[test]
    fn the_biography_source_is_named_only_with_a_web_page() {
        let info = |url: &str| -> ArtistInfo2 {
            serde_json::from_str(&format!(r#"{{"lastFmUrl":"{url}"}}"#)).unwrap()
        };
        assert_eq!(
            source_of(&info("https://last.fm/music/Mara")),
            ("Last.fm", "https://last.fm/music/Mara".to_owned())
        );
        assert_eq!(source_of(&info("javascript:alert(1)")).0, "");
        assert_eq!(source_of(&info("")).0, "");
    }
}
