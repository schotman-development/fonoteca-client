//! The home page's shelf of the albums the account rated highest, best
//! first.

use super::*;

/// The shelf's tiles.
#[derive(Default)]
pub(super) struct TopRated {
    tiles: Rc<VecModel<AlbumTile>>,
}

/// Shows the shelf as it was and asks the server for it again, since an
/// album may have been rated meanwhile.
pub(super) fn ask(ui: &AppWindow, browser: &Shared) {
    ui.set_rated_albums(ModelRc::from(browser.borrow().top_rated.tiles.clone()));
    aside(
        ui,
        browser,
        |client| async move {
            let options = ListOptions {
                size: Some(RECENT as u32),
                ..ListOptions::default()
            };
            let list = client.get_album_list2(&AlbumListType::Highest, &options);
            Ok(list.await?.album)
        },
        shelve,
    );
}

fn shelve(ui: &AppWindow, browser: &Shared, albums: Vec<AlbumID3>) {
    let size = tile_pixels(ui);
    let wanted = {
        let mut b = browser.borrow_mut();
        // A server may list the albums it has no rating of after the rated.
        let rated = albums.iter().filter(|album| album.user_rating.is_some());
        let tiles = rated.map(|album| {
            let (mut tile, cover) = album_tile(album, album_names(album));
            tile.downloaded = b.downloads.is_kept(&tile.link);
            (tile, cover)
        });
        let (model, wanted) = seen_covers(&b, tiles.collect(), size, false);
        b.top_rated.tiles = model;
        ui.set_rated_albums(ModelRc::from(b.top_rated.tiles.clone()));
        wanted
    };
    side_covers(browser, |b| &b.top_rated.tiles, wanted, size, false);
}
