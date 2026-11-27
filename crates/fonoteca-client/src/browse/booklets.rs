//! The album page's Booklets shelf and the viewer that turns a booklet's
//! pages. The server draws the pages, so no client needs a PDF renderer.

use std::collections::{HashMap, VecDeque};

use opensubsonic::{Booklet, BookletKind, Error, FONOTECA_BOOKLETS};

use super::*;

/// How many full-size pages are kept decoded, the one on screen and its
/// neighbours: a page at window size comes to several megabytes.
const PAGES_KEPT: usize = 6;
/// Pages are asked for in steps of this many pixels, so that a window
/// resized a little doesn't ask for them all again.
const PAGE_PIXELS_STEP: u32 = 200;
const SMALLEST_PAGE_PIXELS: u32 = 600;
/// The most the server draws.
const LARGEST_PAGE_PIXELS: u32 = 4000;
/// What a tile's link starts with, then the booklet's place on the shelf.
const LINK_PREFIX: &str = "booklet:";
/// The error code the server gives a page it could not draw.
const COULD_NOT_DRAW_CODE: i64 = 0;
const COULD_NOT_DRAW: &str = "The server could not draw this page";
const COULD_NOT_LOAD: &str = "This page could not be loaded.";

/// The album on screen's booklets, and the one being read.
#[derive(Default)]
pub(super) struct Booklets {
    items: Vec<Booklet>,
    tiles: Rc<VecModel<AlbumTile>>,
    /// First pages for the tiles, by booklet and size.
    thumbnails: HashMap<(String, u32), Image>,
    pages: PageCache,
    viewing: Option<Viewing>,
    /// The page each booklet was left on, kept for the session.
    left_on: HashMap<String, u32>,
    /// Counts the pages asked for, so that the answer to one that is no
    /// longer the page on screen is dropped.
    asked: u64,
}

struct Viewing {
    item: usize,
    page: u32,
}

/// The pages last seen at full size, the oldest let go first.
#[derive(Default)]
struct PageCache(VecDeque<((String, u32, u32), Image)>);

impl PageCache {
    fn get(&self, key: &(String, u32, u32)) -> Option<Image> {
        self.0
            .iter()
            .find(|(kept, _)| kept == key)
            .map(|(_, image)| image.clone())
    }

    fn put(&mut self, key: (String, u32, u32), image: Image) {
        self.0.retain(|(kept, _)| *kept != key);
        self.0.push_back((key, image));
        while self.0.len() > PAGES_KEPT {
            self.0.pop_front();
        }
    }
}

fn kind_label(kind: BookletKind) -> &'static str {
    match kind {
        BookletKind::Booklet => "Booklet",
        BookletKind::Libretto => "Libretto",
        BookletKind::Artwork => "Artwork",
        BookletKind::Other => "File",
    }
}

/// What a tile says under the title: the kind, and the pages of a document.
fn caption(item: &Booklet) -> String {
    let kind = kind_label(item.kind);
    match item.pages {
        Some(1) => format!("{kind} · 1 page"),
        Some(pages) => format!("{kind} · {pages} pages"),
        None => kind.to_owned(),
    }
}

/// How many pages there are to turn: a picture is one.
fn page_count(item: &Booklet) -> u32 {
    item.pages.unwrap_or(1).max(1)
}

/// The page `by` pages on from `page` (counted from 1), stopping at the
/// first and the last of `count`.
fn turned(page: u32, count: u32, by: i32) -> u32 {
    page.saturating_add_signed(by).clamp(1, count.max(1))
}

/// "7 of 32", nothing for a single page.
fn position(page: u32, count: u32) -> String {
    if count > 1 {
        format!("{page} of {count}")
    } else {
        String::new()
    }
}

fn link(index: usize) -> String {
    format!("{LINK_PREFIX}{index}")
}

fn index_of(link: &str) -> Option<usize> {
    link.strip_prefix(LINK_PREFIX)?.parse().ok()
}

/// The size to ask pages for so that they fill a window whose longest side
/// is `logical` pixels at `scale`, stepped and kept to what the server draws.
fn page_pixels(logical: f32, scale: f32) -> u32 {
    let wanted = (logical * scale).ceil() as u32;
    (wanted.div_ceil(PAGE_PIXELS_STEP) * PAGE_PIXELS_STEP)
        .clamp(SMALLEST_PAGE_PIXELS, LARGEST_PAGE_PIXELS)
}

/// What to tell of a page that did not come.
fn notice_for(error: &Error) -> &'static str {
    match error {
        Error::Api(api) if api.code == COULD_NOT_DRAW_CODE => COULD_NOT_DRAW,
        _ => COULD_NOT_LOAD,
    }
}

fn window_pixels(ui: &AppWindow) -> u32 {
    let window = ui.window();
    let size = window.size().to_logical(window.scale_factor());
    page_pixels(size.width.max(size.height), window.scale_factor())
}

/// Asks for the booklets of the album on screen where the server has them;
/// the shelf is drawn once they arrive, and not at all for none.
pub(super) fn ask(ui: &AppWindow, browser: &Shared, album_id: &str) {
    let offered =
        (browser.borrow().client.as_ref()).is_some_and(|client| client.supports(FONOTECA_BOOKLETS));
    if is_offline(browser) || !offered {
        return;
    }
    let album_id = album_id.to_owned();
    aside(
        ui,
        browser,
        move |client| {
            let album_id = album_id.clone();
            async move { client.get_album_booklets(&album_id).await }
        },
        |ui, browser, booklets| shelve(ui, browser, booklets.item),
    );
}

pub(super) fn clear(ui: &AppWindow, browser: &Shared) {
    {
        let mut b = browser.borrow_mut();
        let kept = std::mem::take(&mut b.booklets.left_on);
        b.booklets = Booklets {
            left_on: kept,
            ..Booklets::default()
        };
    }
    ui.set_booklet_open(false);
    ui.set_booklets(ModelRc::default());
}

fn shelve(ui: &AppWindow, browser: &Shared, items: Vec<Booklet>) {
    let tiles: Vec<AlbumTile> = items
        .iter()
        .enumerate()
        .map(|(index, item)| AlbumTile {
            title: item.name.as_str().into(),
            names: names(vec![Name {
                label: caption(item).into(),
                link: "".into(),
            }]),
            link: link(index).into(),
            ..AlbumTile::default()
        })
        .collect();
    let pixels = tile_pixels(ui);
    let wanted: Vec<(usize, String)> = items
        .iter()
        .enumerate()
        .map(|(index, item)| (index, item.id.clone()))
        .collect();
    let model = Rc::new(VecModel::from(tiles));
    {
        let mut b = browser.borrow_mut();
        b.booklets.items = items;
        b.booklets.tiles = model.clone();
    }
    ui.set_booklets(ModelRc::from(model));
    first_pages(browser, wanted, pixels);
}

/// Puts each booklet's first page on its tile, one after the other.
fn first_pages(browser: &Shared, wanted: Vec<(usize, String)>, pixels: u32) {
    let (browser, visit) = (browser.clone(), browser.borrow().visit);
    let tiles = browser.borrow().booklets.tiles.clone();
    slint::spawn_local(async move {
        for (row, id) in wanted {
            let current = {
                let b = browser.borrow();
                b.visit == visit && Rc::ptr_eq(&b.booklets.tiles, &tiles)
            };
            if !current {
                return;
            }
            let Ok(image) = thumbnail(&browser, &id, pixels).await else {
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

async fn thumbnail(browser: &Shared, id: &str, pixels: u32) -> Result<Image, Error> {
    let key = (id.to_owned(), pixels);
    if let Some(image) = browser.borrow().booklets.thumbnails.get(&key) {
        return Ok(image.clone());
    }
    let image = fetch(browser, id, 1, pixels).await?;
    browser
        .borrow_mut()
        .booklets
        .thumbnails
        .insert(key, image.clone());
    Ok(image)
}

async fn fetch(browser: &Shared, id: &str, page: u32, pixels: u32) -> Result<Image, Error> {
    let client = browser.borrow().client.clone().ok_or(Error::NotSignedIn)?;
    let bytes = client.get_booklet_page(id, page, Some(pixels)).await?;
    Image::load_from_data(&bytes, None)
        .map_err(|_| Error::Unexpected("a booklet page that is not a picture".into()))
}

/// The booklet's page at the window's size, from the pages kept or the
/// server.
async fn page_image(browser: &Shared, id: &str, page: u32, pixels: u32) -> Result<Image, Error> {
    let key = (id.to_owned(), page, pixels);
    if let Some(image) = browser.borrow().booklets.pages.get(&key) {
        return Ok(image.clone());
    }
    let image = fetch(browser, id, page, pixels).await?;
    browser.borrow_mut().booklets.pages.put(key, image.clone());
    Ok(image)
}

fn open(ui: &AppWindow, browser: &Shared, link: &str) {
    let mut b = browser.borrow_mut();
    let Some(item) = index_of(link).filter(|&item| item < b.booklets.items.len()) else {
        return;
    };
    let booklet = &b.booklets.items[item];
    let page = (b.booklets.left_on.get(&booklet.id))
        .map_or(1, |&page| turned(page, page_count(booklet), 0));
    ui.set_booklet_title(booklet.name.as_str().into());
    b.booklets.viewing = Some(Viewing { item, page });
    drop(b);
    ui.set_booklet_page(Image::default());
    ui.set_booklet_open(true);
    show_page(ui, browser);
}

fn turn(ui: &AppWindow, browser: &Shared, by: i32) {
    {
        let mut b = browser.borrow_mut();
        let booklets = &mut b.booklets;
        let Some(viewing) = &mut booklets.viewing else {
            return;
        };
        let count = page_count(&booklets.items[viewing.item]);
        let page = turned(viewing.page, count, by);
        if page == viewing.page {
            return;
        }
        viewing.page = page;
    }
    show_page(ui, browser);
}

fn close(ui: &AppWindow, browser: &Shared) {
    let mut b = browser.borrow_mut();
    if let Some(viewing) = b.booklets.viewing.take() {
        let id = b.booklets.items[viewing.item].id.clone();
        b.booklets.left_on.insert(id, viewing.page);
    }
    b.booklets.asked += 1;
    drop(b);
    ui.set_booklet_open(false);
}

/// Shows the page the viewer is on: its counter at once, its picture when
/// it comes.
fn show_page(ui: &AppWindow, browser: &Shared) {
    let (id, page, count, ticket) = {
        let mut b = browser.borrow_mut();
        let booklets = &mut b.booklets;
        let Some(viewing) = &booklets.viewing else {
            return;
        };
        let item = &booklets.items[viewing.item];
        let (id, page, count) = (item.id.clone(), viewing.page, page_count(item));
        booklets.asked += 1;
        (id, page, count, booklets.asked)
    };
    ui.set_booklet_position(position(page, count).into());
    ui.set_booklet_can_go_back(page > 1);
    ui.set_booklet_can_go_on(page < count);
    ui.set_booklet_notice("".into());
    ui.set_booklet_loading(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    let pixels = window_pixels(ui);
    slint::spawn_local(async move {
        let result = page_image(&browser, &id, page, pixels).await;
        let Some(ui) = weak.upgrade() else { return };
        if browser.borrow().booklets.asked != ticket {
            return;
        }
        ui.set_booklet_loading(false);
        match result {
            Ok(image) => ui.set_booklet_page(image),
            Err(error) => {
                ui.set_booklet_page(Image::default());
                ui.set_booklet_notice(notice_for(&error).into());
            }
        }
    })
    .expect("the event loop is running");
}

pub(super) fn wire(ui: &AppWindow, browser: &Shared) {
    ui.on_open_booklet({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |link| open(&weak.unwrap(), &browser, &link)
    });
    ui.on_turn_booklet({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |by| turn(&weak.unwrap(), &browser, by)
    });
    ui.on_retry_booklet({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || show_page(&weak.unwrap(), &browser)
    });
    ui.on_close_booklet({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || close(&weak.unwrap(), &browser)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn booklet(kind: &str, pages: Option<u32>) -> Booklet {
        serde_json::from_value(json!({
            "id": "bk-1", "name": "N", "kind": kind, "contentType": "application/pdf",
            "size": 1, "pages": pages,
        }))
        .unwrap()
    }

    #[test]
    fn a_tile_names_the_kind_and_counts_the_pages_of_a_document() {
        assert_eq!(caption(&booklet("booklet", Some(32))), "Booklet · 32 pages");
        assert_eq!(caption(&booklet("libretto", Some(1))), "Libretto · 1 page");
        assert_eq!(caption(&booklet("artwork", None)), "Artwork");
        assert_eq!(caption(&booklet("sheet", None)), "File");
    }

    #[test]
    fn pages_stop_at_the_first_and_the_last() {
        assert_eq!(turned(1, 32, -1), 1);
        assert_eq!(turned(1, 32, 1), 2);
        assert_eq!(turned(32, 32, 1), 32);
        assert_eq!(turned(7, 32, -1), 6);
        assert_eq!(turned(5, 1, 1), 1, "a picture has one page");
        assert_eq!(
            turned(40, 32, 0),
            32,
            "a page past the end comes back to it"
        );
    }

    #[test]
    fn a_picture_has_one_page_and_a_document_without_a_count_too() {
        assert_eq!(page_count(&booklet("artwork", None)), 1);
        assert_eq!(page_count(&booklet("booklet", Some(0))), 1);
        assert_eq!(page_count(&booklet("booklet", Some(12))), 12);
    }

    #[test]
    fn the_counter_is_left_out_for_a_single_page() {
        assert_eq!(position(7, 32), "7 of 32");
        assert_eq!(position(1, 1), "");
    }

    #[test]
    fn a_tiles_link_leads_back_to_its_place_on_the_shelf() {
        assert_eq!(index_of(&link(3)), Some(3));
        assert_eq!(index_of("album:3"), None);
        assert_eq!(index_of("booklet:x"), None);
    }

    #[test]
    fn pages_are_asked_for_in_steps_within_what_the_server_draws() {
        assert_eq!(page_pixels(760.0, 1.0), 800);
        assert_eq!(page_pixels(800.0, 1.0), 800);
        assert_eq!(page_pixels(300.0, 1.0), SMALLEST_PAGE_PIXELS);
        assert_eq!(page_pixels(2000.0, 2.0), LARGEST_PAGE_PIXELS);
        assert_eq!(page_pixels(640.0, 2.5), 1600);
    }

    #[test]
    fn only_a_page_the_server_could_not_draw_says_so() {
        let api = |code: i64| {
            Error::Api(serde_json::from_value(json!({ "code": code, "message": "m" })).unwrap())
        };
        assert_eq!(notice_for(&api(0)), "The server could not draw this page");
        assert_eq!(notice_for(&api(70)), COULD_NOT_LOAD);
        assert_eq!(notice_for(&Error::Status(500)), COULD_NOT_LOAD);
    }

    #[test]
    fn the_pages_kept_are_the_latest_few() {
        let mut kept = PageCache::default();
        for page in 1..=(PAGES_KEPT as u32 + 2) {
            kept.put(("bk".into(), page, 800), Image::default());
        }
        assert!(kept.get(&("bk".into(), 1, 800)).is_none());
        assert!(kept.get(&("bk".into(), 2, 800)).is_none());
        assert!(kept.get(&("bk".into(), 3, 800)).is_some());
        assert_eq!(kept.0.len(), PAGES_KEPT);
        kept.put(("bk".into(), 3, 800), Image::default());
        assert_eq!(
            kept.0.len(),
            PAGES_KEPT,
            "a page seen again isn't kept twice"
        );
    }
}
