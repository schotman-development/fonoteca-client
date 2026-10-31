//! Preferences kept on this device: how the albums, tracks, compositions and
//! folders are sorted, which columns the tables show, the radio page's
//! languages, and the sound device and volume leveling; and the pictures of saved stations the app found in the radio
//! directory. They aren't secrets, so plain files in the system's settings
//! folder keep them, and the browser preview its localStorage. Where neither
//! works they last until the app closes.

use std::collections::HashMap;
use std::fmt::Write;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The albums page's Sort menu entry and direction.
    pub album_sort: i32,
    pub album_descending: bool,
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
fn now() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0, |since| since.as_secs())
}

#[cfg(target_family = "wasm")]
fn now() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}

#[cfg(test)]
static SAVED: std::sync::Mutex<Option<HashMap<&str, String>>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn read(name: &str) -> Option<String> {
    SAVED.lock().unwrap().as_ref()?.get(name).cloned()
}

#[cfg(test)]
fn write(name: &'static str, json: &str) {
    let mut saved = SAVED.lock().unwrap();
    saved.get_or_insert_default().insert(name, json.to_owned());
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
