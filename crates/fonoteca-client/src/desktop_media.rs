//! macOS's media controls: the keys of a keyboard or headset, Control Center
//! and the menu bar's Now Playing. Without registering here the system
//! sends the keys to Music. The rules about what to tell it, and when, are
//! in `now_playing`.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::Arc;
use std::{fs, io};

use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSImage;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemArtwork, MPMediaItemPropertyAlbumTitle,
    MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork, MPMediaItemPropertyPlaybackDuration,
    MPMediaItemPropertyTitle, MPNowPlayingInfoCenter, MPNowPlayingInfoPropertyElapsedPlaybackTime,
    MPNowPlayingInfoPropertyIsLiveStream, MPNowPlayingInfoPropertyPlaybackRate,
    MPNowPlayingPlaybackState, MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent,
    MPRemoteCommandHandlerStatus,
};

use crate::now_playing::{Command, NowPlaying, Track};

/// The folder of this run's covers, which the system reads from disk.
fn cover_folder() -> PathBuf {
    std::env::temp_dir().join(format!("fonoteca-now-playing-{}", std::process::id()))
}

/// Keeps `bytes`, the cover `cover_id`, in a file of this run's folder and
/// gives its path. The name comes from a hash, as an id is the server's to
/// choose and may not be fit for a file name.
pub fn store_cover(cover_id: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let mut hasher = DefaultHasher::new();
    cover_id.hash(&mut hasher);
    let folder = cover_folder();
    fs::create_dir_all(&folder)?;
    let path = folder.join(format!("{:016x}", hasher.finish()));
    fs::write(&path, bytes)?;
    Ok(path)
}

/// Removes this run's covers.
pub fn remove_covers() {
    match fs::remove_dir_all(cover_folder()) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            eprintln!("The covers kept for the media controls couldn't be removed: {error}");
        }
        _ => {}
    }
}

/// Makes the app the one the media keys and Control Center's buttons go to
/// once it has something to show, and hands each press to `on_command`, on
/// whichever thread the system calls from: it must not wait for the app.
pub fn listen(on_command: impl Fn(Command) + Send + Sync + 'static) {
    let on_command: Arc<dyn Fn(Command) + Send + Sync> = Arc::new(on_command);
    // Safety: the shared command center is there from the start of any app.
    let center = unsafe { MPRemoteCommandCenter::sharedCommandCenter() };
    let plain = |command: &MPRemoteCommand, pressed: Command| {
        register(command, &on_command, move |_| pressed);
    };
    // Safety: each is a command of the center, retained by it.
    unsafe {
        plain(&center.playCommand(), Command::Play);
        plain(&center.pauseCommand(), Command::Pause);
        plain(&center.togglePlayPauseCommand(), Command::Toggle);
        plain(&center.nextTrackCommand(), Command::Next);
        plain(&center.previousTrackCommand(), Command::Previous);
        register(
            &center.changePlaybackPositionCommand(),
            &on_command,
            |event| {
                // Safety: this command only sends position events.
                let event = event
                    .cast::<MPChangePlaybackPositionCommandEvent>()
                    .as_ref();
                Command::Seek(event.positionTime())
            },
        );
    }
}

fn register(
    command: &MPRemoteCommand,
    on_command: &Arc<dyn Fn(Command) + Send + Sync>,
    pressed: impl Fn(NonNull<MPRemoteCommandEvent>) -> Command + 'static,
) {
    let on_command = Arc::clone(on_command);
    let handler = RcBlock::new(move |event: NonNull<MPRemoteCommandEvent>| {
        on_command(pressed(event));
        MPRemoteCommandHandlerStatus::Success
    });
    // Safety: the handler's block matches the one the method takes, and the
    // command copies it.
    unsafe {
        command.setEnabled(true);
        command.addTargetWithHandler(&handler);
    }
}

/// What the system's Now Playing shows.
pub struct SystemControls {
    center: Retained<MPNowPlayingInfoCenter>,
    shown: RefCell<Option<Shown>>,
}

/// The track shown, and its cover as the system takes it, made once and not
/// at each state.
struct Shown {
    track: Track,
    artwork: Option<Retained<MPMediaItemArtwork>>,
}

impl Default for SystemControls {
    fn default() -> Self {
        Self {
            // Safety: the default center is there from the start of any app.
            center: unsafe { MPNowPlayingInfoCenter::defaultCenter() },
            shown: RefCell::new(None),
        }
    }
}

impl NowPlaying for SystemControls {
    fn show(&self, track: &Track, playing: bool, position: f64) {
        let artwork = track.cover.as_deref().and_then(artwork);
        *self.shown.borrow_mut() = Some(Shown {
            track: track.clone(),
            artwork,
        });
        self.set_state(playing, position);
    }

    fn set_state(&self, playing: bool, position: f64) {
        let shown = self.shown.borrow();
        let Some(shown) = shown.as_ref() else { return };
        let state = if playing {
            MPNowPlayingPlaybackState::Playing
        } else {
            MPNowPlayingPlaybackState::Paused
        };
        // Safety: the dictionary holds what the center documents for its
        // keys: strings, numbers and an artwork.
        unsafe {
            self.center
                .setNowPlayingInfo(Some(&info(shown, playing, position)));
            self.center.setPlaybackState(state);
        }
    }

    fn clear(&self) {
        *self.shown.borrow_mut() = None;
        // Safety: as in `set_state`.
        unsafe {
            self.center.setNowPlayingInfo(None);
            self.center
                .setPlaybackState(MPNowPlayingPlaybackState::Stopped);
        }
    }
}

/// The picture in the file at `path`, for Control Center to ask for at the
/// size it wants. One that can't be read is left out: the song is shown
/// without it.
fn artwork(path: &std::path::Path) -> Option<Retained<MPMediaItemArtwork>> {
    let image = NSImage::initWithContentsOfFile(
        NSImage::alloc(),
        &NSString::from_str(&path.to_string_lossy()),
    );
    let Some(image) = image else {
        eprintln!(
            "The cover {} couldn't be read as a picture.",
            path.display()
        );
        return None;
    };
    let size = image.size();
    // The block keeps the image alive for as long as the system keeps the
    // artwork, and it may ask from any thread.
    let handler = RcBlock::new(move |_wanted| NonNull::from(&*image));
    // Safety: the block returns a valid pointer, to an image it owns.
    Some(unsafe {
        MPMediaItemArtwork::initWithBoundsSize_requestHandler(
            MPMediaItemArtwork::alloc(),
            size,
            &handler,
        )
    })
}

fn info(
    shown: &Shown,
    playing: bool,
    position: f64,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let track = &shown.track;
    let mut entries: Vec<(&NSString, Retained<AnyObject>)> = Vec::new();
    // Safety: reading the system's constants.
    unsafe {
        entries.push((MPMediaItemPropertyTitle, text(&track.title)));
        if !track.artist.is_empty() {
            entries.push((MPMediaItemPropertyArtist, text(&track.artist)));
        }
        if !track.album.is_empty() {
            entries.push((MPMediaItemPropertyAlbumTitle, text(&track.album)));
        }
        entries.push((
            MPNowPlayingInfoPropertyIsLiveStream,
            number(NSNumber::new_bool(track.live)),
        ));
        if !track.live {
            entries.push((
                MPMediaItemPropertyPlaybackDuration,
                number(NSNumber::new_f64(track.duration)),
            ));
            entries.push((
                MPNowPlayingInfoPropertyElapsedPlaybackTime,
                number(NSNumber::new_f64(position)),
            ));
        }
        entries.push((
            MPNowPlayingInfoPropertyPlaybackRate,
            number(NSNumber::new_f64(if playing { 1.0 } else { 0.0 })),
        ));
        if let Some(artwork) = &shown.artwork {
            entries.push((MPMediaItemPropertyArtwork, artwork.clone().into()));
        }
    }
    let (keys, objects): (Vec<&NSString>, Vec<Retained<AnyObject>>) = entries.into_iter().unzip();
    NSDictionary::from_retained_objects(&keys, &objects)
}

fn text(value: &str) -> Retained<AnyObject> {
    NSString::from_str(value).into()
}

fn number(value: Retained<NSNumber>) -> Retained<AnyObject> {
    value.into()
}
