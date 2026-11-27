//! The album page's Similar albums shelf. The server has no call for albums
//! that sound alike, so it asks for the songs that sound like a few of the
//! album's own and ranks the albums those songs are on.

use std::collections::HashMap;

use opensubsonic::{SONIC_SIMILARITY, SonicMatch};

use super::*;

/// How many of the album's songs to ask about, spread over the tracklist.
const SONGS_ASKED_ABOUT: usize = 5;
/// How many songs like each one to ask for.
const MATCHES_PER_SONG: u32 = 20;
/// How many albums the shelf shows.
const ALBUMS_SHOWN: usize = 12;

/// The shelf's tiles.
#[derive(Default)]
pub(super) struct SimilarAlbums {
    tiles: Rc<VecModel<AlbumTile>>,
}

/// An album the matches are on, with how well they fit.
#[derive(Debug, PartialEq)]
struct Alike {
    id: String,
    name: String,
    score: f64,
    /// The first song found on it, for the tile where the library has no such album.
    song: Child,
}

/// The ids of up to `SONGS_ASKED_ABOUT` songs spread evenly over `songs`,
/// the first and the last among them; all of them for a short album.
fn songs_to_ask_about(songs: &[Child]) -> Vec<String> {
    let wanted = SONGS_ASKED_ABOUT.min(songs.len());
    (0..wanted)
        .map(|n| match wanted {
            1 => 0,
            _ => n * (songs.len() - 1) / (wanted - 1),
        })
        .map(|index| songs[index].id.clone())
        .collect()
}

/// The albums the `answers` (the matches of each song asked about) are on,
/// without `own_album`, the best first: by the sum of their matches'
/// similarity, ties by name.
fn rank(own_album: &str, answers: &[Vec<SonicMatch>]) -> Vec<Alike> {
    let mut found: HashMap<&str, Alike> = HashMap::new();
    for matched in answers.iter().flatten() {
        let song = &matched.entry;
        let Some(id) = song.album_id.as_deref().filter(|id| *id != own_album) else {
            continue;
        };
        found
            .entry(id)
            .or_insert_with(|| Alike {
                id: id.into(),
                name: song.album.clone().unwrap_or_default(),
                score: 0.0,
                song: song.clone(),
            })
            .score += matched.similarity.max(0.0);
    }
    let mut albums: Vec<Alike> = found.into_values().collect();
    albums.sort_by(|a, b| {
        (b.score.total_cmp(&a.score))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    albums.truncate(ALBUMS_SHOWN);
    albums
}

/// Asks for the albums like the one on screen, whose songs are `songs`,
/// where the server compares by sound; the shelf is drawn once they arrive.
pub(super) fn ask(ui: &AppWindow, browser: &Shared, album_id: &str, songs: &[Child]) {
    let sounds_alike =
        (browser.borrow().client.as_ref()).is_some_and(|client| client.supports(SONIC_SIMILARITY));
    let seeds = songs_to_ask_about(songs);
    if is_offline(browser) || !sounds_alike || seeds.is_empty() {
        return;
    }
    let own_album = album_id.to_owned();
    aside(
        ui,
        browser,
        move |client| {
            let seeds = seeds.clone();
            async move {
                let mut answers = Vec::new();
                for seed in &seeds {
                    let asked = client.get_sonic_similar_tracks(seed, Some(MATCHES_PER_SONG));
                    answers.push(asked.await?);
                }
                Ok(answers)
            }
        },
        move |ui, browser, answers| shelve(ui, browser, rank(&own_album, &answers)),
    );
}

pub(super) fn clear(ui: &AppWindow, browser: &Shared) {
    browser.borrow_mut().similar_albums.tiles = Rc::default();
    ui.set_similar_albums(ModelRc::default());
}

fn shelve(ui: &AppWindow, browser: &Shared, albums: Vec<Alike>) {
    let size = tile_pixels(ui);
    let wanted = {
        let mut b = browser.borrow_mut();
        let tiles = albums.iter().map(|alike| tile(&b, alike)).collect();
        let (model, wanted) = seen_covers(&b, tiles, size, false);
        b.similar_albums.tiles = model;
        ui.set_similar_albums(ModelRc::from(b.similar_albums.tiles.clone()));
        wanted
    };
    side_covers(browser, |b| &b.similar_albums.tiles, wanted, size, false);
}

/// The album's tile from the library where it is there, else from the
/// fields of the song found on it.
fn tile(b: &Browser, alike: &Alike) -> (AlbumTile, Option<String>) {
    let (mut tile, cover) = match b.library.iter().find(|album| album.id == alike.id) {
        Some(album) => album_tile(album, album_names(album)),
        None => {
            let song = &alike.song;
            let tile = AlbumTile {
                title: alike.name.as_str().into(),
                names: names(named(&song.album_artists, &song.artist, &song.artist_id)),
                link: format!("album:{}", alike.id).into(),
                ..AlbumTile::default()
            };
            (tile, song.cover_art.clone())
        }
    };
    tile.downloaded = b.downloads.is_kept(&tile.link);
    (tile, cover)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn found(album: &str, similarity: f64) -> SonicMatch {
        serde_json::from_value(json!({
            "entry": { "id": format!("{album}-{similarity}"), "isDir": false, "title": "T",
                       "albumId": album, "album": album.to_uppercase() },
            "similarity": similarity,
        }))
        .unwrap()
    }

    fn ids(albums: &[Alike]) -> Vec<&str> {
        albums.iter().map(|album| album.id.as_str()).collect()
    }

    #[test]
    fn matches_are_summed_per_album_so_several_songs_pointing_at_one_beat_one_close_match() {
        let answers = vec![
            vec![found("a", 0.9), found("b", 0.5)],
            vec![found("b", 0.5), found("c", 0.8)],
        ];
        assert_eq!(ids(&rank("own", &answers)), ["b", "a", "c"]);
    }

    #[test]
    fn the_album_itself_is_dropped() {
        let answers = vec![vec![found("own", 0.99), found("a", 0.1)]];
        assert_eq!(ids(&rank("own", &answers)), ["a"]);
    }

    #[test]
    fn a_tie_is_broken_by_the_albums_name() {
        let answers = vec![vec![found("zed", 0.5), found("abe", 0.5)]];
        assert_eq!(ids(&rank("own", &answers)), ["abe", "zed"]);
    }

    #[test]
    fn at_most_twelve_albums_and_fewer_when_there_are_fewer() {
        let many: Vec<SonicMatch> = (0..20).map(|n| found(&format!("a{n:02}"), 0.5)).collect();
        assert_eq!(rank("own", &[many]).len(), ALBUMS_SHOWN);
        assert_eq!(rank("own", &[vec![found("a", 0.5)]]).len(), 1);
    }

    #[test]
    fn no_matches_give_no_albums() {
        assert!(rank("own", &[]).is_empty());
        assert!(rank("own", &[Vec::new(), Vec::new()]).is_empty());
    }

    #[test]
    fn songs_without_an_album_are_left_out() {
        let song: SonicMatch = serde_json::from_value(json!({
            "entry": { "id": "x", "isDir": false, "title": "T" }, "similarity": 0.9,
        }))
        .unwrap();
        assert!(rank("own", &[vec![song]]).is_empty());
    }

    fn songs(count: usize) -> Vec<Child> {
        (0..count)
            .map(|n| {
                let song = json!({ "id": n.to_string(), "isDir": false, "title": "T" });
                serde_json::from_value(song).unwrap()
            })
            .collect()
    }

    #[test]
    fn a_short_album_asks_about_all_its_songs_and_a_long_one_about_five_spread_over_it() {
        assert_eq!(songs_to_ask_about(&songs(3)), ["0", "1", "2"]);
        assert_eq!(songs_to_ask_about(&songs(1)), ["0"]);
        assert!(songs_to_ask_about(&songs(0)).is_empty());
        assert_eq!(songs_to_ask_about(&songs(13)), ["0", "3", "6", "9", "12"]);
    }
}
