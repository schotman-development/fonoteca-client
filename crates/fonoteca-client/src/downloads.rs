//! Fetches the songs the user asked to keep, one at a time, in the order
//! they were asked for, into the downloads folder.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

use opensubsonic::Client;
use slint::{ComponentHandle, Timer};

use crate::offline::Item;
use crate::{AppWindow, browse, offline, prefs};

/// How long to wait before the first, second and third new try of a song
/// the server or the network failed on.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(10),
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
];

/// Why a song didn't arrive.
enum Failure {
    /// It isn't wanted any more.
    Stopped,
    /// More arrived than the size limit leaves room for.
    OverLimit,
    Disk(io::Error),
    Server(opensubsonic::Error),
}

pub fn wire(ui: &AppWindow, browser: &browse::Shared) {
    ui.set_can_download(true);
    browse::load_downloads(browser);
    let limit = prefs::load().download_limit;
    ui.set_download_limit(limit);
    browse::downloads(browser).set_limit(offline::limit_of(limit as usize));
    ui.on_choose_download_limit({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| {
            let mut saved = prefs::load();
            saved.download_limit = index;
            prefs::save(&saved);
            browse::downloads(&browser).set_limit(offline::limit_of(index as usize));
            let ui = weak.unwrap();
            browse::show_downloads(&ui, &browser);
            pump(&ui, &browser);
        }
    });
    ui.on_remove_all_downloads({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            if let Err(e) = browse::downloads(&browser).remove_all() {
                browse::notify(&ui, &browser, format!("Not all of it was removed: {e}"));
            }
            browse::show_downloads(&ui, &browser);
        }
    });
    ui.on_download_page({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            if let Some(item) = browse::shown_item(&browser) {
                keep(&weak.unwrap(), &browser, item);
            }
        }
    });
    ui.on_download_track({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            if let Some(song) = browse::song_at(&browser, row as usize) {
                keep(&weak.unwrap(), &browser, Item::Song(Box::new(song)));
            }
        }
    });
    ui.on_remove_track_download({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            if let Some(song) = browse::song_at(&browser, row as usize) {
                let link = Item::Song(Box::new(song)).link();
                let_go(&weak.unwrap(), &browser, &link);
            }
        }
    });
    ui.on_download_link({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |link| {
            let Some(client) = browse::client(&browser) else {
                return;
            };
            let (weak, browser) = (weak.clone(), browser.clone());
            slint::spawn_local(async move {
                let item = item_at(&client, &link).await;
                let Some(ui) = weak.upgrade() else { return };
                match item {
                    Ok(item) => keep(&ui, &browser, item),
                    Err(e) => {
                        browse::notify(&ui, &browser, format!("Not downloaded: {e}"));
                    }
                }
            })
            .expect("the event loop is running");
        }
    });
    ui.on_remove_link_download({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |link| let_go(&weak.unwrap(), &browser, &link)
    });
    ui.on_remove_page_download({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            if let Some(link) = browse::link(&browser) {
                let_go(&weak.unwrap(), &browser, &link);
            }
        }
    });
}

/// Asks for `item` to be kept and starts fetching it.
fn keep(ui: &AppWindow, browser: &browse::Shared, item: Item) {
    let server = ui.get_address().to_string();
    match browse::downloads(browser).add(&server, item) {
        Ok(()) => {
            browse::show_downloads(ui, browser);
            pump(ui, browser);
        }
        Err(e) => browse::notify(ui, browser, format!("Not downloaded: {e}")),
    }
}

/// Lets go of the item at `link` and deletes what no other item wants.
fn let_go(ui: &AppWindow, browser: &browse::Shared, link: &str) {
    if let Err(e) = browse::downloads(browser).remove(link) {
        browse::notify(ui, browser, format!("Not all of it was removed: {e}"));
    }
    browse::show_downloads(ui, browser);
    pump(ui, browser);
}

/// The album or playlist at `link`, as the server has it now.
async fn item_at(client: &Client, link: &str) -> opensubsonic::Result<Item> {
    match link.split_once(':') {
        Some(("album", id)) => client.get_album(id).await.map(Item::Album),
        Some(("playlist", id)) => client.get_playlist(id).await.map(Item::Playlist),
        _ => Err(opensubsonic::Error::Unexpected(format!(
            "{link} can't be downloaded"
        ))),
    }
}

/// Picks up what the last run left: half-fetched songs are dropped and the
/// songs still missing are fetched.
pub fn resume(ui: &AppWindow, browser: &browse::Shared) {
    browse::downloads(browser).clear_partials();
    pump(ui, browser);
}

/// Starts fetching the next song missing, if none is being fetched; when it
/// ends, goes on to the next.
fn pump(ui: &AppWindow, browser: &browse::Shared) {
    let Some(client) = browse::client(browser) else {
        return;
    };
    let downloads = browse::downloads(browser);
    let next = match downloads.start_next() {
        Ok(Some(next)) => next,
        Ok(None) => return,
        Err(reason) => return browse::notify(ui, browser, reason),
    };
    let id = next.song.id.clone();
    let job = tokio::spawn(fetch(
        client,
        id.clone(),
        next.partial,
        next.path.clone(),
        next.room,
        next.cancel,
    ));
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = job.await.expect("the download task doesn't panic");
        let Some(ui) = weak.upgrade() else { return };
        let delay = settle(&ui, &browser, &id, next.path, result);
        browse::show_downloads(&ui, &browser);
        match delay {
            None => pump(&ui, &browser),
            Some(delay) => {
                let (weak, browser) = (ui.as_weak(), browser.clone());
                Timer::single_shot(delay, move || {
                    if let Some(ui) = weak.upgrade() {
                        pump(&ui, &browser);
                    }
                });
            }
        }
    })
    .expect("the event loop is running");
}

/// Records how the fetch of song `id` ended, and says how long to wait
/// before going on, if it should.
fn settle(
    ui: &AppWindow,
    browser: &browse::Shared,
    id: &str,
    path: PathBuf,
    result: Result<(), Failure>,
) -> Option<Duration> {
    let downloads = browse::downloads(browser);
    let failure = match result {
        Ok(()) => match downloads.finished(id, path) {
            Ok(()) => return None,
            Err(e) => Failure::Disk(e),
        },
        Err(failure) => failure,
    };
    downloads.ended();
    match failure {
        Failure::Stopped => None,
        Failure::OverLimit => {
            downloads.halt(offline::LIMIT_REACHED.to_owned());
            browse::notify(ui, browser, offline::LIMIT_REACHED.to_owned());
            None
        }
        Failure::Disk(e) => {
            let reason = stop_reason(&e);
            downloads.halt(reason.clone());
            browse::notify(ui, browser, reason);
            None
        }
        Failure::Server(e) => {
            let delay = retry_after(&e, downloads.failed_try());
            if delay.is_none() {
                downloads.give_up(id, e.to_string());
                browse::notify(ui, browser, format!("A song wasn't downloaded: {e}"));
            }
            delay
        }
    }
}

fn stop_reason(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::StorageFull {
        "Downloads stopped: the disk is full.".to_owned()
    } else {
        format!("Downloads stopped: {error}")
    }
}

/// How long to wait before trying a song again after `error`, on the
/// `attempt`th failure in a row; none when trying again can't help: the
/// server refused the song, or it has been tried enough.
fn retry_after(error: &opensubsonic::Error, attempt: usize) -> Option<Duration> {
    use opensubsonic::Error;
    let may_pass = match error {
        Error::Http(_) => true,
        Error::Status(status) => *status == 429 || (500..600).contains(status),
        _ => false,
    };
    may_pass
        .then(|| RETRY_DELAYS.get(attempt.checked_sub(1)?).copied())
        .flatten()
}

/// Writes the song to `partial` as it arrives, and moves it to `path` once
/// whole, so a file there is always a finished one.
/// A disk error as a failure. A folder that isn't there means everything was
/// removed while the song was on its way, which stops it like a cancel does.
fn disk(error: io::Error) -> Failure {
    if error.kind() == io::ErrorKind::NotFound {
        Failure::Stopped
    } else {
        Failure::Disk(error)
    }
}

/// Opens the file a song arrives in, unless it was cancelled meanwhile. The
/// folder is made if something took it away.
fn create(path: &Path, cancel: &AtomicBool) -> Result<File, Failure> {
    if cancel.load(Relaxed) {
        return Err(Failure::Stopped);
    }
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(disk)?;
    }
    File::create(path).map_err(disk)
}

async fn fetch(
    client: Arc<Client>,
    id: String,
    partial: PathBuf,
    path: PathBuf,
    room: Option<u64>,
    cancel: Arc<AtomicBool>,
) -> Result<(), Failure> {
    let mut stream = client.download(&id).await.map_err(Failure::Server)?;
    let mut file = create(&partial, &cancel)?;
    let arrived = async {
        let mut written = 0;
        while let Some(bytes) = stream.chunk().await.map_err(Failure::Server)? {
            if cancel.load(Relaxed) {
                return Err(Failure::Stopped);
            }
            written += bytes.len() as u64;
            if room.is_some_and(|room| written > room) {
                return Err(Failure::OverLimit);
            }
            file.write_all(&bytes).map_err(disk)?;
        }
        Ok(())
    }
    .await;
    drop(file);
    let moved = arrived.and_then(|()| std::fs::rename(&partial, &path).map_err(disk));
    if moved.is_err() {
        // A half-fetched file left behind is deleted at the next start.
        let _ = std::fs::remove_file(&partial);
    }
    moved
}

#[cfg(test)]
mod tests {
    use opensubsonic::Error;

    use super::*;

    #[test]
    fn a_failing_server_is_tried_again_after_longer_and_longer_and_then_left() {
        let down = Error::Status(503);
        assert_eq!(retry_after(&down, 1), Some(Duration::from_secs(10)));
        assert_eq!(retry_after(&down, 2), Some(Duration::from_secs(60)));
        assert_eq!(retry_after(&down, 3), Some(Duration::from_secs(300)));
        assert_eq!(retry_after(&down, 4), None);
        assert_eq!(retry_after(&down, 0), None);
        assert!(retry_after(&Error::Status(429), 1).is_some());
    }

    #[test]
    fn a_missing_folder_stops_a_song_like_a_cancel_and_other_errors_halt() {
        let missing = io::Error::from(io::ErrorKind::NotFound);
        assert!(matches!(disk(missing), Failure::Stopped));
        let refused = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(matches!(disk(refused), Failure::Disk(_)));
    }

    #[test]
    fn a_song_is_not_opened_once_cancelled_and_its_folder_is_made_when_gone() {
        let folder = std::env::temp_dir().join(format!("create-test-{}", std::process::id()));
        let path = folder.join("songs").join("a.partial");
        assert!(matches!(
            create(&path, &AtomicBool::new(true)),
            Err(Failure::Stopped)
        ));
        assert!(!folder.exists(), "nothing is made for a cancelled song");
        assert!(create(&path, &AtomicBool::new(false)).is_ok());
        assert!(path.exists());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_full_disk_is_said_plainly() {
        let full = io::Error::from(io::ErrorKind::StorageFull);
        assert_eq!(stop_reason(&full), "Downloads stopped: the disk is full.");
        let other = io::Error::other("no good");
        assert_eq!(stop_reason(&other), "Downloads stopped: no good");
    }

    #[test]
    fn a_refusal_is_not_tried_again() {
        assert_eq!(retry_after(&Error::Status(403), 1), None);
        assert_eq!(retry_after(&Error::Status(404), 1), None);
        assert_eq!(retry_after(&Error::Unexpected("x".into()), 1), None);
    }
}
