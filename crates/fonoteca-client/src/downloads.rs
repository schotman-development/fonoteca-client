//! Fetches the songs the user asked to keep, one at a time, in the order
//! they were asked for, into the downloads folder.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

use opensubsonic::Client;
use slint::{ComponentHandle, Timer};

use crate::offline::{Fetch, Item, Work};
use crate::{AppWindow, browse, offline, prefs, quality};

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
    let saved = prefs::load();
    ui.set_download_limit(saved.download_limit);
    browse::downloads(browser).set_limit(offline::limit_of(saved.download_limit as usize));
    let chosen = quality::within_lists(saved.download_quality);
    ui.set_download_format(chosen.format);
    ui.set_download_bitrate(chosen.bitrate);
    let (format, bit_rate) = quality::download_request(&saved);
    browse::downloads(browser).set_quality(format, bit_rate);
    ui.on_choose_download_quality({
        let browser = browser.clone();
        move |format, bitrate| {
            let mut saved = prefs::load();
            saved.download_quality = prefs::StreamQuality { format, bitrate };
            prefs::save(&saved);
            let (format, bit_rate) = quality::download_request(&saved);
            browse::downloads(&browser).set_quality(format, bit_rate);
        }
    });
    let pending: Rc<RefCell<Option<Item>>> = Rc::default();
    ui.on_confirm_download({
        let (weak, browser, pending) = (ui.as_weak(), browser.clone(), pending.clone());
        move || {
            if let Some(item) = pending.take() {
                keep(&weak.unwrap(), &browser, item);
            }
        }
    });
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
            browse::downloads_changed(&ui, &browser);
        }
    });
    ui.on_download_page({
        let (weak, browser, pending) = (ui.as_weak(), browser.clone(), pending.clone());
        move || {
            if let Some(item) = browse::shown_item(&browser) {
                ask(&weak.unwrap(), &pending, item);
            }
        }
    });
    ui.on_download_track({
        let (weak, browser, pending) = (ui.as_weak(), browser.clone(), pending.clone());
        move |row| {
            if let Some(song) = browse::song_at(&browser, row as usize) {
                ask(&weak.unwrap(), &pending, Item::Song(Box::new(song)));
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
        let (weak, browser, pending) = (ui.as_weak(), browser.clone(), pending.clone());
        move |link| {
            let Some(client) = browse::client(&browser) else {
                return;
            };
            let (weak, browser, pending) = (weak.clone(), browser.clone(), pending.clone());
            slint::spawn_local(async move {
                let item = item_at(&client, &link).await;
                let Some(ui) = weak.upgrade() else { return };
                match item {
                    Ok(item) => ask(&ui, &pending, item),
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

/// Asks whether to keep `item`, telling what that takes, and keeps it in
/// `pending` for the answer.
fn ask(ui: &AppWindow, pending: &RefCell<Option<Item>>, item: Item) {
    let (format, bit_rate) = quality::download_request(&prefs::load());
    let note = offline::estimate_note(item.songs(), format, bit_rate);
    ui.invoke_ask_download(item.title().into(), note.into());
    *pending.borrow_mut() = Some(item);
}

/// Asks for `item` to be kept and starts fetching it.
fn keep(ui: &AppWindow, browser: &browse::Shared, item: Item) {
    let server = ui.get_address().to_string();
    match browse::downloads(browser).add(&server, item) {
        Ok(()) => {
            browse::downloads_changed(ui, browser);
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
    browse::downloads_changed(ui, browser);
    pump(ui, browser);
}

/// Brings the kept copy of the album or playlist on screen up to what the
/// server just sent, unless that came from the kept copy itself.
pub fn follow_shown(ui: &AppWindow, browser: &browse::Shared) {
    if browse::is_offline(browser) {
        return;
    }
    if let Some(item) = browse::shown_item(browser) {
        follow(ui, browser, item);
    }
}

/// Follows the server's playlists: the kept ones it says changed are
/// fetched anew, and those it no longer has are let go.
pub fn follow_playlists(
    ui: &AppWindow,
    browser: &browse::Shared,
    (stale, gone): (Vec<String>, Vec<String>),
) {
    if browse::is_offline(browser) {
        return;
    }
    for id in gone {
        let_go(ui, browser, &format!("playlist:{id}"));
    }
    let Some(client) = browse::client(browser) else {
        return;
    };
    for id in stale {
        let (client, weak, browser) = (client.clone(), ui.as_weak(), browser.clone());
        slint::spawn_local(async move {
            let playlist = client.get_playlist(&id).await;
            let Some(ui) = weak.upgrade() else { return };
            match playlist {
                Ok(playlist) => follow(&ui, &browser, Item::Playlist(playlist)),
                Err(e) => {
                    browse::notify(&ui, &browser, format!("A download isn't up to date: {e}"))
                }
            }
        })
        .expect("the event loop is running");
    }
}

fn follow(ui: &AppWindow, browser: &browse::Shared, item: Item) {
    match browse::downloads(browser).follow(item) {
        Ok(true) => {
            browse::downloads_changed(ui, browser);
            pump(ui, browser);
        }
        Ok(false) => {}
        Err(e) => browse::notify(ui, browser, format!("A download isn't up to date: {e}")),
    }
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
    let work = next.work.clone();
    let job = tokio::spawn(fetch(client, next));
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let result = job.await.expect("the download task doesn't panic");
        let Some(ui) = weak.upgrade() else { return };
        let delay = settle(&ui, &browser, &work, result);
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

/// Records how the fetch of `work` ended, and says how long to wait
/// before going on, if it should.
fn settle(
    ui: &AppWindow,
    browser: &browse::Shared,
    work: &Work,
    result: Result<Arrived, Failure>,
) -> Option<Duration> {
    let downloads = browse::downloads(browser);
    let failure = match result {
        Ok(arrived) => {
            if arrived.originals_only {
                downloads.note_originals_only();
            }
            match downloads.finished(work, arrived.path, &arrived.tag) {
                Ok(()) => return None,
                Err(e) => Failure::Disk(e),
            }
        }
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
                downloads.give_up(work, e.to_string());
                browse::notify(ui, browser, format!("Something wasn't downloaded: {e}"));
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

async fn fetch(client: Arc<Client>, next: Fetch) -> Result<Arrived, Failure> {
    let mut partial = None;
    let arrived = receive(&client, &next, &mut partial).await;
    match (partial, arrived) {
        (Some(partial), Ok(arrived)) => match std::fs::rename(&partial, &arrived.path) {
            Ok(()) => Ok(arrived),
            Err(e) => {
                let _ = std::fs::remove_file(&partial);
                Err(disk(e))
            }
        },
        (Some(partial), Err(failure)) => {
            // A half-fetched file left behind is deleted at the next start.
            let _ = std::fs::remove_file(&partial);
            Err(failure)
        }
        (None, result) => result,
    }
}

/// Writes what `next` is for to a partial file as it arrives, the file it
/// is in `partial` once there is one, and says where it goes once whole.
async fn receive(
    client: &Client,
    next: &Fetch,
    partial: &mut Option<PathBuf>,
) -> Result<Arrived, Failure> {
    let song = match &next.work {
        Work::Cover(id) => {
            let cover = client.get_cover_art(id, Some(offline::COVER_SIZE));
            let bytes = cover.await.map_err(Failure::Server)?;
            let path = offline::cover_file(&next.folder, id);
            let file = partial.insert(offline::partial_of(&path));
            create(file, &next.cancel)?
                .write_all(&bytes)
                .map_err(disk)?;
            return Ok(Arrived {
                path,
                tag: String::new(),
                originals_only: false,
            });
        }
        Work::Song(song) => song,
    };
    let (format, bit_rate) = (next.asked.0.as_str(), next.asked.1);
    let mut stream = if format == offline::RAW {
        client.download(&song.id).await
    } else {
        client.stream(&song.id, format, bit_rate, None).await
    }
    .map_err(Failure::Server)?;
    let converted = offline::sent_as_asked(format, stream.content_type().as_deref());
    let tag = if converted {
        offline::tag_of(format, bit_rate)
    } else {
        offline::ORIGINAL.to_owned()
    };
    let path = offline::song_file(&next.folder, song, &tag);
    let mut file = create(partial.insert(offline::partial_of(&path)), &next.cancel)?;
    let mut written = 0;
    while let Some(bytes) = stream.chunk().await.map_err(Failure::Server)? {
        if next.cancel.load(Relaxed) {
            return Err(Failure::Stopped);
        }
        written += bytes.len() as u64;
        if next.room.is_some_and(|room| written > room) {
            return Err(Failure::OverLimit);
        }
        file.write_all(&bytes).map_err(disk)?;
    }
    Ok(Arrived {
        path,
        tag,
        originals_only: !converted,
    })
}

/// A song or cover whole in its partial file, and where it goes.
struct Arrived {
    path: PathBuf,
    /// How a song was fetched, as `offline::tag_of` says.
    tag: String,
    /// Whether a conversion was asked for and the original came.
    originals_only: bool,
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

    /// A server that answers every request with the three bytes "abc".
    fn serve_abc() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = std::io::Read::read(&mut stream, &mut [0; 1024]);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
                );
            }
        });
        address
    }

    #[test]
    fn a_cover_is_not_written_once_cancelled() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = Client::new(&serve_abc(), opensubsonic::Auth::None).unwrap();
        let folder = std::env::temp_dir().join(format!("cover-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let partial = offline::partial_of(&offline::cover_file(&folder, "c"));
        let fetch = |cancelled: bool| Fetch {
            work: Work::Cover("c".to_owned()),
            folder: folder.clone(),
            asked: ("raw".to_owned(), 0),
            room: None,
            cancel: Arc::new(AtomicBool::new(cancelled)),
        };
        let result = runtime.block_on(receive(&client, &fetch(true), &mut None));
        assert!(matches!(result, Err(Failure::Stopped)));
        assert!(!partial.exists(), "a cancelled cover isn't written");
        let result = runtime.block_on(receive(&client, &fetch(false), &mut None));
        assert!(result.is_ok());
        assert!(partial.exists());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_refusal_is_not_tried_again() {
        assert_eq!(retry_after(&Error::Status(403), 1), None);
        assert_eq!(retry_after(&Error::Status(404), 1), None);
        assert_eq!(retry_after(&Error::Unexpected("x".into()), 1), None);
    }
}
