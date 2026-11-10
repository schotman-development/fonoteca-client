//! What LRCLIB had for a song, kept on this device in a file of its own per
//! song, so that a song is asked about once and its words show offline. A song
//! LRCLIB had nothing for is kept too.

use lrclib::{Lyrics, TimedLine};
use serde::{Deserialize, Serialize};

/// The folder in the app's settings folder, and the browser's key prefix,
/// the files go under.
const FOLDER: &str = "lyrics";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    /// The song as it was asked about. File names are a hash of it, so a name
    /// shared by two songs gives one of them nothing instead of the other's
    /// words.
    pub key: String,
    /// None when LRCLIB had no words. Files kept before the lines had times
    /// hold just the text.
    pub words: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timed: Vec<TimedLine>,
}

/// The song's words as LRCLIB was asked for them: its artist, title, album
/// and length.
pub fn key(song: lrclib::Song<'_>) -> String {
    let album = song.album.unwrap_or_default();
    let seconds = song.seconds.map(|seconds| seconds.to_string());
    let seconds = seconds.unwrap_or_default();
    format!("{}\n{}\n{album}\n{seconds}", song.artist, song.title)
}

/// The name of the file `key` is kept in, a 64-bit FNV-1a hash of it, which
/// unlike `DefaultHasher` doesn't change with the Rust version.
pub fn file_name(key: &str) -> String {
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
    });
    format!("{FOLDER}/{hash:016x}")
}

/// What is kept for `key`: `Some(None)` is a song LRCLIB had nothing for and
/// `None` a song not asked about yet.
pub fn load(key: &str) -> Option<Option<Lyrics>> {
    let kept = crate::prefs::kept_lyrics(&file_name(key))?;
    if kept.key != key {
        return None;
    }
    let lyrics = kept.words.map(|text| Lyrics {
        text,
        timed: kept.timed,
    });
    Some(lyrics)
}

pub fn save(key: &str, lyrics: Option<&Lyrics>) {
    let kept = Kept {
        key: key.to_owned(),
        words: lyrics.map(|lyrics| lyrics.text.clone()),
        timed: lyrics
            .map(|lyrics| lyrics.timed.clone())
            .unwrap_or_default(),
    };
    crate::prefs::save_kept_lyrics(&file_name(key), &kept);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song<'a>(title: &'a str, seconds: Option<i64>) -> lrclib::Song<'a> {
        lrclib::Song {
            artist: "Artist",
            title,
            album: Some("Album"),
            seconds,
        }
    }

    #[test]
    fn a_song_is_asked_about_once_whether_or_not_it_had_words() {
        let (found, missing) = (
            key(song("Found", Some(200))),
            key(song("Missing", Some(200))),
        );
        assert_eq!(load(&found), None, "not asked yet");
        let words = Lyrics {
            text: "Some words".into(),
            timed: vec![TimedLine {
                seconds: 1.5,
                text: "Some words".into(),
            }],
        };
        save(&found, Some(&words));
        save(&missing, None);
        assert_eq!(load(&found), Some(Some(words)));
        assert_eq!(load(&missing), Some(None));
    }

    #[test]
    fn a_file_kept_before_lines_had_times_still_reads() {
        let old = key(song("Old", Some(200)));
        let earlier = format!(r#"{{"key":{old:?},"words":"Kept text"}}"#);
        let kept: Kept = serde_json::from_str(&earlier).unwrap();
        crate::prefs::save_kept_lyrics(&file_name(&old), &kept);
        let lyrics = load(&old).unwrap().unwrap();
        assert_eq!((lyrics.text.as_str(), lyrics.timed.len()), ("Kept text", 0));
    }

    #[test]
    fn another_length_or_a_clashing_file_name_is_another_song() {
        assert_ne!(key(song("T", Some(200))), key(song("T", Some(201))));
        assert_ne!(key(song("T", Some(200))), key(song("T", None)));
        let clashing = key(song("Clash", Some(1)));
        crate::prefs::save_kept_lyrics(
            &file_name(&clashing),
            &Kept {
                key: "another song".into(),
                words: Some("its words".into()),
                timed: Vec::new(),
            },
        );
        assert_eq!(load(&clashing), None);
    }

    #[test]
    fn file_names_are_stable() {
        assert_eq!(file_name(""), "lyrics/cbf29ce484222325");
        assert_eq!(file_name("a"), "lyrics/af63dc4c8601ec8c");
    }
}
