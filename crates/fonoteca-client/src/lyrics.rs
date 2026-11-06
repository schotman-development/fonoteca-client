//! The words of what plays: the song's own lyrics on servers with
//! `songLyrics`, else those found by its artist and title.

use std::sync::Arc;

use opensubsonic::{Child, Client, LyricsList};
use slint::ComponentHandle;

use crate::AppWindow;

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
    let Some(client) = client else {
        ui.set_playing_lyrics_note(NONE_FOUND.into());
        return;
    };
    ui.set_playing_lyrics_note(LOOKING.into());
    let weak = ui.as_weak();
    let (id, artist, title) = (song.id.clone(), song.artist.clone(), song.title.clone());
    slint::spawn_local(async move {
        let found = if client.supports(EXTENSION) {
            let list = client.get_lyrics_by_song_id(&id, false).await;
            list.map(|list| words(&list))
        } else {
            let lyrics = client.get_lyrics(artist.as_deref(), Some(&title)).await;
            lyrics.map(|lyrics| lyrics.value)
        };
        let (Some(ui), true) = (weak.upgrade(), still_playing()) else {
            return;
        };
        match found {
            Ok(lyrics) if !lyrics.trim().is_empty() => ui.set_playing_lyrics(lyrics.into()),
            Ok(_) => ui.set_playing_lyrics_note(NONE_FOUND.into()),
            Err(problem) => {
                eprintln!("The lyrics of {id}: {problem}");
                ui.set_playing_lyrics_note(UNANSWERED.into());
            }
        }
    })
    .expect("the event loop is running");
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
