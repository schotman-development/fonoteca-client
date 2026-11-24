//! Songs that sound alike, queued without being asked for one by one: a
//! radio, started from a song, an album or an artist, and auto play, which
//! goes on from the last song when the queue has played to its end. Both ask
//! the server for songs like the one the queue ends with, whenever the
//! queue runs low, and skip the songs that were queued already.

use std::collections::HashSet;

use opensubsonic::{Child, Client, SONIC_SIMILARITY};
use slint::ComponentHandle;

use super::{Shared, begin, insert, playable, songs_of};
use crate::queue::Queue;
use crate::{AppWindow, Page, browse, prefs};

/// How many songs a lookup asks for.
const BATCH_SIZE: u32 = 50;
/// A radio asks for more when this few songs are left after the one playing.
const RUNNING_LOW: usize = 5;
// The lookup takes seconds, so the window shows a spinner with this and stays usable.
const SEARCHING: &str = "Looking for songs that sound alike…";
// The server answers with none until it has analysed the library.
const NOTHING_YET: &str = "Nothing to compare yet.";
const RUN_OUT: &str = "There are no more songs that sound alike to play.";

/// What a radio is started from.
enum Seed {
    Song(String),
    /// An album, and its first song, which stands for it where the server
    /// compares songs by their sound.
    Album {
        id: String,
        first_song: Option<String>,
    },
    Artist(String),
}

/// What the radio or auto play has queued so far, and whether it asks for more.
#[derive(Default)]
pub(super) struct Following {
    /// The ids of the songs queued, so that none comes twice.
    heard: HashSet<String>,
    /// Whether the queue is a radio's, which goes on without the setting.
    radio: bool,
    asking: bool,
    /// Whether the last answer had nothing new, after which it stops asking
    /// until songs are queued.
    run_out: bool,
}

impl Following {
    /// A radio that begins with `first_play`.
    fn radio(first_play: &[Child]) -> Self {
        let mut following = Following {
            radio: true,
            ..Following::default()
        };
        following
            .heard
            .extend(first_play.iter().map(|song| song.id.clone()));
        following
    }

    /// Whether to ask for more songs now: `ahead` songs are left after the
    /// one playing, and `ended` says the queue played to its end.
    fn wants_more(&self, auto_play: bool, ahead: usize, ended: bool) -> bool {
        if self.asking || self.run_out {
            return false;
        }
        if self.radio {
            return ahead < RUNNING_LOW;
        }
        auto_play && ended && ahead == 0
    }

    /// The songs of `found` not queued before, each once, which are queued
    /// now.
    fn fresh(&mut self, found: Vec<Child>) -> Vec<Child> {
        found
            .into_iter()
            .filter(|song| self.heard.insert(song.id.clone()))
            .collect()
    }

    /// Songs were queued that this didn't ask for, so it may find more.
    pub(super) fn reopen(&mut self) {
        self.run_out = false;
    }
}

/// Wires the settings' auto play, the song menus' and album and artist pages'
/// "Start radio", and the sheet's for the song playing.
pub(super) fn wire(ui: &AppWindow, browser: &browse::Shared, playing: &Shared) {
    ui.set_auto_play(prefs::load().auto_play());
    ui.on_choose_auto_play(|on| {
        let mut saved = prefs::load();
        saved.auto_play = Some(on);
        prefs::save(&saved);
    });
    ui.on_start_song_radio({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row| {
            let ui = weak.unwrap();
            let (songs, start) = browse::songs(&browser, ui.get_page(), row as usize);
            if let Some(song) = songs.get(start) {
                start_radio(&ui, &browser, &playing, format!("song:{}", song.id));
            }
        }
    });
    ui.on_start_playing_radio({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            let playing_id = super::shown_song_id(&playing.borrow());
            if let Some(id) = playing_id {
                start_radio(&weak.unwrap(), &browser, &playing, format!("song:{id}"));
            }
        }
    });
    ui.on_start_page_radio({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            let ui = weak.unwrap();
            let link = match ui.get_page() {
                Page::Artist => Some(format!("artist:{}", ui.get_shown_id())),
                _ => browse::songs(&browser, ui.get_page(), 0)
                    .0
                    .first()
                    .and_then(|song| song.album_id.as_ref())
                    .map(|id| format!("album:{id}")),
            };
            if let Some(link) = link {
                start_radio(&ui, &browser, &playing, link);
            }
        }
    });
}

/// Replaces the queue with the first play of what `link` names, a song's
/// link, an album's or an artist's, and the songs that sound like it, which
/// it goes on to ask for. With no such songs, the queue stays as it is.
pub(super) fn start_radio(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    link: String,
) {
    let Some(client) = browse::client(browser) else {
        return;
    };
    let (kept, offline) = (browse::downloads(browser), browse::is_offline(browser));
    ui.set_working(SEARCHING.into());
    let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
    slint::spawn_local(async move {
        let found = first_play_and_similar(&client, &kept, offline, &link).await;
        let Some(ui) = weak.upgrade() else { return };
        ui.set_working("".into());
        match found {
            Ok((first_play, similar)) => {
                radio_arrived(&ui, &browser, &playing, first_play, similar)
            }
            Err(e) => browse::notify(&ui, &browser, not_fetched(&e)),
        }
    })
    .expect("the event loop is running");
}

fn radio_arrived(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    first_play: Vec<Child>,
    similar: Vec<Child>,
) {
    let mut following = Following::radio(&first_play);
    let similar = following.fresh(similar);
    if similar.is_empty() {
        return browse::notify(ui, browser, NOTHING_YET.into());
    }
    let (songs, _) = playable(ui, browser, [first_play, similar].concat(), 0);
    // Unshuffled: the songs come in the order of their likeness.
    begin(
        ui,
        browser,
        playing,
        Queue::from_page(songs, 0),
        (0, 0.0),
        false,
    );
    playing.borrow_mut().following = following;
}

/// Asks for more songs like the last one queued when the radio runs low, or
/// when auto play is on and the queue has played to its end, and queues them.
/// Called on each tick of the player.
pub(super) fn keep_going(ui: &AppWindow, browser: &browse::Shared, playing: &Shared) {
    let Some(last) = start_asking(ui, playing) else {
        return;
    };
    let Some(client) = browse::client(browser) else {
        return;
    };
    let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
    slint::spawn_local(async move {
        let found = similar_to(&client, &Seed::Song(last.id.clone())).await;
        let Some(ui) = weak.upgrade() else { return };
        top_up_arrived(&ui, &browser, &playing, &last, found);
    })
    .expect("the event loop is running");
}

/// The last song queued, when it is time to ask for songs like it, which is
/// then marked as asked for.
fn start_asking(ui: &AppWindow, playing: &Shared) -> Option<Child> {
    let mut guard = playing.borrow_mut();
    let p = &mut *guard;
    let player = p.player.as_ref().filter(|_| p.station.is_none())?;
    let status = player.status();
    if status.requeues != p.requeued {
        return None;
    }
    let ahead = status
        .index
        .map_or(0, |at| p.queue.len().saturating_sub(at + 1));
    let ended = status.ended && status.stopped.is_none();
    let auto_play = ui.get_auto_play() && ui.get_radio_offered();
    if !p.following.wants_more(auto_play, ahead, ended) {
        return None;
    }
    let last = p.queue.songs().last()?.clone();
    p.following
        .heard
        .extend(p.queue.songs().map(|song| song.id.clone()));
    p.following.asking = true;
    Some(last)
}

fn top_up_arrived(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    asked_from: &Child,
    found: opensubsonic::Result<Vec<Child>>,
) {
    let mut p = playing.borrow_mut();
    p.following.asking = false;
    // Another queue, or more songs in this one, since: the answer is for a
    // queue that is gone.
    if p.queue.songs().last().map(|song| &song.id) != Some(&asked_from.id) {
        return;
    }
    let songs = match found {
        Ok(found) => p.following.fresh(found),
        Err(e) => {
            p.following.run_out = true;
            drop(p);
            return browse::notify(ui, browser, not_fetched(&e));
        }
    };
    if songs.is_empty() {
        p.following.run_out = true;
        drop(p);
        return browse::notify(ui, browser, RUN_OUT.into());
    }
    let radio = p.following.radio;
    drop(p);
    let (songs, _) = playable(ui, browser, songs, 0);
    insert(ui, browser, playing, songs, false, None);
    if !radio {
        let text = format!("Playing songs that sound like {}.", asked_from.title);
        browse::notify(ui, browser, text);
    }
}

fn not_fetched(error: &opensubsonic::Error) -> String {
    format!("Songs that sound alike couldn't be fetched: {error}")
}

/// What `link` names, which a radio plays first, and the songs like it.
async fn first_play_and_similar(
    client: &Client,
    kept: &crate::offline::Downloads,
    offline: bool,
    link: &str,
) -> opensubsonic::Result<(Vec<Child>, Vec<Child>)> {
    let first_play = match link.split_once(':') {
        Some(("song" | "album", _)) => songs_of(client, kept, offline, link).await?,
        _ => Vec::new(),
    };
    let seed = match link.split_once(':') {
        Some(("song", id)) => Seed::Song(id.into()),
        Some(("album", id)) => Seed::Album {
            id: id.into(),
            first_song: first_play.first().map(|song| song.id.clone()),
        },
        Some(("artist", id)) => Seed::Artist(id.into()),
        _ => return Ok((Vec::new(), Vec::new())),
    };
    let similar = similar_to(client, &seed).await?;
    Ok((first_play, similar))
}

/// The songs that sound like `seed`, most alike first where the server says.
/// Where it compares songs by their sound, a song's and an album's (by its
/// first song) go by that; else its `getSimilarSongs` answers for a song and
/// an album, and `getSimilarSongs2` for an artist.
async fn similar_to(client: &Client, seed: &Seed) -> opensubsonic::Result<Vec<Child>> {
    match seed {
        Seed::Song(id) => client.similar_songs(id, BATCH_SIZE).await,
        Seed::Album {
            first_song: Some(song),
            ..
        } if client.supports(SONIC_SIMILARITY) => client.similar_songs(song, BATCH_SIZE).await,
        Seed::Album { id, .. } => Ok(client.get_similar_songs(id, Some(BATCH_SIZE)).await?.song),
        Seed::Artist(id) => Ok(client.get_similar_songs2(id, Some(BATCH_SIZE)).await?.song),
    }
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
    fn a_radio_skips_the_songs_it_played_first_and_the_ones_it_queued() {
        let mut radio = Following::radio(&songs(&["seed"]));
        assert_eq!(
            ids(radio.fresh(songs(&["seed", "a", "b", "a"]))),
            ["a", "b"]
        );
        assert_eq!(ids(radio.fresh(songs(&["b", "c"]))), ["c"]);
        assert!(radio.fresh(songs(&["a", "c"])).is_empty());
    }

    #[test]
    fn a_radio_asks_for_more_as_it_runs_low_whatever_the_setting() {
        let radio = Following::radio(&[]);
        assert!(radio.wants_more(false, RUNNING_LOW - 1, false));
        assert!(!radio.wants_more(false, RUNNING_LOW, false));
    }

    #[test]
    fn auto_play_asks_only_once_the_queue_has_played_to_its_end() {
        let queue = Following::default();
        assert!(queue.wants_more(true, 0, true));
        assert!(!queue.wants_more(false, 0, true), "turned off");
        assert!(!queue.wants_more(true, 0, false), "still playing the last");
        assert!(!queue.wants_more(true, 3, true), "stopped short of the end");
    }

    #[test]
    fn nothing_is_asked_while_asking_or_after_an_answer_with_nothing_new() {
        let mut radio = Following::radio(&[]);
        radio.asking = true;
        assert!(!radio.wants_more(true, 0, true));
        radio.asking = false;
        radio.run_out = true;
        assert!(!radio.wants_more(true, 0, true));
        radio.reopen();
        assert!(radio.wants_more(true, 0, true));
    }
}
