//! The home page's shelves of what was played last: the albums the server
//! lists as played most recently, and their artists, each once, in that
//! order. The server learns of each play from the scrobbler.

use super::*;

/// The albums the server last listed as played most recently, and the
/// shelves' tiles made of them.
#[derive(Default)]
pub(super) struct Played {
    albums: Vec<AlbumID3>,
    album_tiles: Rc<VecModel<AlbumTile>>,
    artist_tiles: Rc<VecModel<AlbumTile>>,
    /// Whether the artists' tiles were made with every artist in, which
    /// gives them their pictures.
    has_artists: bool,
}

/// Shows the shelves as they were, then as this device kept them, and asks
/// the server for them again, since something may have played meanwhile.
/// The page shows no "Loading" for them, as they are there at once from the
/// second visit on.
pub(super) fn ask(ui: &AppWindow, browser: &Shared) {
    let (client, visit) = {
        let b = browser.borrow();
        put(ui, &b.played);
        (b.client.clone(), b.visit)
    };
    let Some(client) = client else { return };
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let on_this_visit = || (weak.upgrade()).filter(|_| browser.borrow().visit == visit);
        let reader = client.reader(true);
        let first = played_last(&reader).await;
        let Some(ui) = on_this_visit() else { return };
        match first {
            Ok(albums) => shelve(&ui, &browser, albums),
            Err(e) => {
                let problem = format!("Last played couldn't be fetched: {e}");
                return ui.set_page_problem(problem.into());
            }
        }
        // An answer kept, even a fresh one, misses the plays since: a play
        // outdates none, or every song would have the albums pages asked
        // for anew.
        if reader.heard_news() {
            return;
        }
        let asker = client.asking_the_server();
        let Ok(albums) = played_last(&asker).await else {
            return;
        };
        let Some(ui) = on_this_visit() else { return };
        if asker.heard_news() {
            shelve(&ui, &browser, albums);
        }
    })
    .expect("the event loop is running");
}

async fn played_last(client: &Client) -> opensubsonic::Result<Vec<AlbumID3>> {
    let options = ListOptions {
        size: Some(RECENT as u32),
        ..ListOptions::default()
    };
    let list = client.get_album_list2(&AlbumListType::Recent, &options);
    Ok(list.await?.album)
}

fn shelve(ui: &AppWindow, browser: &Shared, albums: Vec<AlbumID3>) {
    browser.borrow_mut().played.albums = albums;
    draw(ui, browser);
}

/// Draws the shelves again once every artist has arrived, for the artists'
/// pictures.
pub(super) fn show(ui: &AppWindow, browser: &Shared) {
    let is_due = {
        let b = browser.borrow();
        b.artists_loaded && !b.played.has_artists && !b.played.albums.is_empty()
    };
    if is_due {
        draw(ui, browser);
    }
}

fn put(ui: &AppWindow, played: &Played) {
    ui.set_played_albums(ModelRc::from(played.album_tiles.clone()));
    ui.set_played_artists(ModelRc::from(played.artist_tiles.clone()));
}

/// Makes both shelves' tiles anew, each with its picture where it has been
/// seen, and fetches the others.
fn draw(ui: &AppWindow, browser: &Shared) {
    let size = tile_pixels(ui);
    let (wanted_albums, wanted_artists) = {
        let mut b = browser.borrow_mut();
        let (albums, wanted_albums) = seen_covers(&b, album_tiles(&b), size, false);
        let (artists, wanted_artists) = seen_covers(&b, artist_tiles(&b), size, false);
        let has_artists = b.artists_loaded;
        b.played.album_tiles = albums;
        b.played.artist_tiles = artists;
        b.played.has_artists = has_artists;
        put(ui, &b.played);
        (wanted_albums, wanted_artists)
    };
    side_covers(
        browser,
        |b| &b.played.album_tiles,
        wanted_albums,
        size,
        false,
    );
    side_covers(
        browser,
        |b| &b.played.artist_tiles,
        wanted_artists,
        size,
        false,
    );
}

fn album_tiles(b: &Browser) -> Vec<(AlbumTile, Option<String>)> {
    (b.played.albums.iter())
        .map(|album| {
            let (mut tile, cover) = album_tile(album, album_names(album));
            tile.downloaded = b.downloads.is_kept(&tile.link);
            (tile, cover)
        })
        .collect()
}

/// The artists' tiles; with every artist in, with their pictures and how
/// many albums they have.
fn artist_tiles(b: &Browser) -> Vec<(AlbumTile, Option<String>)> {
    (artists_of(&b.played.albums).into_iter())
        .map(|name| {
            let id = name.link.strip_prefix("artist:").unwrap_or_default();
            match b.artists.iter().find(|artist| artist.id == id) {
                Some(artist) => artist_tile(artist),
                None => {
                    let tile = AlbumTile {
                        title: name.label,
                        link: name.link,
                        ..AlbumTile::default()
                    };
                    (tile, None)
                }
            }
        })
        .collect()
}

/// The artists credited on `albums`, in their order and each once: those
/// with an id, which their tiles open.
fn artists_of(albums: &[AlbumID3]) -> Vec<Name> {
    let mut seen = HashSet::new();
    (albums.iter().flat_map(album_names))
        .filter(|name| !name.link.is_empty() && seen.insert(name.link.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(credits: &str) -> AlbumID3 {
        serde_json::from_str(&format!(
            r#"{{"id":"a","name":"A","songCount":1,"duration":1,"created":"2024-01-01T00:00:00Z",{credits}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn the_artists_come_in_the_albums_order_each_once_and_only_with_an_id() {
        let albums = [
            album(r#""artist":"Mara Lind","artistId":"r1""#),
            album(r#""artist":"Bach Ensemble""#),
            album(r#""artists":[{"id":"r3","name":"Anna Bach"},{"id":"r1","name":"Mara Lind"}]"#),
        ];
        let links: Vec<String> = (artists_of(&albums).iter())
            .map(|name| name.link.to_string())
            .collect();
        assert_eq!(links, ["artist:r1", "artist:r3"]);
    }
}
