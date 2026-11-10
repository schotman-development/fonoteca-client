//! The words of what plays: the song's own lyrics on servers with
//! `songLyrics`, else those found by its artist and title. Where the server
//! has none and the setting allows it, LRCLIB is asked, once per song.

pub mod kept;

use std::sync::Arc;

use opensubsonic::{Child, Client, LyricsList};
use slint::ComponentHandle;

use crate::{AppWindow, browse, prefs};

const EXTENSION: &str = "songLyrics";
const LOOKING: &str = "Looking for the lyrics…";
const NONE_FOUND: &str = "No lyrics for this song.";
const UNANSWERED: &str = "The server didn't give the lyrics.";

/// Shows the lyrics of `song` once they come, and a note until then or
/// where there are none. `still_playing` says whether the song is still the
/// one shown, for a reply that arrives after it moved on.
pub fn show(
    ui: &AppWindow,
    client: Option<Arc<Client>>,
    song: &Child,
    still_playing: impl Fn() -> bool + 'static,
) {
    ui.set_playing_lyrics("".into());
    ui.set_playing_lyrics_note(LOOKING.into());
    let weak = ui.as_weak();
    let song = song.clone();
    slint::spawn_local(async move {
        let found = words_of(client, &song).await;
        let (Some(ui), true) = (weak.upgrade(), still_playing()) else {
            return;
        };
        match found {
            Ok(lyrics) if !lyrics.trim().is_empty() => ui.set_playing_lyrics(lyrics.into()),
            Ok(_) => ui.set_playing_lyrics_note(NONE_FOUND.into()),
            Err(problem) => {
                eprintln!("The lyrics of {}: {problem}", song.id);
                ui.set_playing_lyrics_note(UNANSWERED.into());
            }
        }
    })
    .expect("the event loop is running");
}

/// The server's words for `song`, else, where the setting allows it, LRCLIB's.
/// A server that didn't answer still leaves LRCLIB's words to show, as
/// offline; without any, its problem is what is returned.
async fn words_of(client: Option<Arc<Client>>, song: &Child) -> Result<String, String> {
    let server = words_from_server(client, song).await;
    if has_words(&server) || !prefs::load().look_up_lyrics {
        return server;
    }
    match words_from_lrclib(song).await {
        Some(words) => Ok(words),
        None => server,
    }
}

fn has_words(found: &Result<String, String>) -> bool {
    found.as_ref().is_ok_and(|words| !words.trim().is_empty())
}

async fn words_from_server(client: Option<Arc<Client>>, song: &Child) -> Result<String, String> {
    let Some(client) = client else {
        return Ok(String::new());
    };
    if client.supports(EXTENSION) {
        let list = client.get_lyrics_by_song_id(&song.id, false).await;
        return list.map(|list| words(&list)).map_err(|e| e.to_string());
    }
    let lyrics = client
        .get_lyrics(song.artist.as_deref(), Some(&song.title))
        .await;
    lyrics.map(|lyrics| lyrics.value).map_err(|e| e.to_string())
}

/// What LRCLIB has for `song`, from this device when it was asked about
/// before. A failed ask is not kept, so the next play asks again.
async fn words_from_lrclib(song: &Child) -> Option<String> {
    let asked = asked_about(song)?;
    let key = kept::key(asked);
    if let Some(words) = kept::load(&key) {
        return words;
    }
    let lrclib = lrclib::Client::with_root(&lrclib_root(), browse::USER_AGENT);
    let found = match lrclib {
        Ok(lrclib) => lrclib.words(asked).await,
        Err(problem) => Err(problem),
    };
    match found {
        Ok(words) => {
            kept::save(&key, words.as_deref());
            words
        }
        Err(problem) => {
            eprintln!("LRCLIB's lyrics of {}: {problem}", song.id);
            None
        }
    }
}

/// `song` as LRCLIB is asked about it; None for a song without an artist or
/// a title, which it can't match.
fn asked_about(song: &Child) -> Option<lrclib::Song<'_>> {
    let artist = song
        .artist
        .as_deref()
        .filter(|artist| !artist.trim().is_empty())?;
    let title = Some(song.title.as_str()).filter(|title| !title.trim().is_empty())?;
    Some(lrclib::Song {
        artist,
        title,
        album: song
            .album
            .as_deref()
            .filter(|album| !album.trim().is_empty()),
        seconds: song.duration,
    })
}

#[cfg(not(test))]
fn lrclib_root() -> String {
    lrclib::API_ROOT.into()
}

/// Where the tests' LRCLIB answers.
#[cfg(test)]
pub static LRCLIB_ROOT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

#[cfg(test)]
fn lrclib_root() -> String {
    LRCLIB_ROOT.lock().unwrap().clone()
}

/// Shows the setting that lets LRCLIB be asked, and keeps what is chosen.
pub fn wire(ui: &AppWindow) {
    ui.set_look_up_lyrics(prefs::load().look_up_lyrics);
    ui.on_choose_look_up_lyrics(|allowed| {
        let mut saved = prefs::load();
        saved.look_up_lyrics = allowed;
        prefs::save(&saved);
    });
}

/// Shows no lyrics and no note, for what has no words to look for, as a
/// radio station.
pub fn clear(ui: &AppWindow) {
    ui.set_playing_lyrics("".into());
    ui.set_playing_lyrics_note("".into());
}

/// The first of a song's lyrics that has lines, a line of text to each.
fn words(list: &LyricsList) -> String {
    list.structured_lyrics
        .iter()
        .find(|lyrics| !lyrics.line.is_empty())
        .map(|lyrics| {
            let lines: Vec<&str> = lyrics.line.iter().map(|line| line.value.as_str()).collect();
            lines.join("\n")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(json: &str) -> LyricsList {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_lines_of_the_first_lyrics_with_any_are_the_words() {
        let both = list(
            r#"{"structuredLyrics":[{"lang":"und","synced":true,"line":[]},{"lang":"en","synced":true,"line":[{"start":0,"value":"First line"},{"start":2000,"value":"Second line"}]},{"lang":"de","synced":false,"line":[{"value":"Erste Zeile"}]}]}"#,
        );
        assert_eq!(words(&both), "First line\nSecond line");
    }

    #[test]
    fn a_song_without_lyrics_has_no_words() {
        assert_eq!(words(&list(r#"{"structuredLyrics":[]}"#)), "");
        assert_eq!(words(&list("{}")), "");
    }
}
