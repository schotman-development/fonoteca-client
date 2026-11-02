//! Playing music on this device: a queue from the page on screen, the bar
//! under the pages, and the sound device, volume leveling and sound
//! processing in the settings.
//! The queue and where it is are saved on the server, for this app or any
//! other to carry on with.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use opensubsonic::{Child, Client};
use playback::{Gain, Leveling, Player, Step, Track};
use slint::{ComponentHandle, Image, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::{AppWindow, PathStep, browse, prefs, sound};

/// The settings' volume leveling entries, in their order.
const LEVELING: [Leveling; 3] = [Leveling::Off, Leveling::Track, Leveling::Album];
/// How often the bar catches up with the player.
const TICK: Duration = Duration::from_millis(250);
/// How many seconds play between saves of where the queue is, so the next
/// start picks up close to where this one stopped.
const SAVE_EVERY: f64 = 15.0;
/// The bar's cover, in logical pixels.
const COVER_SIZE: f32 = 48.0;

#[derive(Default)]
struct Playing {
    player: Option<Player>,
    /// The songs queued, as the page had them; the one the bar shows and
    /// the steps it shows for it.
    queue: Vec<Child>,
    shown: Option<usize>,
    path: Vec<Step>,
    /// Where the queue saved on the server left off, until it plays.
    resume: Option<(usize, f64)>,
    /// The track, whether it was paused and the seconds into it when the
    /// queue was last saved; and whether the server turned the saving down.
    saved: Option<(Option<usize>, bool, f64)>,
    queue_refused: bool,
    /// The settings' sound devices by cpal id, none for the default.
    devices: Vec<Option<String>>,
    tick: Timer,
}

type Shared = Rc<RefCell<Playing>>;

pub fn wire(ui: &AppWindow, browser: &browse::Shared) {
    let playing = Shared::default();
    ui.set_leveling(prefs::load().leveling.clamp(0, 2));
    ui.set_can_play(true);
    list_devices(ui, &playing);

    ui.on_library_started({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || restore(&weak.unwrap(), &browser, &playing)
    });
    ui.on_play_tracks({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row| play(&weak.unwrap(), &browser, &playing, row as usize)
    });
    ui.on_toggle_pause({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            if resume(&weak.unwrap(), &browser, &playing, false) {
                return;
            }
            let p = playing.borrow();
            let Some(player) = &p.player else { return };
            let status = player.status();
            if status.ended {
                player.play(p.queue.iter().map(track).collect(), 0, 0.0);
            } else {
                player.set_paused(!status.paused);
            }
        }
    });
    ui.on_previous_track({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            resume(&weak.unwrap(), &browser, &playing, true);
            playing.borrow().player.iter().for_each(Player::previous)
        }
    });
    ui.on_next_track({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            resume(&weak.unwrap(), &browser, &playing, true);
            playing.borrow().player.iter().for_each(Player::next)
        }
    });
    ui.on_seek({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |seconds| {
            let seconds = f64::from(seconds);
            if let Some(at) = playing.borrow_mut().resume.as_mut() {
                at.1 = seconds;
            }
            if resume(&weak.unwrap(), &browser, &playing, true) {
                return;
            }
            let p = playing.borrow();
            let Some(player) = &p.player else { return };
            player.seek(seconds);
            let status = player.status();
            save(&browser, &p, &playing, status.index, seconds);
            #[cfg(target_os = "android")]
            announce(&p, status.index, status.paused || status.ended, seconds);
        }
    });
    ui.on_list_devices({
        let (weak, playing) = (ui.as_weak(), playing.clone());
        move || list_devices(&weak.unwrap(), &playing)
    });
    ui.on_choose_output({
        let playing = playing.clone();
        move |index| {
            let p = playing.borrow();
            let device = p.devices.get(index as usize).cloned().flatten();
            let mut saved = prefs::load();
            saved.output_device = device.clone();
            prefs::save(&saved);
            if let Some(player) = &p.player {
                player.set_device(device);
            }
        }
    });
    ui.on_choose_leveling({
        let playing = playing.clone();
        move |index| {
            let mut saved = prefs::load();
            saved.leveling = index;
            prefs::save(&saved);
            if let Some(player) = &playing.borrow().player {
                player.set_leveling(LEVELING[index.clamp(0, 2) as usize]);
            }
        }
    });
    sound::wire(ui, {
        let playing = playing.clone();
        move |dsp| {
            if let Some(player) = &playing.borrow().player {
                player.set_dsp(dsp);
            }
        }
    });
}

/// Puts the sound devices there are now in the settings, with the one chosen
/// selected. Looking for them can take a moment, so it happens off the event
/// loop.
fn list_devices(ui: &AppWindow, playing: &Shared) {
    let found = tokio::task::spawn_blocking(playback::devices);
    let (weak, playing) = (ui.as_weak(), playing.clone());
    slint::spawn_local(async move {
        let Ok(found) = found.await else { return };
        let Some(ui) = weak.upgrade() else { return };
        let mut names = vec![SharedString::from("System default")];
        let mut devices = vec![None];
        for (id, name) in found {
            let pcm = id.strip_prefix("alsa:").unwrap_or(&id);
            names.push(format!("{name} ({pcm})").into());
            devices.push(Some(id));
        }
        let chosen = devices
            .iter()
            .position(|id| *id == prefs::load().output_device);
        ui.set_output_devices(ModelRc::new(VecModel::from(names)));
        ui.set_output_device(chosen.unwrap_or(0) as i32);
        playing.borrow_mut().devices = devices;
    })
    .expect("the event loop is running");
}

/// Plays the page's songs from its `row`th row, in place of what played.
fn play(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, row: usize) {
    let (songs, start) = browse::songs(browser, ui.get_page(), row);
    begin(ui, browser, playing, songs, (start, 0.0), false);
}

/// Plays the queue saved on the server from where it left off, `paused` or
/// not; false when there is none waiting.
fn resume(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, paused: bool) -> bool {
    let p = playing.borrow();
    let Some(at) = p.resume else { return false };
    let songs = p.queue.clone();
    drop(p);
    begin(ui, browser, playing, songs, at, paused);
    true
}

/// Plays `songs` from `at`, a track and seconds into it, in place of what
/// played, `paused` or not.
fn begin(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    songs: Vec<Child>,
    (start, seconds): (usize, f64),
    paused: bool,
) {
    let Some(client) = browse::client(browser).filter(|_| start < songs.len()) else {
        return;
    };
    let mut p = playing.borrow_mut();
    let device = prefs::load().output_device;
    // Each queue gets a player with the client signed in now. The last one
    // lets the device go first, since a device played on directly can only
    // be open once.
    p.player = None;
    let player = Player::new(device, fetch(client));
    player.set_leveling(LEVELING[ui.get_leveling().clamp(0, 2) as usize]);
    player.set_dsp(sound::dsp());
    player.play(songs.iter().map(track).collect(), start, seconds);
    // Before any of it is heard: a track starts once some of it is in.
    player.set_paused(paused);
    p.player = Some(player);
    p.queue = songs;
    p.resume = None;
    p.saved = None;
    p.shown = None;
    p.path.clear();
    let (weak, pages, ticked) = (ui.as_weak(), browser.clone(), playing.clone());
    p.tick.start(TimerMode::Repeated, TICK, move || {
        if let Some(ui) = weak.upgrade() {
            show(&ui, &pages, &ticked);
        }
    });
    drop(p);
    show(ui, browser, playing);
}

/// Puts what the player says on the bar, or takes the bar down after a
/// sign out.
fn show(ui: &AppWindow, browser: &browse::Shared, playing: &Shared) {
    let mut p = playing.borrow_mut();
    if !browse::started(browser) {
        p.player = None;
        p.tick.stop();
        ui.set_playing(false);
        #[cfg(target_os = "android")]
        crate::media::hide();
        return;
    }
    let Some(player) = &p.player else { return };
    let status = player.status();
    let state = (status.index, status.paused || status.ended);
    let last = p.saved;
    #[cfg(target_os = "android")]
    if last.map(|(index, paused, _)| (index, paused)) != Some(state) {
        announce(&p, state.0, state.1, status.position);
    }
    let due = last.is_none_or(|(index, paused, at)| {
        (index, paused) != state || !paused && (status.position - at).abs() >= SAVE_EVERY
    });
    if due {
        save(browser, &p, playing, status.index, status.position);
        p.saved = Some((state.0, state.1, status.position));
    }
    ui.set_playing(true);
    ui.set_playing_paused(status.paused || status.ended);
    ui.set_playing_position(status.position as f32);
    let problem = match (status.problem, &status.skipped) {
        (Some(problem), _) => problem,
        (None, Some((index, why))) => match p.queue.get(*index) {
            Some(song) => format!("Skipped {}: {why}", song.title),
            None => why.clone(),
        },
        (None, None) => String::new(),
    };
    ui.set_playing_problem(problem.into());
    ui.set_playing_bit_perfect(status.bit_perfect);
    // The device step comes once it is open in the track's format; until
    // then, every step can be lossless without the whole path being.
    let quality = if status.bit_perfect {
        "Bit-perfect"
    } else if status.path.first().is_some_and(|step| !step.lossless) {
        "Lossy source"
    } else if status.path.iter().any(|step| !step.lossless) {
        "Processed"
    } else {
        ""
    };
    ui.set_playing_quality(quality.into());
    if status.path != p.path {
        let steps: Vec<PathStep> = status
            .path
            .iter()
            .map(|step| PathStep {
                text: step.text.as_str().into(),
                lossless: step.lossless,
            })
            .collect();
        ui.set_playing_path(ModelRc::new(VecModel::from(steps)));
        p.path = status.path;
    }
    if status.index == p.shown {
        return;
    }
    p.shown = status.index;
    if let Some(index) = status.index {
        show_song(ui, browser, &p, playing, index);
    }
}

/// Puts the `index`th song of the queue on the bar, with its cover once it
/// comes.
fn show_song(
    ui: &AppWindow,
    browser: &browse::Shared,
    p: &Playing,
    playing: &Shared,
    index: usize,
) {
    let Some(song) = p.queue.get(index) else {
        return;
    };
    ui.set_playing_title(song.title.as_str().into());
    let names = browse::song_names(song);
    ui.set_playing_names(ModelRc::new(VecModel::from(names)));
    ui.set_playing_duration(song.duration.unwrap_or_default() as f32);
    ui.set_playing_cover(Image::default());
    if let Some(cover) = song.cover_art.clone() {
        let playing = playing.clone();
        browse::cover(ui, browser, cover, COVER_SIZE, move |ui, image| {
            if playing.borrow().shown == Some(index) {
                ui.set_playing_cover(image);
            }
        });
    }
}

/// Tells Android what plays, for its notification and lock screen.
#[cfg(target_os = "android")]
fn announce(p: &Playing, index: Option<usize>, paused: bool, seconds: f64) {
    let Some(song) = index.and_then(|index| p.queue.get(index)) else {
        return crate::media::hide();
    };
    let artists = song.artist.as_deref().unwrap_or_default();
    let duration = song.duration.unwrap_or_default() as f64;
    crate::media::show(&song.title, artists, !paused, duration, seconds);
}

/// Takes up the queue saved on the server, paused on the bar where it left
/// off, in place of anything from the account signed in before.
fn restore(ui: &AppWindow, browser: &browse::Shared, playing: &Shared) {
    #[cfg(target_os = "android")]
    crate::media::hide();
    {
        let mut p = playing.borrow_mut();
        p.player = None;
        p.tick.stop();
        p.queue.clear();
        p.resume = None;
        p.shown = None;
        p.saved = None;
        p.queue_refused = false;
    }
    ui.set_playing(false);
    let Some(client) = browse::client(browser) else {
        return;
    };
    let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
    slint::spawn_local(async move {
        let saved = client.get_play_queue().await;
        let Some(ui) = weak.upgrade() else { return };
        let mut p = playing.borrow_mut();
        // Something played meanwhile, or another account signed in.
        let current = browse::client(&browser).is_some_and(|now| Arc::ptr_eq(&now, &client));
        if !current || p.player.is_some() {
            return;
        }
        let queue = match saved {
            Ok(Some(queue)) if !queue.entry.is_empty() => queue,
            Err(opensubsonic::Error::Status(_) | opensubsonic::Error::Api(_)) => {
                p.queue_refused = true;
                return;
            }
            _ => return,
        };
        let index = queue
            .current
            .and_then(|id| queue.entry.iter().position(|song| song.id == id))
            .unwrap_or(0);
        let seconds = queue.position.unwrap_or(0) as f64 / 1000.0;
        p.queue = queue.entry;
        p.resume = Some((index, seconds));
        p.shown = Some(index);
        ui.set_playing(true);
        ui.set_playing_paused(true);
        ui.set_playing_position(seconds as f32);
        ui.set_playing_problem("".into());
        ui.set_playing_quality("".into());
        ui.set_playing_bit_perfect(false);
        ui.set_playing_path(ModelRc::default());
        show_song(&ui, &browser, &p, &playing, index);
    })
    .expect("the event loop is running");
}

/// Saves the queue and where it is on the server, for any app to carry on
/// with; a server that turned it down isn't asked again.
fn save(
    browser: &browse::Shared,
    p: &Playing,
    playing: &Shared,
    index: Option<usize>,
    seconds: f64,
) {
    let Some(client) = browse::client(browser).filter(|_| !p.queue_refused) else {
        return;
    };
    // Nothing playing, as without a device, would lose where it was.
    let Some(current) = index.and_then(|index| p.queue.get(index)) else {
        return;
    };
    let current = current.id.clone();
    let ids: Vec<String> = p.queue.iter().map(|song| song.id.clone()).collect();
    let position = (seconds * 1000.0) as i64;
    let playing = playing.clone();
    slint::spawn_local(async move {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let saved = client
            .save_play_queue(&ids, Some(&current), Some(position))
            .await;
        // One lost on the way is made again with the next change.
        if let Err(opensubsonic::Error::Status(_) | opensubsonic::Error::Api(_)) = saved {
            playing.borrow_mut().queue_refused = true;
        }
    })
    .expect("the event loop is running");
}

fn track(song: &Child) -> Track {
    let gain = song.replay_gain.as_ref();
    Track {
        id: song.id.clone(),
        gain: Gain {
            track: gain.and_then(|gain| gain.track_gain),
            album: gain.and_then(|gain| gain.album_gain),
            track_peak: gain.and_then(|gain| gain.track_peak),
            album_peak: gain.and_then(|gain| gain.album_peak),
            fallback: gain.and_then(|gain| gain.fallback_gain),
        },
    }
}

/// Fetches tracks from `client` as the server keeps them, into files in the
/// cache folder that are gone once played. Runs on the player's thread.
fn fetch(client: Arc<Client>) -> playback::Fetch {
    let runtime = tokio::runtime::Handle::current();
    let folder = cache();
    Box::new(move |track: &Track| {
        let mut stream = runtime
            .block_on(client.stream(&track.id))
            .map_err(|e| format!("The track couldn't be fetched: {e}"))?;
        let (download, mut fill) = playback::download(&folder, stream.size())
            .map_err(|e| format!("The track couldn't be saved to disk: {e}"))?;
        runtime.spawn(async move {
            loop {
                match stream.chunk().await {
                    Ok(Some(bytes)) if fill.append(&bytes) => {}
                    Ok(Some(_)) => return,
                    Ok(None) => return fill.finish(),
                    Err(e) => return fill.fail(format!("The track stopped arriving: {e}")),
                }
            }
        });
        Ok(download)
    })
}

/// fonoteca-client in the system's cache folder, or on Android in a cache
/// folder in the app's own, the only place it may write.
fn cache() -> PathBuf {
    #[cfg(target_os = "android")]
    let folder = prefs::FOLDER.get().map(|folder| folder.join("cache"));
    #[cfg(target_os = "macos")]
    let folder = std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches"));
    #[cfg(not(any(target_os = "android", target_os = "macos")))]
    let folder = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|folder| folder.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
    folder
        .unwrap_or_else(std::env::temp_dir)
        .join("fonoteca-client")
}
