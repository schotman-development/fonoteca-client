//! This device's log of plays, the base of the listening history: a play is
//! logged when ListenBrainz would count it, with what its row needs, so the
//! history shows and plays it without asking anyone. A line each, in a file
//! for each account in a plays folder beside the downloads, removed on sign
//! out and with the server.

// The browser plays nothing, so it logs and reads nothing.
#![cfg_attr(target_family = "wasm", allow(dead_code))]

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use opensubsonic::Child;
use serde::{Deserialize, Serialize};

/// Held while the log is added to or removed, so that a removal can't be
/// overtaken by an addition that started before it.
static WRITING: Mutex<()> = Mutex::new(());

/// A song played on this device, and when it began.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Play {
    /// In seconds since 1970, the time the server is told it began.
    pub listened_at: i64,
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    /// In seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    /// Its recording's MusicBrainz ID, which the history matches ListenBrainz's
    /// listens by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording: Option<String>,
}

impl Play {
    pub fn of(song: &Child, listened_at: i64) -> Self {
        Play {
            listened_at,
            id: song.id.clone(),
            title: song.title.clone(),
            artist: song.artist.clone(),
            artist_id: song.artist_id.clone(),
            album: song.album.clone(),
            album_id: song.album_id.clone(),
            cover_art: song.cover_art.clone(),
            duration: song.duration,
            recording: song.music_brainz_id.clone(),
        }
    }

    /// The song played, as much of it as was logged.
    pub fn song(&self) -> Child {
        Child {
            id: self.id.clone(),
            title: self.title.clone(),
            artist: self.artist.clone(),
            artist_id: self.artist_id.clone(),
            album: self.album.clone(),
            album_id: self.album_id.clone(),
            cover_art: self.cover_art.clone(),
            duration: self.duration,
            music_brainz_id: self.recording.clone(),
            ..Child::default()
        }
    }
}

/// Adds `play` to the log of `account`, off the UI thread. One that can't be
/// written is missing from the history, and still goes to the server.
#[cfg(not(target_family = "wasm"))]
pub fn add(account: &str, play: Play) {
    let account = account.to_owned();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = append(&account, &play) {
            eprintln!("A play couldn't be logged: {e}");
        }
    });
}

/// Writes `play` at the end of the log of `account`.
pub fn append(account: &str, play: &Play) -> io::Result<()> {
    let Some(file) = file(account) else {
        return Ok(());
    };
    // Each line starts a new one, so a line a crash cut short doesn't take
    // the next play with it.
    let line = format!("\n{}", serde_json::to_string(play)?);
    let _writing = WRITING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?;
    log.write_all(line.as_bytes())
}

/// The plays logged for `account`, in the order they were logged, read off
/// the UI thread. None when they can't be read, which the history then
/// lacks.
#[cfg(not(target_family = "wasm"))]
pub async fn read(account: &str) -> Vec<Play> {
    let Some(file) = file(account) else {
        return Vec::new();
    };
    let read = tokio::task::spawn_blocking(move || load(&file)).await;
    match read.map_err(io::Error::other).and_then(|read| read) {
        Ok(plays) => plays,
        Err(e) => {
            eprintln!("The plays logged couldn't be read: {e}");
            Vec::new()
        }
    }
}

#[cfg(target_family = "wasm")]
pub async fn read(_: &str) -> Vec<Play> {
    Vec::new()
}

/// The plays in the log at `file`, none when there is none. A line a crash
/// cut short is left out.
fn load(file: &Path) -> io::Result<Vec<Play>> {
    let text = match std::fs::read_to_string(file) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        read => read?,
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// Removes the log of `account` from this device.
pub fn forget(account: &str) -> io::Result<()> {
    let Some(file) = file(account) else {
        return Ok(());
    };
    let _writing = WRITING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The plays folder beside the downloads in the app's data folder.
#[cfg(not(any(target_family = "wasm", test)))]
fn file(account: &str) -> Option<PathBuf> {
    let folder = crate::offline::folder()?.with_file_name("plays");
    Some(folder.join(format!("{account}.jsonl")))
}

#[cfg(target_family = "wasm")]
fn file(_: &str) -> Option<PathBuf> {
    None
}

/// Each test process keeps its own.
#[cfg(test)]
pub fn file(account: &str) -> Option<PathBuf> {
    let folder = std::env::temp_dir().join(format!("fonoteca-plays-{}", std::process::id()));
    Some(folder.join(format!("{account}.jsonl")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Child {
        serde_json::from_str(
            r#"{"id": "s1", "title": "Salt", "isDir": false, "artist": "Mara Lind",
                "artistId": "ar1", "album": "Night Harbour", "albumId": "al1",
                "coverArt": "al-1", "duration": 200, "year": 2020,
                "musicBrainzId": "00000000-0000-4000-8000-000000000005"}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_play_keeps_what_its_row_and_listenbrainz_need_of_its_song() {
        let play = Play::of(&song(), 1000);
        let again = play.song();
        assert_eq!(play.listened_at, 1000);
        assert_eq!((again.id.as_str(), again.title.as_str()), ("s1", "Salt"));
        assert_eq!(again.artist.as_deref(), Some("Mara Lind"));
        assert_eq!(again.artist_id.as_deref(), Some("ar1"));
        assert_eq!(again.album.as_deref(), Some("Night Harbour"));
        assert_eq!(again.album_id.as_deref(), Some("al1"));
        assert_eq!(again.cover_art.as_deref(), Some("al-1"));
        assert_eq!(again.duration, Some(200));
        assert_eq!(
            again.music_brainz_id.as_deref(),
            Some("00000000-0000-4000-8000-000000000005")
        );
        assert_eq!(again.year, None, "only what the row needs is kept");
    }

    #[test]
    fn plays_are_logged_in_order_until_the_log_is_forgotten() {
        let account = "logged-in-order";
        forget(account).unwrap();
        append(account, &Play::of(&song(), 1)).unwrap();
        append(account, &Play::of(&song(), 2)).unwrap();
        let file = file(account).unwrap();
        let times =
            |plays: Vec<Play>| -> Vec<i64> { plays.iter().map(|p| p.listened_at).collect() };
        assert_eq!(times(load(&file).unwrap()), [1, 2]);
        forget(account).unwrap();
        assert!(!file.exists());
        assert!(load(&file).unwrap().is_empty());
        forget(account).expect("forgetting twice is fine");
    }

    #[test]
    fn a_line_a_crash_cut_short_is_left_out_and_the_next_play_kept() {
        let account = "cut-short";
        forget(account).unwrap();
        append(account, &Play::of(&song(), 1)).unwrap();
        let file = file(account).unwrap();
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        log.write_all(b"\n{\"listenedAt\":2,\"id\":\"s1\",\"ti")
            .unwrap();
        append(account, &Play::of(&song(), 3)).unwrap();
        let plays = load(&file).unwrap();
        assert_eq!(plays, [Play::of(&song(), 1), Play::of(&song(), 3)]);
        forget(account).unwrap();
    }
}
