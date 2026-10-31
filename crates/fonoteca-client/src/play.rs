//! Playing music on this device: a queue from the page on screen, the bar
//! under the pages, and the sound device and volume leveling in the settings.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use opensubsonic::{Child, Client};
use playback::{Gain, Leveling, Player, Step, Track};
use slint::{ComponentHandle, Image, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::{AppWindow, PathStep, browse, prefs};

/// The settings' volume leveling entries, in their order.
const LEVELING: [Leveling; 3] = [Leveling::Off, Leveling::Track, Leveling::Album];
/// How often the bar catches up with the player.
const TICK: Duration = Duration::from_millis(250);
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
    /// The settings' sound devices by cpal id, none for the default.
    devices: Vec<Option<String>>,
    tick: Timer,
}

type Shared = Rc<RefCell<Playing>>;

pub fn wire(ui: &AppWindow, browser: &browse::Shared) {
    let playing = Shared::default();
    let saved = prefs::load();
    let mut names = vec![SharedString::from("System default")];
    let mut devices = vec![None];
    for (id, name) in playback::devices() {
        let pcm = id.strip_prefix("alsa:").unwrap_or(&id);
        names.push(format!("{name} ({pcm})").into());
        devices.push(Some(id));
    }
    let chosen = devices.iter().position(|id| *id == saved.output_device);
    ui.set_output_devices(ModelRc::new(VecModel::from(names)));
    ui.set_output_device(chosen.unwrap_or(0) as i32);
    ui.set_leveling(saved.leveling.clamp(0, 2));
    ui.set_can_play(true);
    playing.borrow_mut().devices = devices;

    ui.on_play_tracks({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row| play(&weak.unwrap(), &browser, &playing, row as usize)
    });
    ui.on_toggle_pause({
        let playing = playing.clone();
        move || {
            let p = playing.borrow();
            let Some(player) = &p.player else { return };
            let status = player.status();
            if status.ended {
                player.play(p.queue.iter().map(track).collect(), 0);
            } else {
                player.set_paused(!status.paused);
            }
        }
    });
    ui.on_previous_track({
        let playing = playing.clone();
        move || playing.borrow().player.iter().for_each(Player::previous)
    });
    ui.on_next_track({
        let playing = playing.clone();
        move || playing.borrow().player.iter().for_each(Player::next)
    });
    ui.on_seek({
        let playing = playing.clone();
        move |seconds| {
            if let Some(player) = &playing.borrow().player {
                player.seek(f64::from(seconds));
            }
        }
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
}

/// Plays the page's songs from the `start`th, in place of what played.
fn play(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, start: usize) {
    let songs = browse::songs(browser);
    let Some(client) = browse::client(browser).filter(|_| start < songs.len()) else {
        return;
    };
    let mut p = playing.borrow_mut();
    let device = p
        .devices
        .get(ui.get_output_device() as usize)
        .cloned()
        .flatten();
    // Each queue gets a player with the client signed in now. The last one
    // lets the device go first, since a device played on directly can only
    // be open once.
    p.player = None;
    let player = Player::new(device, fetch(client));
    player.set_leveling(LEVELING[ui.get_leveling().clamp(0, 2) as usize]);
    player.play(songs.iter().map(track).collect(), start);
    p.player = Some(player);
    p.queue = songs;
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
        return;
    }
    let Some(player) = &p.player else { return };
    let status = player.status();
    ui.set_playing(true);
    ui.set_playing_paused(status.paused || status.ended);
    ui.set_playing_position(status.position as f32);
    ui.set_playing_problem(status.problem.unwrap_or_default().into());
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
    let Some(song) = status.index.and_then(|index| p.queue.get(index)) else {
        return;
    };
    ui.set_playing_title(song.title.as_str().into());
    let names = browse::song_names(song);
    ui.set_playing_names(ModelRc::new(VecModel::from(names)));
    ui.set_playing_duration(song.duration.unwrap_or_default() as f32);
    ui.set_playing_cover(Image::default());
    if let Some(cover) = song.cover_art.clone() {
        let (index, playing) = (status.index, playing.clone());
        browse::cover(ui, browser, cover, COVER_SIZE, move |ui, image| {
            if playing.borrow().shown == index {
                ui.set_playing_cover(image);
            }
        });
    }
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

/// fonoteca-client in the system's cache folder.
fn cache() -> PathBuf {
    let folder = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|folder| folder.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    folder.join("fonoteca-client")
}
