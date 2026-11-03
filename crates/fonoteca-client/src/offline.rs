//! Music kept on this device for use without a connection: what the user
//! asked to keep, which is the manifest, and the song files that arrived.
//! The manifest says what is wanted and the files say what is finished, so a
//! song finishing is a file renamed into place and nothing is rewritten.
//!
//! The browser has no folder to keep them in; there nothing is ever kept.

// The browser keeps nothing, so there only the empty handle is used.
#![cfg_attr(target_family = "wasm", allow(dead_code))]

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard};

use opensubsonic::{AlbumID3WithSongs, Child};
use serde::{Deserialize, Serialize};

const MANIFEST: &str = "index.json";
const SONGS: &str = "songs";
/// Tells backup tools that what is in the folder can be fetched again.
const CACHEDIR_TAG: &str = "CACHEDIR.TAG";
const CACHEDIR_TAG_CONTENT: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# Songs downloaded from the server; they can be fetched again.\n";
/// The tag of a file that is the song as the server keeps it.
pub const ORIGINAL: &str = "original";
/// What a song's file is called while it arrives.
const PARTIAL: &str = "partial";
/// For a song whose file name gives no extension.
const UNKNOWN_EXTENSION: &str = "audio";

/// Something the user asked to keep, with what it was made of when asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Item {
    Album(AlbumID3WithSongs),
}

impl Item {
    /// The link of the page that shows it.
    pub fn link(&self) -> String {
        match self {
            Item::Album(album) => format!("album:{}", album.album.id),
        }
    }

    pub fn songs(&self) -> &[Child] {
        match self {
            Item::Album(album) => &album.song,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    /// The server the songs are from; another server's account shows none.
    server: String,
    items: Vec<Item>,
}

/// A song's finished file.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Default)]
struct State {
    folder: Option<PathBuf>,
    manifest: Manifest,
    present: HashMap<String, Saved>,
    /// Songs the server refused, with why, and why everything stopped, until
    /// the next thing is asked for.
    failed: HashMap<String, String>,
    halted: Option<String>,
    /// The song being fetched and the flag that stops it.
    running: Option<(String, Arc<AtomicBool>)>,
    attempt: usize,
}

/// What is kept on this device, shared by the pages, the player and the job
/// that fetches songs.
#[derive(Clone, Default)]
pub struct Downloads(Arc<Mutex<State>>);

/// The folder downloads live in: the app's own data folder, which cleaners
/// of the cache leave alone.
#[cfg(not(any(target_family = "wasm", test)))]
pub fn folder() -> Option<PathBuf> {
    #[cfg(not(target_os = "android"))]
    use std::env::var_os;

    #[cfg(target_os = "android")]
    let folder = crate::prefs::FOLDER.get().cloned();
    #[cfg(target_os = "macos")]
    let folder = var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(windows)]
    let folder = var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "android", windows)))]
    let folder = var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|folder| folder.is_absolute())
        .or_else(|| var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    Some(folder?.join("fonoteca-client").join("downloads"))
}

#[cfg(target_family = "wasm")]
pub fn folder() -> Option<PathBuf> {
    None
}

/// Each test process keeps its own, which it removes.
#[cfg(test)]
pub fn folder() -> Option<PathBuf> {
    Some(std::env::temp_dir().join(format!("fonoteca-downloads-{}", std::process::id())))
}

/// `id`'s file name: the id as hex, since ids can hold anything, the tag
/// for how it was fetched and the extension.
fn file_name(id: &str, tag: &str, extension: &str) -> String {
    let hex: String = id.bytes().map(|byte| format!("{byte:02x}")).collect();
    format!("{hex}.{tag}.{extension}")
}

/// The song id and tag a finished file's name stands for.
fn parse_name(name: &str) -> Option<(String, &str)> {
    let mut parts = name.split('.');
    let (hex, tag, extension) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || extension == PARTIAL || !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect();
    Some((String::from_utf8(bytes?).ok()?, tag))
}

/// The extension a song's file gets: the song's own suffix, kept to
/// letters and digits, since it ends up in a file name.
fn extension_of(song: &Child) -> String {
    song.suffix
        .as_deref()
        .map(|suffix| suffix.to_ascii_lowercase())
        .filter(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| UNKNOWN_EXTENSION.to_owned())
}

/// The songs of `items` without a finished file, in the order they were
/// asked for, each once.
fn missing<'a>(items: &'a [Item], present: &HashMap<String, Saved>) -> Vec<&'a Child> {
    let mut seen = std::collections::HashSet::new();
    items
        .iter()
        .flat_map(Item::songs)
        .filter(|song| !present.contains_key(&song.id) && seen.insert(song.id.as_str()))
        .collect()
}

/// The ids of finished songs no item wants any more.
fn orphans(items: &[Item], present: &HashMap<String, Saved>) -> Vec<String> {
    let wanted: std::collections::HashSet<&str> = items
        .iter()
        .flat_map(Item::songs)
        .map(|song| song.id.as_str())
        .collect();
    present
        .keys()
        .filter(|id| !wanted.contains(id.as_str()))
        .cloned()
        .collect()
}

/// How many of `songs` have a finished file, and how many there are.
fn progress(songs: &[Child], present: &HashMap<String, Saved>) -> (usize, usize) {
    let done = songs
        .iter()
        .filter(|song| present.contains_key(&song.id))
        .count();
    (done, songs.len())
}

impl Downloads {
    /// What the folder holds. A manifest that can't be read is set aside,
    /// not overwritten, and the downloads start over from the songs found.
    pub fn load() -> Self {
        let downloads = Downloads::default();
        let Some(folder) = folder() else {
            return downloads;
        };
        {
            let mut state = downloads.state();
            match read_manifest(&folder) {
                Ok(manifest) => state.manifest = manifest,
                Err(e) => {
                    let aside = folder.join(format!("{MANIFEST}.unreadable"));
                    eprintln!(
                        "The downloads' list is unreadable ({e}); set aside as {}",
                        aside.display()
                    );
                    if let Err(e) = std::fs::rename(folder.join(MANIFEST), &aside) {
                        eprintln!("It couldn't be set aside: {e}");
                    }
                }
            }
            state.present = scan(&folder.join(SONGS));
            state.folder = Some(folder);
        }
        downloads
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    /// The finished file of song `id`.
    pub fn local(&self, id: &str) -> Option<PathBuf> {
        self.state().present.get(id).map(|saved| saved.path.clone())
    }

    /// How many songs of the item shown at `link` have arrived, and how many
    /// it has; none when it wasn't asked for.
    pub fn progress_of(&self, link: &str) -> Option<(usize, usize)> {
        let state = self.state();
        let item = state
            .manifest
            .items
            .iter()
            .find(|item| item.link() == link)?;
        Some(progress(item.songs(), &state.present))
    }

    /// Asks for `item`, from `server`. Anything that stopped is tried again.
    pub fn add(&self, server: &str, item: Item) -> io::Result<()> {
        let mut state = self.state();
        let Some(folder) = state.folder.clone() else {
            return Err(io::Error::other(
                "This device has nowhere to keep downloads.",
            ));
        };
        if state.manifest.server != server {
            state.manifest = Manifest {
                server: server.to_owned(),
                items: Vec::new(),
            };
        }
        let link = item.link();
        state.manifest.items.retain(|kept| kept.link() != link);
        state.manifest.items.push(item);
        state.failed.clear();
        state.halted = None;
        state.attempt = 0;
        prepare(&folder)?;
        write_manifest(&folder, &state.manifest)
    }

    /// Lets go of the item at `link` and deletes the songs no other item
    /// wants.
    pub fn remove(&self, link: &str) -> io::Result<()> {
        let mut state = self.state();
        state.manifest.items.retain(|item| item.link() != link);
        let gone = orphans(&state.manifest.items, &state.present);
        let mut problem = None;
        for id in gone {
            if let Some(saved) = state.present.remove(&id)
                && let Err(e) = std::fs::remove_file(&saved.path)
            {
                problem = Some(e);
            }
        }
        let wanted = state.manifest.items.iter().flat_map(Item::songs);
        if let Some((id, cancel)) = &state.running
            && !wanted.into_iter().any(|song| &song.id == id)
        {
            cancel.store(true, Relaxed);
        }
        if let Some(folder) = state.folder.clone() {
            write_manifest(&folder, &state.manifest)?;
        }
        problem.map_or(Ok(()), Err)
    }

    /// The next song to fetch, and where its file goes while it arrives and
    /// when it is whole, with the flag that stops the fetch. None while a
    /// song is being fetched, or after a stop, or with none left.
    pub fn start_next(&self) -> Option<Fetch> {
        let mut state = self.state();
        if state.running.is_some() || state.halted.is_some() {
            return None;
        }
        let folder = state.folder.clone()?.join(SONGS);
        let song = missing(&state.manifest.items, &state.present)
            .into_iter()
            .find(|song| !state.failed.contains_key(&song.id))?
            .clone();
        let cancel = Arc::new(AtomicBool::new(false));
        state.running = Some((song.id.clone(), cancel.clone()));
        let name = file_name(&song.id, ORIGINAL, &extension_of(&song));
        Some(Fetch {
            partial: folder.join(format!("{name}.{PARTIAL}")),
            path: folder.join(name),
            song,
            cancel,
        })
    }

    /// The fetch of song `id` ended with its whole file at `path`.
    pub fn finished(&self, id: &str, path: PathBuf) -> io::Result<()> {
        let mut state = self.state();
        state.running = None;
        state.attempt = 0;
        let size = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            // Everything was removed while the song arrived: nobody wants it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let wanted = state
            .manifest
            .items
            .iter()
            .flat_map(Item::songs)
            .any(|song| song.id == id);
        if wanted {
            state.present.insert(id.to_owned(), Saved { path, size });
            return Ok(());
        }
        std::fs::remove_file(path)
    }

    /// The fetch ended without the song: stopped because it isn't wanted
    /// any more, or failed.
    pub fn ended(&self) {
        self.state().running = None;
    }

    /// Counts a failed try of the song being fetched; how many there have
    /// been since one worked.
    pub fn failed_try(&self) -> usize {
        let mut state = self.state();
        state.attempt += 1;
        state.attempt
    }

    /// Leaves out song `id`, which the server won't give, until something
    /// else is asked for.
    pub fn give_up(&self, id: &str, reason: String) {
        self.state().failed.insert(id.to_owned(), reason);
    }

    /// Stops fetching for `reason`, until something else is asked for.
    pub fn halt(&self, reason: String) {
        self.state().halted = Some(reason);
    }

    /// Deletes what a stopped fetch left half done.
    pub fn clear_partials(&self) {
        let Some(folder) = self.state().folder.clone() else {
            return;
        };
        for entry in std::fs::read_dir(folder.join(SONGS))
            .into_iter()
            .flatten()
            .flatten()
        {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == PARTIAL)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// What the job needs to fetch one song.
pub struct Fetch {
    pub song: Child,
    pub partial: PathBuf,
    pub path: PathBuf,
    pub cancel: Arc<AtomicBool>,
}

fn prepare(folder: &Path) -> io::Result<()> {
    std::fs::create_dir_all(folder.join(SONGS))?;
    let tag = folder.join(CACHEDIR_TAG);
    if !tag.exists() {
        std::fs::write(tag, CACHEDIR_TAG_CONTENT)?;
    }
    Ok(())
}

fn read_manifest(folder: &Path) -> io::Result<Manifest> {
    match std::fs::read(folder.join(MANIFEST)) {
        Ok(json) => Ok(serde_json::from_slice(&json)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(e) => Err(e),
    }
}

/// Written beside the manifest and moved over it, so a crash halfway leaves
/// the old one whole.
fn write_manifest(folder: &Path, manifest: &Manifest) -> io::Result<()> {
    let partial = folder.join(format!("{MANIFEST}.{PARTIAL}"));
    std::fs::write(&partial, serde_json::to_vec(manifest)?)?;
    std::fs::rename(partial, folder.join(MANIFEST))
}

/// The finished songs in `folder`, by song id.
fn scan(folder: &Path) -> HashMap<String, Saved> {
    let mut present = HashMap::new();
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some((id, _)) = name.to_str().and_then(parse_name) else {
            continue;
        };
        if let Ok(metadata) = entry.metadata() {
            present.insert(
                id,
                Saved {
                    path: entry.path(),
                    size: metadata.len(),
                },
            );
        }
    }
    present
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, suffix: Option<&str>) -> Child {
        serde_json::from_value(serde_json::json!({
            "id": id, "isDir": false, "title": id, "suffix": suffix,
        }))
        .unwrap()
    }

    fn album(id: &str, songs: &[&str]) -> Item {
        let mut album: AlbumID3WithSongs = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "songCount": songs.len(), "duration": 0,
            "created": "2020-01-01T00:00:00Z",
        }))
        .unwrap();
        album.song = songs.iter().map(|id| song(id, Some("flac"))).collect();
        Item::Album(album)
    }

    fn saved(ids: &[&str]) -> HashMap<String, Saved> {
        ids.iter()
            .map(|id| {
                let saved = Saved {
                    path: PathBuf::from(id),
                    size: 1,
                };
                (id.to_string(), saved)
            })
            .collect()
    }

    #[test]
    fn a_file_name_gives_back_the_id_it_was_made_from() {
        let name = file_name("al/1 é.x", ORIGINAL, "flac");
        assert_eq!(parse_name(&name), Some(("al/1 é.x".to_owned(), ORIGINAL)));
        assert_eq!(
            parse_name(&format!("{name}.{PARTIAL}")),
            None,
            "a song still arriving isn't finished"
        );
        assert_eq!(parse_name("CACHEDIR.TAG"), None);
        assert_eq!(parse_name("zz.original.flac"), None);
    }

    #[test]
    fn an_extension_is_the_suffix_when_it_is_safe_to_use() {
        assert_eq!(extension_of(&song("a", Some("FLAC"))), "flac");
        assert_eq!(extension_of(&song("a", Some("../x"))), UNKNOWN_EXTENSION);
        assert_eq!(extension_of(&song("a", Some(""))), UNKNOWN_EXTENSION);
        assert_eq!(extension_of(&song("a", None)), UNKNOWN_EXTENSION);
    }

    #[test]
    fn missing_songs_come_in_the_order_asked_for_each_once() {
        let items = [album("one", &["a", "b", "c"]), album("two", &["c", "d"])];
        let ids = |present: &[&str]| -> Vec<String> {
            missing(&items, &saved(present))
                .into_iter()
                .map(|song| song.id.clone())
                .collect()
        };
        assert_eq!(ids(&[]), ["a", "b", "c", "d"]);
        assert_eq!(ids(&["b", "c"]), ["a", "d"]);
        assert!(ids(&["a", "b", "c", "d"]).is_empty());
    }

    #[test]
    fn a_song_two_items_want_stays_when_one_goes() {
        let present = saved(&["a", "b", "c", "x"]);
        let both = [album("one", &["a", "b"]), album("two", &["b", "c"])];
        assert_eq!(orphans(&both, &present), ["x"]);
        let mut gone = orphans(&both[1..], &present);
        gone.sort();
        assert_eq!(gone, ["a", "x"]);
        assert_eq!(progress(both[0].songs(), &present), (2, 2));
        assert_eq!(progress(both[0].songs(), &saved(&["a"])), (1, 2));
    }

    #[test]
    fn a_song_that_arrives_after_everything_was_removed_is_nobodys() {
        let downloads = Downloads::default();
        downloads.state().running = Some(("a".to_owned(), Arc::default()));
        let gone = std::env::temp_dir().join("offline-test-never-there/a.original.flac");
        assert!(downloads.finished("a", gone).is_ok());
        assert!(
            downloads.state().running.is_none(),
            "the next song can start"
        );
        assert!(downloads.local("a").is_none());
    }

    #[test]
    fn the_manifest_keeps_what_was_asked_for_and_the_files_what_arrived() {
        let folder = std::env::temp_dir().join(format!("offline-test-{}", std::process::id()));
        let item = album("one", &["a", "b"]);
        std::fs::create_dir_all(folder.join(SONGS)).unwrap();
        write_manifest(
            &folder,
            &Manifest {
                server: "http://s".into(),
                items: vec![item.clone()],
            },
        )
        .unwrap();
        std::fs::write(
            folder.join(SONGS).join(file_name("a", ORIGINAL, "flac")),
            b"xyz",
        )
        .unwrap();
        std::fs::write(
            folder
                .join(SONGS)
                .join(format!("{}.{PARTIAL}", file_name("b", ORIGINAL, "flac"))),
            b"x",
        )
        .unwrap();

        let manifest = read_manifest(&folder).unwrap();
        assert_eq!(manifest.items, [item]);
        let present = scan(&folder.join(SONGS));
        assert_eq!(present.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(present["a"].size, 3);
        assert!(
            read_manifest(&folder.join("nowhere"))
                .unwrap()
                .items
                .is_empty()
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
