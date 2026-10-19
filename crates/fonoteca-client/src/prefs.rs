//! Preferences kept on this device: how the albums, tracks and compositions
//! are sorted and which columns the tables show. They aren't secrets, so a plain file
//! in the system's settings folder keeps them, and the browser preview its
//! localStorage. Where neither works they last until the app closes.

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
}

/// The preferences saved, or the first choices when there are none.
pub fn load() -> Prefs {
    read()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

pub fn save(prefs: &Prefs) {
    if let Ok(json) = serde_json::to_string_pretty(prefs) {
        write(&json);
    }
}

#[cfg(test)]
static SAVED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn read() -> Option<String> {
    SAVED.lock().unwrap().clone()
}

#[cfg(test)]
fn write(json: &str) {
    *SAVED.lock().unwrap() = Some(json.to_owned());
}

#[cfg(all(not(test), target_family = "wasm"))]
const KEY: &str = "fonoteca-client.preferences";

#[cfg(all(not(test), target_family = "wasm"))]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(all(not(test), target_family = "wasm"))]
fn read() -> Option<String> {
    storage()?.get_item(KEY).ok().flatten()
}

#[cfg(all(not(test), target_family = "wasm"))]
fn write(json: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(KEY, json);
    }
}

/// The app's own folder on Android, which the system hands it at start.
#[cfg(target_os = "android")]
pub static FOLDER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// preferences.json in a fonoteca-client folder in the system's settings
/// folder, or the app's own folder on Android.
#[cfg(all(not(test), not(target_family = "wasm")))]
fn file() -> Option<std::path::PathBuf> {
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
    Some(folder?.join("fonoteca-client").join("preferences.json"))
}

#[cfg(all(not(test), not(target_family = "wasm")))]
fn read() -> Option<String> {
    std::fs::read_to_string(file()?).ok()
}

#[cfg(all(not(test), not(target_family = "wasm")))]
fn write(json: &str) {
    let Some(file) = file() else { return };
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
