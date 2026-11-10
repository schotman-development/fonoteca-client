//! The words of what plays: the song's own lyrics on servers with
//! `songLyrics`, else those found by its artist and title. Where the server
//! has none and the setting allows it, LRCLIB is asked, once per song. Where
//! the lyrics are synced, the line sung now is marked as the song plays.

pub mod kept;

use std::sync::Arc;

use lrclib::{Lyrics, TimedLine};
use opensubsonic::{Child, Client, LyricsList};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppWindow, LyricLine, browse, prefs};

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
    clear(ui);
    ui.set_playing_lyrics_note(LOOKING.into());
    let weak = ui.as_weak();
    let song = song.clone();
    slint::spawn_local(async move {
        let found = words_of(client, &song).await;
        let (Some(ui), true) = (weak.upgrade(), still_playing()) else {
            return;
        };
        match found {
            Ok(lyrics) if has_words(&lyrics) => set_lyrics(&ui, lyrics),
            Ok(_) => ui.set_playing_lyrics_note(NONE_FOUND.into()),
            Err(problem) => {
                eprintln!("The lyrics of {}: {problem}", song.id);
                ui.set_playing_lyrics_note(UNANSWERED.into());
            }
        }
    })
    .expect("the event loop is running");
}

/// The server's lyrics for `song`, else, where the setting allows it,
/// LRCLIB's. A server that didn't answer still leaves LRCLIB's words to
/// show, as offline; without any, its problem is what is returned.
async fn words_of(client: Option<Arc<Client>>, song: &Child) -> Result<Lyrics, String> {
    let server = words_from_server(client, song).await;
    if server.as_ref().is_ok_and(has_words) || !prefs::load().look_up_lyrics {
        return server;
    }
    match words_from_lrclib(song).await {
        Some(lyrics) => Ok(lyrics),
        None => server,
    }
}

fn has_words(lyrics: &Lyrics) -> bool {
    !lyrics.text.trim().is_empty()
}

async fn words_from_server(client: Option<Arc<Client>>, song: &Child) -> Result<Lyrics, String> {
    let Some(client) = client else {
        return Ok(Lyrics::default());
    };
    if client.supports(EXTENSION) {
        let list = client.get_lyrics_by_song_id(&song.id, false).await;
        return list.map(|list| lyrics_of(&list)).map_err(|e| e.to_string());
    }
    let found = client
        .get_lyrics(song.artist.as_deref(), Some(&song.title))
        .await;
    let plain = |lyrics: opensubsonic::Lyrics| Lyrics {
        text: lyrics.value,
        timed: Vec::new(),
    };
    found.map(plain).map_err(|e| e.to_string())
}

/// What LRCLIB has for `song`, from this device when it was asked about
/// before. A failed ask is not kept, so the next play asks again.
async fn words_from_lrclib(song: &Child) -> Option<Lyrics> {
    let asked = asked_about(song)?;
    let key = kept::key(asked);
    if let Some(lyrics) = kept::load(&key) {
        return lyrics;
    }
    let lrclib = lrclib::Client::with_root(&lrclib_root(), browse::USER_AGENT);
    let found = match lrclib {
        Ok(lrclib) => lrclib.lyrics(asked).await,
        Err(problem) => Err(problem),
    };
    match found {
        Ok(lyrics) => {
            kept::save(&key, lyrics.as_ref());
            lyrics
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
    ui.set_playing_lyric_lines(ModelRc::default());
    ui.set_playing_lyric_current(-1);
}

fn set_lyrics(ui: &AppWindow, lyrics: Lyrics) {
    let lines: Vec<LyricLine> = lyrics
        .timed
        .iter()
        .map(|line| LyricLine {
            text: line.text.as_str().into(),
            start: line.seconds as f32,
        })
        .collect();
    ui.set_playing_lyrics(lyrics.text.into());
    ui.set_playing_lyric_lines(ModelRc::new(VecModel::from(lines)));
    follow(ui);
}

/// Marks the line sung at the position the song is at, for the lyrics view
/// to highlight and scroll to. Called whenever the position moves.
pub fn follow(ui: &AppWindow) {
    let starts: Vec<f64> = ui
        .get_playing_lyric_lines()
        .iter()
        .map(|line| f64::from(line.start))
        .collect();
    let current = line_at(&starts, f64::from(ui.get_playing_position()));
    let current = current.map_or(-1, |at| at as i32);
    if ui.get_playing_lyric_current() != current {
        ui.set_playing_lyric_current(current);
    }
}

/// The last line that has started at `position` seconds, if any; `starts`
/// is in order.
fn line_at(starts: &[f64], position: f64) -> Option<usize> {
    starts
        .partition_point(|&start| start <= position)
        .checked_sub(1)
}

/// The first of a song's lyrics that has lines, a line of text to each, with
/// their times where the lyrics are synced and every line has one.
fn lyrics_of(list: &LyricsList) -> Lyrics {
    let Some(found) = list
        .structured_lyrics
        .iter()
        .find(|lyrics| !lyrics.line.is_empty())
    else {
        return Lyrics::default();
    };
    let lines: Vec<&str> = found.line.iter().map(|line| line.value.as_str()).collect();
    // Positive offsets make the lines appear sooner.
    let offset = found.offset.unwrap_or_default();
    let starts: Option<Vec<f64>> = found.line.iter().map(|line| line.start).collect();
    let timed = starts
        .filter(|_| found.synced)
        .map_or_else(Vec::new, |starts| {
            let timed = starts.iter().zip(&lines);
            timed
                .filter(|(_, text)| !text.trim().is_empty())
                .map(|(start, text)| TimedLine {
                    seconds: ((start - offset) / 1000.0).max(0.0),
                    text: (*text).to_owned(),
                })
                .collect()
        });
    Lyrics {
        text: lines.join("\n"),
        timed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(json: &str) -> LyricsList {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_lines_of_the_first_lyrics_with_any_are_the_text_and_synced_ones_keep_their_times() {
        let both = list(
            r#"{"structuredLyrics":[{"lang":"und","synced":true,"line":[]},{"lang":"en","synced":true,"line":[{"start":0,"value":"First line"},{"start":2000,"value":"Second line"}]},{"lang":"de","synced":false,"line":[{"value":"Erste Zeile"}]}]}"#,
        );
        let found = lyrics_of(&both);
        assert_eq!(found.text, "First line\nSecond line");
        let times: Vec<(f64, &str)> = found
            .timed
            .iter()
            .map(|line| (line.seconds, line.text.as_str()))
            .collect();
        assert_eq!(times, [(0.0, "First line"), (2.0, "Second line")]);
    }

    #[test]
    fn unsynced_lines_and_lines_without_a_start_have_no_times() {
        let unsynced = list(
            r#"{"structuredLyrics":[{"lang":"de","synced":false,"line":[{"value":"Erste Zeile"}]}]}"#,
        );
        assert_eq!(lyrics_of(&unsynced).timed, []);
        let gap = list(
            r#"{"structuredLyrics":[{"lang":"en","synced":true,"line":[{"start":0,"value":"A"},{"value":"B"}]}]}"#,
        );
        assert_eq!(
            (lyrics_of(&gap).text.as_str(), lyrics_of(&gap).timed.len()),
            ("A\nB", 0)
        );
    }

    #[test]
    fn the_offset_moves_the_times_sooner_and_blank_lines_are_not_marked() {
        let offset = list(
            r#"{"structuredLyrics":[{"lang":"en","synced":true,"offset":500,"line":[{"start":300,"value":"A"},{"start":2500,"value":""},{"start":4500,"value":"C"}]}]}"#,
        );
        let seconds: Vec<f64> = lyrics_of(&offset)
            .timed
            .iter()
            .map(|line| line.seconds)
            .collect();
        assert_eq!(seconds, [0.0, 4.0]);
    }

    #[test]
    fn a_song_without_lyrics_has_no_words() {
        assert_eq!(
            lyrics_of(&list(r#"{"structuredLyrics":[]}"#)),
            Lyrics::default()
        );
        assert_eq!(lyrics_of(&list("{}")), Lyrics::default());
    }

    #[test]
    fn the_line_sung_is_the_last_that_has_started() {
        let starts = [1.0, 5.0, 5.0, 9.5];
        assert_eq!(line_at(&starts, 0.0), None, "before the first line");
        assert_eq!(line_at(&starts, 1.0), Some(0));
        assert_eq!(line_at(&starts, 4.99), Some(0));
        assert_eq!(line_at(&starts, 5.0), Some(2), "of two at once, the later");
        assert_eq!(line_at(&starts, 600.0), Some(3));
        assert_eq!(line_at(&[], 3.0), None);
    }
}
