//! Preferences kept on this device: how the albums, artists, composers,
//! tracks, compositions and folders are sorted, which columns the tables
//! show, the radio page's languages, the sound device, volume leveling and
//! sound processing; and the pictures of saved stations the app found in the
//! radio directory. They aren't secrets, so plain files in the system's
//! settings folder keep them, and the browser preview its localStorage.
//! Where neither works they last until the app closes.

use std::collections::HashMap;
use std::fmt::Write;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The albums page's Sort menu entry and direction.
    pub album_sort: i32,
    pub album_descending: bool,
    /// The same for the artists page.
    pub artist_sort: i32,
    pub artist_descending: bool,
    /// The same for the composers page.
    pub composer_sort: i32,
    pub composer_descending: bool,
    /// The headings of the tracks table's columns shown, none for the first
    /// choice; the heading of the column it is sorted by, and the direction.
    pub track_columns: Option<Vec<String>>,
    pub track_sort: Option<String>,
    pub track_descending: bool,
    /// The same for the compositions table.
    pub composition_columns: Option<Vec<String>>,
    pub composition_sort: Option<String>,
    pub composition_descending: bool,
    /// The same for the folders' tables.
    pub folder_columns: Option<Vec<String>>,
    pub folder_sort: Option<String>,
    pub folder_descending: bool,
    /// The languages whose stations the radio page's genres show first, as
    /// the radio directory names them, in the order picked.
    pub radio_languages: Vec<String>,
    /// The sound device to play on, by cpal id; none for the system's
    /// default.
    pub output_device: Option<String>,
    /// Volume leveling's entry in the settings: off, by track or by album.
    pub leveling: i32,
    /// The format and bitrate to stream in, on wifi and on mobile data.
    pub wifi_quality: StreamQuality,
    pub mobile_quality: StreamQuality,
    /// The format and bitrate to download in.
    pub download_quality: StreamQuality,
    /// Whether songs may be downloaded over mobile data; wifi only without.
    pub download_on_mobile_data: bool,
    /// The sound processing's entries in the settings: headroom, crossfeed
    /// and upsampling; the EQ, whether it is on, and the EQs saved by name.
    pub headroom: i32,
    pub crossfeed: i32,
    pub upsampling: i32,
    pub eq_on: bool,
    pub eq: Equalizer,
    pub eq_presets: Vec<EqPreset>,
    /// Whether the queue repeats: not, all of it, or the track playing.
    pub repeat: i32,
    /// The works of the queue saved on the server whose rows the now playing
    /// sheet lists open, by the ids of their first songs: the server keeps
    /// only the songs.
    pub expanded_works: Vec<String>,
    /// The settings' entry for the most the downloads may take.
    pub download_limit: i32,
}

/// A streaming format's entry in the settings, and its bitrate's; the first
/// format is the song as the server keeps it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamQuality {
    pub format: i32,
    pub bitrate: i32,
}

/// A parametric EQ: its preamp in dB and its bands.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Equalizer {
    pub preamp: f64,
    pub bands: Vec<EqBand>,
}

/// A band: its filter's entry in the settings, its frequency in Hz, its
/// gain in dB and its Q.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqBand {
    pub filter: i32,
    pub frequency: f64,
    pub gain: f64,
    pub q: f64,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqPreset {
    pub name: String,
    pub eq: Equalizer,
}

/// The preferences saved, or the first choices when there are none.
pub fn load() -> Prefs {
    read(PREFERENCES)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

pub fn save(prefs: &Prefs) {
    if let Ok(json) = serde_json::to_string_pretty(prefs) {
        write(PREFERENCES, &json);
    }
}

const PREFERENCES: &str = "preferences";

const LOGOS: &str = "station-logos";

/// The listens an earlier version kept for ListenBrainz. They are dropped:
/// plays reach it through the server now.
#[cfg(not(target_family = "wasm"))]
const WAITING_LISTENS: &str = "waiting-listens";

/// Removes the listens an earlier version kept for ListenBrainz.
#[cfg(not(target_family = "wasm"))]
pub fn forget_waiting_listens() -> std::io::Result<()> {
    remove(WAITING_LISTENS)
}

/// Leaves listens for ListenBrainz behind, as an earlier version did.
#[cfg(test)]
pub(crate) fn leave_waiting_listens() {
    write(WAITING_LISTENS, "[]");
}

#[cfg(test)]
pub(crate) fn has_waiting_listens() -> bool {
    read(WAITING_LISTENS).is_some()
}

#[cfg(not(target_family = "wasm"))]
const WAITING_PLAYS: &str = "waiting-plays";

#[cfg(not(target_family = "wasm"))]
fn waiting_plays_name(account: &str) -> String {
    format!("{WAITING_PLAYS}-{account}")
}

/// The plays the server of `account` is still to be told of, oldest first.
#[cfg(not(target_family = "wasm"))]
pub fn waiting_plays(account: &str) -> Vec<crate::scrobble::WaitingPlay> {
    read(&waiting_plays_name(account))
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

#[cfg(not(target_family = "wasm"))]
pub fn save_waiting_plays(account: &str, plays: &[crate::scrobble::WaitingPlay]) {
    if let Ok(json) = serde_json::to_string(plays) {
        write(&waiting_plays_name(account), &json);
    }
}

/// Removes the plays waiting for the server of `account`.
#[cfg(not(target_family = "wasm"))]
pub fn forget_waiting_plays(account: &str) -> std::io::Result<()> {
    remove(&waiting_plays_name(account))
}

const ANSWERS_SALT: &str = "answers-salt";

/// What the names of the files the pages' answers are kept in are made with:
/// random and made once, so that a name can't confirm a guessed login.
pub fn answers_salt() -> String {
    let kept = read(ANSWERS_SALT).and_then(|json| serde_json::from_str(&json).ok());
    if let Some(salt) = kept {
        return salt;
    }
    let salt = opensubsonic::random_salt().expect("the system has randomness");
    write(
        ANSWERS_SALT,
        &serde_json::to_string(&salt).expect("strings are JSON"),
    );
    salt
}

const KEPT_LOVES: &str = "synced-loves";

/// What ListenBrainz and the server agreed on at the last sync of hearts,
/// for this ListenBrainz user and server account.
pub fn kept_loves(account: &str) -> crate::loves::Kept {
    let mut all = all_kept_loves();
    all.remove(account).unwrap_or_default()
}

pub fn save_kept_loves(account: &str, kept: &crate::loves::Kept) {
    let mut all = all_kept_loves();
    all.insert(account.to_owned(), kept.clone());
    if let Ok(json) = serde_json::to_string(&all) {
        write(KEPT_LOVES, &json);
    }
}

fn all_kept_loves() -> HashMap<String, crate::loves::Kept> {
    read(KEPT_LOVES)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// What the radio directory had for a saved station the server has no
/// picture of: the picture's address and bytes, or nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Logo {
    /// When the directory was asked, in seconds since 1970.
    pub checked: u64,
    /// Empty when the directory had no picture.
    pub address: String,
    /// The picture's bytes as hex.
    image: String,
}

/// How long a station the directory had no picture of goes unasked.
const NOT_FOUND_KEPT: u64 = 7 * 24 * 60 * 60;

impl Logo {
    pub fn found(address: String, bytes: &[u8]) -> Self {
        let mut image = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(image, "{byte:02x}");
        }
        Logo {
            checked: now(),
            address,
            image,
        }
    }

    pub fn missing() -> Self {
        Logo {
            checked: now(),
            address: String::new(),
            image: String::new(),
        }
    }

    /// The picture's bytes, or none when there is no picture.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        let hex = self.image.as_str();
        let bytes: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
            .collect();
        bytes.filter(|bytes| !bytes.is_empty())
    }

    /// Whether this is a station with no picture asked about lately enough
    /// not to ask again.
    pub fn missing_lately(&self) -> bool {
        self.image.is_empty() && now() < self.checked + NOT_FOUND_KEPT
    }
}

/// The saved stations' pictures from the directory, by stream address.
pub fn station_logos() -> HashMap<String, Logo> {
    read(LOGOS)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

pub fn save_station_logos(logos: &HashMap<String, Logo>) {
    if let Ok(json) = serde_json::to_string(logos) {
        write(LOGOS, &json);
    }
}

#[cfg(not(target_family = "wasm"))]
pub(crate) fn now() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0, |since| since.as_secs())
}

#[cfg(target_family = "wasm")]
pub(crate) fn now() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}

#[cfg(test)]
static SAVED: std::sync::Mutex<Option<HashMap<String, String>>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn read(name: &str) -> Option<String> {
    SAVED.lock().unwrap().as_ref()?.get(name).cloned()
}

#[cfg(test)]
fn write(name: &str, json: &str) {
    let mut saved = SAVED.lock().unwrap();
    saved
        .get_or_insert_default()
        .insert(name.to_owned(), json.to_owned());
}

#[cfg(test)]
fn remove(name: &str) -> std::io::Result<()> {
    if let Some(saved) = SAVED.lock().unwrap().as_mut() {
        saved.remove(name);
    }
    Ok(())
}

#[cfg(all(not(test), target_family = "wasm"))]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(all(not(test), target_family = "wasm"))]
fn read(name: &str) -> Option<String> {
    let key = format!("fonoteca-client.{name}");
    storage()?.get_item(&key).ok().flatten()
}

#[cfg(all(not(test), target_family = "wasm"))]
fn write(name: &str, json: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(&format!("fonoteca-client.{name}"), json);
    }
}

/// The app's own folder on Android, which the system hands it at start.
#[cfg(target_os = "android")]
pub static FOLDER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// `name`.json in a fonoteca-client folder in the system's settings
/// folder, or the app's own folder on Android.
#[cfg(all(not(test), not(target_family = "wasm")))]
fn file(name: &str) -> Option<std::path::PathBuf> {
    #[cfg(not(target_os = "android"))]
    use std::env::var_os;
    #[cfg(not(target_os = "android"))]
    use std::path::PathBuf;

    #[cfg(target_os = "android")]
    let folder = FOLDER.get().cloned();
    #[cfg(target_os = "macos")]
    let folder = var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(windows)]
    let folder = var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "android", windows)))]
    let folder = var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|folder| folder.is_absolute())
        .or_else(|| var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    Some(folder?.join("fonoteca-client").join(format!("{name}.json")))
}

#[cfg(all(not(test), not(target_family = "wasm")))]
fn read(name: &str) -> Option<String> {
    std::fs::read_to_string(file(name)?).ok()
}

#[cfg(all(not(test), not(target_family = "wasm")))]
fn write(name: &str, json: &str) {
    let Some(file) = file(name) else { return };
    // Written beside it and moved over it, so a crash halfway leaves the old
    // file whole.
    let partial = file.with_extension("json.partial");
    let written = file
        .parent()
        .is_some_and(|folder| std::fs::create_dir_all(folder).is_ok())
        && std::fs::write(&partial, json).is_ok();
    if written {
        let _ = std::fs::rename(&partial, &file);
    }
}

#[cfg(all(not(test), not(target_family = "wasm")))]
fn remove(name: &str) -> std::io::Result<()> {
    let Some(file) = file(name) else {
        return Ok(());
    };
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logo_keeps_its_bytes_and_none_is_asked_again_after_a_week() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(Logo::found("a".into(), &bytes).bytes(), Some(bytes));
        let mut missing = Logo::missing();
        assert_eq!(missing.bytes(), None);
        missing.checked -= 6 * 24 * 60 * 60;
        assert!(missing.missing_lately());
        missing.checked -= 2 * 24 * 60 * 60;
        assert!(!missing.missing_lately());
    }
}
