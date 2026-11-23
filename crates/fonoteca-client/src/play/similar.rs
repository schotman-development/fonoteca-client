//! Songs that sound alike: a mix from a song or an artist, played or queued,
//! and a path of songs from where the queue ends to another song.

use std::future::Future;

use opensubsonic::Child;
use slint::ComponentHandle;

use super::{Shared, enqueue, play};
use crate::{AppWindow, browse};

/// How many songs a mix asks for.
const MIX_SIZE: u32 = 50;
/// How many songs a path asks for, the most the server gives.
const PATH_SIZE: u32 = 25;
// The lookup takes seconds, so the window shows a spinner with this and stays usable.
const SEARCHING: &str = "Looking for songs that sound alike…";
// The server answers with none until it has analysed the library.
const NOTHING_YET: &str = "Nothing to compare yet.";

/// Wires the song menus' "Play similar", "Add similar to queue" and "Make a
/// path to here", and the artist page's "Play something like this".
pub(super) fn wire(ui: &AppWindow, browser: &browse::Shared, playing: &Shared) {
    ui.on_similar_song({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row, how| {
            let ui = weak.unwrap();
            let (songs, start) = browse::songs(&browser, ui.get_page(), row as usize);
            let Some(song) = songs.into_iter().nth(start) else {
                return;
            };
            match how.as_str() {
                "path" => path_to(&ui, &browser, &playing, song.id),
                how => mix_of_song(&ui, &browser, &playing, song, how == "play"),
            }
        }
    });
    ui.on_similar_artist({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            let ui = weak.unwrap();
            let Some(client) = browse::client(&browser) else {
                return;
            };
            let artist = ui.get_shown_id().to_string();
            let lookup = async move {
                Ok(client
                    .get_similar_songs2(&artist, Some(MIX_SIZE))
                    .await?
                    .song)
            };
            run(
                &ui,
                &browser,
                &playing,
                lookup,
                |ui, browser, playing, songs| play(ui, browser, playing, songs, 0),
            );
        }
    });
}

/// Plays `song` and then the songs that sound like it, or queues those.
fn mix_of_song(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    song: Child,
    play_now: bool,
) {
    let Some(client) = browse::client(browser) else {
        return;
    };
    let id = song.id.clone();
    let lookup = async move { client.similar_songs(&id, MIX_SIZE).await };
    run(
        ui,
        browser,
        playing,
        lookup,
        move |ui, browser, playing, similar| {
            if play_now {
                play(ui, browser, playing, [vec![song], similar].concat(), 0);
            } else {
                enqueue(ui, browser, playing, similar, false, None);
            }
        },
    );
}

/// Queues the songs on the way from the last song of the queue to `end`.
fn path_to(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, end: String) {
    let Some(client) = browse::client(browser) else {
        return;
    };
    let queue_end = playing
        .borrow()
        .queue
        .songs()
        .last()
        .map(|song| song.id.clone());
    let Some(queue_end) = queue_end else {
        return browse::notify(
            ui,
            browser,
            "Play something first, to make a path from it.".into(),
        );
    };
    let lookup = async move {
        let path = client
            .find_sonic_path(&queue_end, &end, Some(PATH_SIZE))
            .await?;
        let path = path.into_iter().map(|found| found.entry).collect();
        Ok(after_queue_end(path, &queue_end))
    };
    run(
        ui,
        browser,
        playing,
        lookup,
        |ui, browser, playing, path| enqueue(ui, browser, playing, path, false, None),
    );
}

/// The songs of a path to queue: it begins with the song the queue ends on,
/// which is queued already.
fn after_queue_end(path: Vec<Child>, queue_end: &str) -> Vec<Child> {
    let begins_at_the_end = path.first().is_some_and(|song| song.id == queue_end);
    path.into_iter()
        .skip(usize::from(begins_at_the_end))
        .collect()
}

/// Says the lookup is under way, and hands the songs it finds to `deliver`
/// once it is done, or says there are none or why they weren't fetched.
fn run(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    lookup: impl Future<Output = opensubsonic::Result<Vec<Child>>> + 'static,
    deliver: impl FnOnce(&AppWindow, &browse::Shared, &Shared, Vec<Child>) + 'static,
) {
    ui.set_working(SEARCHING.into());
    let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
    slint::spawn_local(async move {
        let found = lookup.await;
        let Some(ui) = weak.upgrade() else { return };
        ui.set_working("".into());
        match found {
            Ok(songs) if songs.is_empty() => browse::notify(&ui, &browser, NOTHING_YET.into()),
            Ok(songs) => deliver(&ui, &browser, &playing, songs),
            Err(e) => browse::notify(
                &ui,
                &browser,
                format!("Songs that sound alike couldn't be fetched: {e}"),
            ),
        }
    })
    .expect("the event loop is running");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn songs(ids: &[&str]) -> Vec<Child> {
        let song = |id: &&str| {
            serde_json::from_value(serde_json::json!({ "id": id, "isDir": false, "title": id }))
                .unwrap()
        };
        ids.iter().map(song).collect()
    }

    fn ids(songs: Vec<Child>) -> Vec<String> {
        songs.into_iter().map(|song| song.id).collect()
    }

    #[test]
    fn a_path_leaves_out_the_song_the_queue_ends_on() {
        assert_eq!(
            ids(after_queue_end(songs(&["a", "x", "b"]), "a")),
            ["x", "b"]
        );
    }

    #[test]
    fn a_path_that_does_not_begin_at_the_queues_end_is_kept_whole() {
        assert_eq!(ids(after_queue_end(songs(&["x", "b"]), "a")), ["x", "b"]);
    }

    #[test]
    fn a_path_of_the_queues_end_alone_has_nothing_to_add() {
        assert!(after_queue_end(songs(&["a"]), "a").is_empty());
    }
}
