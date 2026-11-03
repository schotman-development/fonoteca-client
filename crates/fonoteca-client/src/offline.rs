//! Music kept on this device for use without a connection: what the user
//! asked to keep, which is the manifest, and the song files that arrived.
//! The manifest says what is wanted and the files say what is finished, so a
//! song finishing is a file renamed into place and nothing is rewritten.
//!
//! The browser has no folder to keep them in; there nothing is ever kept.

// The browser keeps nothing, so there only the empty handle is used.
#![cfg_attr(target_family = "wasm", allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard};

use opensubsonic::{AlbumID3WithSongs, Child, PlaylistWithSongs};
use serde::{Deserialize, Serialize};

const MANIFEST: &str = "index.json";
const SONGS: &str = "songs";
const COVERS: &str = "covers";
/// How wide a cover is kept, in pixels; one size serves every tile and
/// header.
pub const COVER_SIZE: u32 = 1000;
/// Tells backup tools that what is in the folder can be fetched again.
const CACHEDIR_TAG: &str = "CACHEDIR.TAG";
const CACHEDIR_TAG_CONTENT: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# Songs downloaded from the server; they can be fetched again.\n";
/// The tag of a file that is the song as the server keeps it.
pub const ORIGINAL: &str = "original";
/// What a song's file is called while it arrives.
const PARTIAL: &str = "partial";
/// For a song whose file name gives no extension.
const UNKNOWN_EXTENSION: &str = "audio";
/// Why downloading stopped when the size limit is reached.
pub const LIMIT_REACHED: &str = "Downloads stopped: the size limit is reached.";
/// The size limits the settings offer, in GB, in the order of its list; 0
/// is none.
pub const LIMITS_GB: [u64; 8] = [0, 1, 5, 10, 25, 50, 100, 250];
const BYTES_PER_MB: f64 = 1_048_576.0;
const BYTES_PER_GB: u64 = 1_073_741_824;

/// Something the user asked to keep, with what it was made of when asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// There are few items and they live as long as the list, so boxing the big
// ones would only add indirection.
#[allow(clippy::large_enum_variant)]
pub enum Item {
    Album(AlbumID3WithSongs),
    Playlist(PlaylistWithSongs),
    /// A song asked for on its own.
    Song(Box<Child>),
}

impl Item {
    /// The link of the page that shows it.
    pub fn link(&self) -> String {
        match self {
            Item::Album(album) => format!("album:{}", album.album.id),
            Item::Playlist(playlist) => format!("playlist:{}", playlist.playlist.id),
            Item::Song(song) => format!("song:{}", song.id),
        }
    }

    /// The id of its cover art.
    fn cover_art(&self) -> Option<&str> {
        match self {
            Item::Album(album) => album.album.cover_art.as_deref(),
            Item::Playlist(playlist) => playlist.playlist.cover_art.as_deref(),
            Item::Song(song) => song.cover_art.as_deref(),
        }
        .filter(|id| !id.is_empty())
    }

    pub fn songs(&self) -> &[Child] {
        match self {
            Item::Album(album) => &album.song,
            Item::Playlist(playlist) => &playlist.entry,
            Item::Song(song) => std::slice::from_ref(song.as_ref()),
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
    /// The ids of the covers whose files are there.
    covers: HashSet<String>,
    /// Songs the server refused, with why, and why everything stopped, until
    /// the next thing is asked for.
    failed: HashMap<String, String>,
    halted: Option<String>,
    /// The most the songs may take, if the user set one.
    limit: Option<u64>,
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

/// `id` as hex, to name a file by it, since ids can hold anything.
fn hex(id: &str) -> String {
    id.bytes().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(hex: &str) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// A song's file name: its id, the tag for how it was fetched and the
/// extension.
fn file_name(id: &str, tag: &str, extension: &str) -> String {
    format!("{}.{tag}.{extension}", hex(id))
}

/// The song id and tag a finished file's name stands for.
fn parse_name(name: &str) -> Option<(String, &str)> {
    let mut parts = name.split('.');
    let (hex, tag, extension) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || extension == PARTIAL {
        return None;
    }
    Some((unhex(hex)?, tag))
}

/// The ids of the covers `items` show, each once, in order.
fn wanted_covers(items: &[Item]) -> Vec<&str> {
    let mut seen = HashSet::new();
    items
        .iter()
        .filter_map(Item::cover_art)
        .filter(|id| seen.insert(*id))
        .collect()
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

/// The size limit the settings' `index`th entry stands for.
pub fn limit_of(index: usize) -> Option<u64> {
    LIMITS_GB
        .get(index)
        .filter(|&&gigabytes| gigabytes > 0)
        .map(|gigabytes| gigabytes * BYTES_PER_GB)
}

/// Whether a song of `size` bytes, when the server says, still fits in
/// `limit` with `used` already kept.
fn fits(used: u64, size: Option<i64>, limit: Option<u64>) -> bool {
    let size = u64::try_from(size.unwrap_or(0)).unwrap_or(0);
    limit.is_none_or(|limit| used + size <= limit)
}

/// `bytes` in the units of the tracks table's Size column.
fn bytes_text(bytes: u64) -> String {
    let megabytes = bytes as f64 / BYTES_PER_MB;
    if megabytes < 1024.0 {
        format!("{megabytes:.1} MB")
    } else {
        format!("{:.1} GB", megabytes / 1024.0)
    }
}

/// What the downloads' settings say about them.
pub struct Summary {
    pub used: u64,
    pub songs: usize,
    /// How many songs asked for have yet to arrive.
    pub missing: usize,
    /// Why nothing is arriving, when something should.
    pub halted: Option<String>,
}

impl Summary {
    pub fn space(&self) -> String {
        match self.songs {
            0 => "Nothing is downloaded.".to_owned(),
            1 => format!("{} used by 1 song.", bytes_text(self.used)),
            songs => format!("{} used by {songs} songs.", bytes_text(self.used)),
        }
    }

    pub fn status(&self) -> String {
        match (&self.halted, self.missing) {
            (Some(reason), _) => reason.clone(),
            (None, 0) => String::new(),
            (None, missing) => format!("{missing} to go."),
        }
    }
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

/// How many bytes the finished files take.
fn used(present: &HashMap<String, Saved>) -> u64 {
    present.values().map(|saved| saved.size).sum()
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
            state.covers = scan_covers(&folder.join(COVERS));
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

    /// Whether all of the item shown at `link` has arrived.
    pub fn is_kept(&self, link: &str) -> bool {
        self.progress_of(link)
            .is_some_and(|(done, total)| done == total)
    }

    /// Whether the song was asked for on its own, not only as part of an
    /// album or a playlist.
    pub fn is_kept_alone(&self, id: &str) -> bool {
        self.is_kept(&format!("song:{id}"))
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
        let unwanted: Vec<String> = {
            let wanted = wanted_covers(&state.manifest.items);
            let mut unwanted: Vec<String> = state
                .covers
                .iter()
                .filter(|id| !wanted.contains(&id.as_str()))
                .cloned()
                .collect();
            unwanted.sort();
            unwanted
        };
        if let Some(folder) = state.folder.clone() {
            for id in unwanted {
                state.covers.remove(&id);
                if let Err(e) = std::fs::remove_file(folder.join(COVERS).join(hex(&id))) {
                    problem = Some(e);
                }
            }
            write_manifest(&folder, &state.manifest)?;
        }
        problem.map_or(Ok(()), Err)
    }

    /// The next song to fetch, and where its file goes while it arrives and
    /// when it is whole, with the flag that stops the fetch. None while a
    /// song is being fetched, or after a stop, or with none left. The reason
    /// when the size limit stops it, once.
    pub fn start_next(&self) -> Result<Option<Fetch>, String> {
        let mut state = self.state();
        if state.running.is_some() || state.halted.is_some() {
            return Ok(None);
        }
        let Some(folder) = state.folder.clone() else {
            return Ok(None);
        };
        // The covers first: they are small, and the pages need them at once.
        let cover = wanted_covers(&state.manifest.items)
            .into_iter()
            .find(|id| !state.covers.contains(*id) && !state.failed.contains_key(&cover_key(id)))
            .map(str::to_owned);
        if let Some(id) = cover {
            let cancel = Arc::new(AtomicBool::new(false));
            state.running = Some((cover_key(&id), cancel.clone()));
            let path = folder.join(COVERS).join(hex(&id));
            return Ok(Some(Fetch {
                partial: path.with_extension(PARTIAL),
                path,
                work: Work::Cover(id),
                room: None,
                cancel,
            }));
        }
        let folder = folder.join(SONGS);
        let Some(song) = missing(&state.manifest.items, &state.present)
            .into_iter()
            .find(|song| !state.failed.contains_key(&song.id))
            .cloned()
        else {
            return Ok(None);
        };
        let used = used(&state.present);
        if !fits(used, song.size, state.limit) {
            state.halted = Some(LIMIT_REACHED.to_owned());
            return Err(LIMIT_REACHED.to_owned());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.running = Some((song.id.clone(), cancel.clone()));
        let name = file_name(&song.id, ORIGINAL, &extension_of(&song));
        Ok(Some(Fetch {
            partial: folder.join(format!("{name}.{PARTIAL}")),
            path: folder.join(name),
            room: state.limit.map(|limit| limit.saturating_sub(used)),
            work: Work::Song(Box::new(song)),
            cancel,
        }))
    }

    /// Sets the most the songs may take; songs that stopped for the old
    /// one are fetched again.
    pub fn set_limit(&self, limit: Option<u64>) {
        let mut state = self.state();
        state.limit = limit;
        state.halted = None;
    }

    pub fn summary(&self) -> Summary {
        let state = self.state();
        Summary {
            used: used(&state.present),
            songs: state.present.len(),
            missing: missing(&state.manifest.items, &state.present).len(),
            halted: state.halted.clone(),
        }
    }

    /// Lets go of everything and deletes the folder.
    pub fn remove_all(&self) -> io::Result<()> {
        let mut state = self.state();
        state.manifest.items.clear();
        state.present.clear();
        state.covers.clear();
        if let Some((_, cancel)) = &state.running {
            cancel.store(true, Relaxed);
        }
        let Some(folder) = &state.folder else {
            return Ok(());
        };
        match std::fs::remove_dir_all(folder) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The fetch of `work` ended with its whole file at `path`.
    pub fn finished(&self, work: &Work, path: PathBuf) -> io::Result<()> {
        let mut state = self.state();
        state.running = None;
        state.attempt = 0;
        let size = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            // Everything was removed while it arrived: nobody wants it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let wanted = match work {
            Work::Song(song) => {
                let mut songs = state.manifest.items.iter().flat_map(Item::songs);
                songs.any(|wanted| wanted.id == song.id)
            }
            Work::Cover(id) => wanted_covers(&state.manifest.items).contains(&id.as_str()),
        };
        if !wanted {
            return std::fs::remove_file(path);
        }
        match work {
            Work::Song(song) => {
                state.present.insert(song.id.clone(), Saved { path, size });
            }
            Work::Cover(id) => {
                state.covers.insert(id.clone());
            }
        }
        Ok(())
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

    /// Leaves out `work`, which the server won't give, until something else
    /// is asked for.
    pub fn give_up(&self, work: &Work, reason: String) {
        self.state().failed.insert(work.key(), reason);
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
        for folder in [folder.join(SONGS), folder.join(COVERS)] {
            for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
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

    /// The album kept at `id`, as the server had it.
    pub fn album(&self, id: &str) -> Option<AlbumID3WithSongs> {
        self.state()
            .manifest
            .items
            .iter()
            .find_map(|item| match item {
                Item::Album(album) if album.album.id == id => Some(album.clone()),
                _ => None,
            })
    }

    pub fn playlist(&self, id: &str) -> Option<PlaylistWithSongs> {
        self.state()
            .manifest
            .items
            .iter()
            .find_map(|item| match item {
                Item::Playlist(playlist) if playlist.playlist.id == id => Some(playlist.clone()),
                _ => None,
            })
    }

    /// A song in any item kept, as the server had it.
    pub fn song(&self, id: &str) -> Option<Child> {
        let state = self.state();
        let mut songs = state.manifest.items.iter().flat_map(Item::songs);
        songs.find(|song| song.id == id).cloned()
    }

    /// The albums, playlists and songs kept, in the order they were asked
    /// for.
    pub fn items(&self) -> Vec<Item> {
        self.state().manifest.items.clone()
    }

    /// Whether something from `server` is kept.
    pub fn has_songs_from(&self, server: &str) -> bool {
        let state = self.state();
        state.manifest.server == server && !state.manifest.items.is_empty()
    }

    /// Whether anything is kept.
    pub fn any(&self) -> bool {
        !self.state().manifest.items.is_empty()
    }

    /// The cover kept for the cover art `id`.
    pub fn cover(&self, id: &str) -> Option<Vec<u8>> {
        let state = self.state();
        if !state.covers.contains(id) {
            return None;
        }
        std::fs::read(state.folder.as_ref()?.join(COVERS).join(hex(id))).ok()
    }
}

fn cover_key(id: &str) -> String {
    format!("cover:{id}")
}

/// What there is to fetch.
#[derive(Clone)]
pub enum Work {
    Song(Box<Child>),
    /// The cover art with this id.
    Cover(String),
}

impl Work {
    fn key(&self) -> String {
        match self {
            Work::Song(song) => song.id.clone(),
            Work::Cover(id) => cover_key(id),
        }
    }
}

/// What the job needs to fetch one thing.
pub struct Fetch {
    pub work: Work,
    pub partial: PathBuf,
    pub path: PathBuf,
    /// How many bytes may arrive before the size limit is passed, whatever
    /// size the server said the song has.
    pub room: Option<u64>,
    pub cancel: Arc<AtomicBool>,
}

fn prepare(folder: &Path) -> io::Result<()> {
    std::fs::create_dir_all(folder.join(SONGS))?;
    std::fs::create_dir_all(folder.join(COVERS))?;
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

/// The ids of the finished covers in `folder`.
fn scan_covers(folder: &Path) -> HashSet<String> {
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| unhex(entry.file_name().to_str()?))
        .collect()
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
        Item::Album(album_with_songs(id, songs))
    }

    fn album_with_songs(id: &str, songs: &[&str]) -> AlbumID3WithSongs {
        let mut album: AlbumID3WithSongs = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "songCount": songs.len(), "duration": 0,
            "created": "2020-01-01T00:00:00Z",
        }))
        .unwrap();
        album.song = songs.iter().map(|id| song(id, Some("flac"))).collect();
        album
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
        let work = Work::Song(Box::new(song("a", None)));
        assert!(downloads.finished(&work, gone).is_ok());
        assert!(
            downloads.state().running.is_none(),
            "the next song can start"
        );
        assert!(downloads.local("a").is_none());
    }

    #[test]
    fn every_kind_of_item_has_its_page_and_its_songs() {
        let playlist: PlaylistWithSongs = serde_json::from_value(serde_json::json!({
            "id": "p1", "name": "Mix", "songCount": 1, "duration": 0,
            "created": "2020-01-01T00:00:00Z", "changed": "2020-01-01T00:00:00Z",
            "entry": [{"id": "a", "isDir": false, "title": "a"}],
        }))
        .unwrap();
        let items = [
            album("one", &["a", "b"]),
            Item::Playlist(playlist),
            Item::Song(Box::new(song("a", None))),
        ];
        assert_eq!(
            items.each_ref().map(Item::link),
            ["album:one", "playlist:p1", "song:a"]
        );
        assert_eq!(items.each_ref().map(|item| item.songs().len()), [2, 1, 1]);
        // One song wanted by all three is kept until the last goes.
        let present = saved(&["a", "b"]);
        assert!(orphans(&items, &present).is_empty());
        assert_eq!(orphans(&items[1..], &present), ["b"]);
        assert!(orphans(&items[2..], &present).contains(&"b".to_owned()));
    }

    #[test]
    fn letting_go_deletes_the_songs_and_covers_nobody_else_wants() {
        let folder = std::env::temp_dir().join(format!("let-go-test-{}", std::process::id()));
        prepare(&folder).unwrap();
        let with_cover = |id: &str, songs: &[&str], cover: &str| {
            let mut album = album_with_songs(id, songs);
            album.album.cover_art = Some(cover.to_owned());
            Item::Album(album)
        };
        let (one, two) = (
            with_cover("one", &["a", "b"], "c1"),
            with_cover("two", &["b", "c"], "c2"),
        );
        assert_eq!(wanted_covers(&[one.clone(), one.clone()]), ["c1"]);
        for id in ["a", "b", "c"] {
            std::fs::write(
                folder.join(SONGS).join(file_name(id, ORIGINAL, "flac")),
                b"x",
            )
            .unwrap();
        }
        for id in ["c1", "c2"] {
            std::fs::write(folder.join(COVERS).join(hex(id)), b"x").unwrap();
        }
        let downloads = Downloads::default();
        {
            let mut state = downloads.state();
            state.manifest.items = vec![one, two.clone()];
            state.present = scan(&folder.join(SONGS));
            state.covers = scan_covers(&folder.join(COVERS));
            state.folder = Some(folder.clone());
        }
        assert_eq!(downloads.cover("c1"), Some(b"x".to_vec()));
        downloads.remove("album:one").unwrap();
        let mut left: Vec<String> = downloads.state().present.keys().cloned().collect();
        left.sort();
        assert_eq!(left, ["b", "c"]);
        assert_eq!(downloads.cover("c1"), None);
        assert_eq!(downloads.cover("c2"), Some(b"x".to_vec()));
        assert_eq!(scan(&folder.join(SONGS)).len(), 2);
        assert_eq!(
            scan_covers(&folder.join(COVERS)),
            HashSet::from(["c2".to_owned()])
        );
        assert_eq!(read_manifest(&folder).unwrap().items, [two]);
        assert!(downloads.album("two").is_some() && downloads.album("one").is_none());
        assert_eq!(
            downloads.song("c").map(|song| song.id),
            Some("c".to_owned())
        );
        downloads.remove_all().unwrap();
        assert!(!folder.exists());
    }

    #[test]
    fn a_song_fits_until_it_would_pass_the_limit() {
        let gigabyte = i64::try_from(BYTES_PER_GB).unwrap();
        assert!(fits(u64::MAX / 2, Some(gigabyte), None));
        assert!(fits(0, None, Some(BYTES_PER_GB)));
        assert!(fits(BYTES_PER_GB - 10, Some(10), Some(BYTES_PER_GB)));
        assert!(!fits(BYTES_PER_GB - 10, Some(11), Some(BYTES_PER_GB)));
        assert_eq!(limit_of(0), None);
        assert_eq!(limit_of(2), Some(5 * BYTES_PER_GB));
        assert_eq!(limit_of(LIMITS_GB.len()), None);
    }

    #[test]
    fn sizes_are_told_in_the_units_of_the_size_column() {
        assert_eq!(bytes_text(0), "0.0 MB");
        assert_eq!(bytes_text(1_572_864), "1.5 MB");
        assert_eq!(bytes_text(BYTES_PER_GB), "1.0 GB");
        let summary = |songs, missing, halted: Option<&str>| Summary {
            used: 3 * BYTES_PER_GB,
            songs,
            missing,
            halted: halted.map(str::to_owned),
        };
        assert_eq!(summary(0, 0, None).space(), "Nothing is downloaded.");
        assert_eq!(summary(1, 0, None).space(), "3.0 GB used by 1 song.");
        assert_eq!(summary(14, 0, None).space(), "3.0 GB used by 14 songs.");
        assert_eq!(summary(14, 0, None).status(), "");
        assert_eq!(summary(14, 3, None).status(), "3 to go.");
        assert_eq!(summary(14, 3, Some("Full")).status(), "Full");
    }

    #[test]
    fn the_limit_stops_the_next_song_once_and_lifting_it_goes_on() {
        let folder = std::env::temp_dir().join(format!("limit-test-{}", std::process::id()));
        let mut big = album_with_songs("one", &["a"]);
        big.song[0].size = Some(2 * BYTES_PER_GB as i64);
        let downloads = Downloads::default();
        {
            let mut state = downloads.state();
            state.folder = Some(folder);
            state.manifest.items.push(Item::Album(big));
            state.limit = limit_of(1);
        }
        assert!(downloads.start_next().is_err());
        assert!(matches!(downloads.start_next(), Ok(None)), "said once");
        assert_eq!(downloads.summary().missing, 1);
        downloads.set_limit(limit_of(2));
        let Ok(Some(fetch)) = downloads.start_next() else {
            panic!("the song is let go on");
        };
        assert_eq!(fetch.room, Some(5 * BYTES_PER_GB), "what may arrive");
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
