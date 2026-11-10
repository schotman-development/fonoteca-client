//! Playing music on this device: a queue from the page on screen, the bar
//! under the pages, the now playing sheet with the queue, and the sound
//! device, volume leveling and sound processing in the settings.
//! The queue and where it is are saved on the server, for this app or any
//! other to carry on with.

use std::cell::RefCell;
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::ops::Range;
#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
#[cfg(not(target_family = "wasm"))]
use std::time::Instant;

use opensubsonic::{Child, Client};
use playback::{Gain, Leveling, Player, Quality, Repeat, Step, Track};
#[cfg(not(target_family = "wasm"))]
use playback::{NetworkProtocol, OutputDevice, OutputKind};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::active::Active;
use crate::queue::{Moved, Plan, Queue, Row, Shape, plan, run_from};
use crate::radio;
use crate::{
    AppWindow, Name, Page, PathStep, QueueLine, browse, lyrics, offline, prefs, queue_drag,
    waveform,
};
#[cfg(not(target_family = "wasm"))]
use crate::{Icons, MenuEntry, network_outputs, quality, scrobble, sound};

/// The settings' volume leveling entries, in their order.
const LEVELING: [Leveling; 3] = [Leveling::Off, Leveling::Track, Leveling::Album];
/// The repeat button's states, in the order a press goes through them.
const REPEAT: [Repeat; 3] = [Repeat::Off, Repeat::All, Repeat::One];
/// How often the bar catches up with the player.
const TICK: Duration = Duration::from_millis(250);
/// How many seconds play between saves of where the queue is, so the next
/// start picks up close to where this one stopped.
const SAVE_EVERY: f64 = 15.0;
/// The now playing sheet's cover, which the bar shows too, and the covers
/// of the queue under it, in logical pixels.
const ART_SIZE: f32 = 400.0;
const LINE_COVER_SIZE: f32 = 40.0;
/// How many songs of the queue the now playing sheet lists, from the one
/// playing on.
// ponytail: a fixed window; page through it once queues that long need reordering.
const QUEUE_SHOWN: usize = 100;
/// What the line under a station's name says.
const LIVE_RADIO: &str = "Live radio";
/// The width of the cover Control Center is given, in pixels.
#[cfg(target_os = "macos")]
const CONTROLS_COVER_PIXELS: u32 = 600;

/// How many rows of the songs played before the one playing the sheet lists.
const PREVIOUS_SHOWN: usize = 50;

#[derive(Default)]
pub struct Playing {
    player: Option<Active>,
    /// The songs queued; the one the bar shows and the steps it shows for
    /// it.
    queue: Queue,
    /// The radio station that plays in place of a queue, which is empty
    /// then: a station is live, with no length, nothing before or after it
    /// and no song to love, tag or scrobble.
    station: Option<Tuned>,
    shown: Option<usize>,
    path: Vec<Step>,
    /// How many of the queue's changes the player was handed. Until it has
    /// made them all, the track it says plays counts in the queue before.
    requeued: u64,
    /// The rows of the now playing sheet's list of the queue, and the
    /// covers it has shown, by their ids.
    listed: Vec<Row>,
    /// The rows above those, of the songs played before, in order.
    previous: Vec<Row>,
    covers: HashMap<String, Image>,
    /// Where the queue saved on the server left off, until it plays.
    resume: Option<(usize, f64)>,
    /// The track, whether it was paused and the seconds into it when the
    /// queue was last saved; and whether the server turned the saving down.
    saved: Option<(Option<usize>, bool, f64)>,
    queue_refused: bool,
    /// The sound devices by cpal id, none for the default.
    #[cfg(not(target_family = "wasm"))]
    devices: Vec<Option<String>>,
    /// What names each of `devices` beside the button that opens their menu.
    #[cfg(not(target_family = "wasm"))]
    device_titles: Vec<SharedString>,
    /// What the sound presets know each of `devices` by, from one connection
    /// to the next.
    #[cfg(not(target_family = "wasm"))]
    device_keys: Vec<String>,
    /// The device the player last said it plays on, by cpal id.
    #[cfg(not(target_family = "wasm"))]
    output: Option<String>,
    /// The devices on the network found, while the search for them runs, and how often
    /// to look at what it found.
    #[cfg(not(target_family = "wasm"))]
    networks: network_outputs::Finder,
    #[cfg(not(target_family = "wasm"))]
    network_watch: Timer,
    /// The device on the network the queue plays on instead of a sound device, while
    /// one is chosen. Not kept from one start to the next: a device is only
    /// there once found.
    #[cfg(not(target_family = "wasm"))]
    network_target: Option<network_output::Device>,
    tick: Timer,
    #[cfg(not(target_family = "wasm"))]
    scrobbler: scrobble::Scrobbler,
    /// What the Mac's media keys and Control Center have been told plays.
    #[cfg(target_os = "macos")]
    controls: crate::now_playing::Announcer<crate::desktop_media::SystemControls>,
}

/// A station playing, and the picture of it that was on its tile.
struct Tuned {
    station: radio::Station,
    cover: Image,
}

pub type Shared = Rc<RefCell<Playing>>;

/// Wires up the player, the bar and the queue, and hands back what plays,
/// for a sign out to reach the plays waiting for the server.
pub fn wire(ui: &AppWindow, browser: &browse::Shared) -> Shared {
    let playing = Shared::default();
    #[cfg(not(target_family = "wasm"))]
    {
        let browser = browser.clone();
        playing.borrow_mut().scrobbler =
            scrobble::Scrobbler::new(move || browse::play_taken(&browser));
    }
    #[cfg(test)]
    testing::keep(browser, &playing);
    #[cfg(not(target_family = "wasm"))]
    if let Err(e) = prefs::forget_waiting_listens() {
        eprintln!("The listens an earlier version kept couldn't be removed: {e}");
    }
    let saved = prefs::load();
    ui.set_leveling(saved.leveling.clamp(0, 2));
    ui.set_repeat(saved.repeat.clamp(0, 2));
    #[cfg(not(target_family = "wasm"))]
    show_stream_quality(ui, &saved);
    ui.set_can_play(true);
    ui.set_has_sound(cfg!(not(target_family = "wasm")));
    #[cfg(not(target_family = "wasm"))]
    list_devices(ui, &playing);

    ui.on_now_playing_moving_changed({
        let browser = browser.clone();
        move |moving| browse::move_playing_motion(&browser, moving)
    });
    ui.on_library_started({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || restore(&weak.unwrap(), &browser, &playing)
    });
    // A player left behind would play on beside the next start's.
    ui.on_event_loop_ended({
        let playing = playing.clone();
        move || {
            let player = playing.borrow_mut().player.take();
            player.into_iter().for_each(Active::close);
            #[cfg(target_os = "macos")]
            crate::desktop_media::remove_covers();
        }
    });
    #[cfg(target_os = "macos")]
    {
        let weak = ui.as_weak();
        crate::desktop_media::listen(move |command| {
            // Once the app has quit, nobody is left to press for.
            weak.upgrade_in_event_loop(move |ui| press(&ui, command))
                .ok();
        });
    }
    ui.on_play_tracks({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row| {
            let ui = weak.unwrap();
            let (songs, start) = browse::songs(&browser, ui.get_page(), row as usize);
            play(&ui, &browser, &playing, songs, start);
        }
    });
    ui.on_play_station({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |link| {
            let ui = weak.unwrap();
            if let Some((station, cover)) = browse::tile_station(&ui, &browser, &link) {
                tune(&ui, &browser, &playing, station, cover);
            }
        }
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
                // Short of the queue's end it goes on where it stopped; played
                // to its end, the queue starts over.
                let (start, seconds) = status.stopped.unwrap_or((0, 0.0));
                player.play(queued_tracks(&p), start, seconds);
            } else {
                player.set_paused(!status.paused);
            }
        }
    });
    ui.on_previous_track({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            if playing.borrow().station.is_some() {
                return;
            }
            resume(&weak.unwrap(), &browser, &playing, true);
            playing.borrow().player.iter().for_each(Active::previous)
        }
    });
    ui.on_next_track({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            if playing.borrow().station.is_some() {
                return;
            }
            resume(&weak.unwrap(), &browser, &playing, true);
            playing.borrow().player.iter().for_each(Active::next)
        }
    });
    ui.on_seek({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |seconds| {
            // A live stream has no place to seek to, whoever asks: the
            // lock screen's slider too.
            if playing.borrow().station.is_some() {
                return;
            }
            let seconds = f64::from(seconds);
            if let Some(at) = playing.borrow_mut().resume.as_mut() {
                at.1 = seconds;
            }
            if resume(&weak.unwrap(), &browser, &playing, true) {
                return;
            }
            #[cfg(target_os = "macos")]
            let mut p = playing.borrow_mut();
            #[cfg(not(target_os = "macos"))]
            let p = playing.borrow();
            let Some(player) = &p.player else { return };
            player.seek(seconds);
            let status = player.status();
            save(&browser, &p, &playing, status.index, seconds);
            #[cfg(target_os = "android")]
            announce(&p, status.index, status.paused || status.ended, seconds);
            #[cfg(target_os = "macos")]
            announce_to_controls(
                &browser,
                &playing,
                &mut p,
                status.index,
                status.paused || status.ended,
                seconds,
            );
        }
    });
    ui.on_toggle_playing_love({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            let ui = weak.unwrap();
            let Some(id) = shown_song_id(&playing.borrow()) else {
                return;
            };
            browse::love_playing(&ui, &browser, id, !ui.get_playing_loved());
        }
    });
    // Whichever way a song is loved or unloved, in a table or on the sheet,
    // the queue's copy and the sheet's heart follow.
    ui.on_song_loved({
        let (weak, playing) = (ui.as_weak(), playing.clone());
        move |id, loved| {
            let mut p = playing.borrow_mut();
            p.queue.set_loved(&id, loved);
            if shown_song_id(&p).as_deref() == Some(id.as_str()) {
                weak.unwrap().set_playing_loved(loved);
            }
        }
    });
    ui.on_add_playing_to_playlist({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |playlist| {
            let links = shown_song_id(&playing.borrow()).map(|id| format!("song:{id}"));
            browse::add_to_playlist(
                &weak.unwrap(),
                &browser,
                playlist.into(),
                links.into_iter().collect(),
            );
        }
    });
    ui.on_tag_playing({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            let links = shown_song_id(&playing.borrow()).map(|id| format!("song:{id}"));
            browse::start_tagging(&weak.unwrap(), &browser, links.into_iter().collect());
        }
    });
    ui.on_toggle_shuffle({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            let mut p = playing.borrow_mut();
            let current = p.shown;
            let moved = if p.queue.shuffled() {
                p.queue.unshuffle()
            } else {
                p.queue.shuffle(current, seed())
            };
            drop(p);
            requeued(&weak.unwrap(), &browser, &playing, moved);
        }
    });
    ui.on_cycle_repeat({
        let (weak, playing) = (ui.as_weak(), playing.clone());
        move || {
            let ui = weak.unwrap();
            let repeat = (ui.get_repeat() + 1) % REPEAT.len() as i32;
            ui.set_repeat(repeat);
            let mut saved = prefs::load();
            saved.repeat = repeat;
            prefs::save(&saved);
            if let Some(player) = &playing.borrow().player {
                player.set_repeat(REPEAT[repeat as usize]);
            }
        }
    });
    wire_queue(ui, browser, &playing);
    #[cfg(not(target_family = "wasm"))]
    ui.on_list_devices({
        let (weak, playing) = (ui.as_weak(), playing.clone());
        move || {
            let ui = weak.unwrap();
            search_networks(&ui, &playing);
            list_devices(&ui, &playing);
        }
    });
    #[cfg(not(target_family = "wasm"))]
    ui.on_choose_output({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |index| choose_output(&weak.unwrap(), &browser, &playing, index as usize)
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
    ui.on_choose_stream_quality(|mobile, format, bitrate| {
        let mut saved = prefs::load();
        let chosen = prefs::StreamQuality { format, bitrate };
        if mobile {
            saved.mobile_quality = chosen;
        } else {
            saved.wifi_quality = chosen;
        }
        prefs::save(&saved);
    });
    #[cfg(not(target_family = "wasm"))]
    sound::wire(ui, {
        let playing = playing.clone();
        move |dsp| {
            if let Some(player) = &playing.borrow().player {
                player.set_dsp(dsp);
            }
        }
    });
    playing
}

#[cfg(not(target_family = "wasm"))]
fn show_stream_quality(ui: &AppWindow, saved: &prefs::Prefs) {
    ui.set_has_mobile_data(quality::HAS_MOBILE_DATA);
    let wifi = quality::within_lists(saved.wifi_quality);
    ui.set_wifi_format(wifi.format);
    ui.set_wifi_bitrate(wifi.bitrate);
    let mobile = quality::within_lists(saved.mobile_quality);
    ui.set_mobile_format(mobile.format);
    ui.set_mobile_bitrate(mobile.bitrate);
}

/// Puts songs in the queue from the pages' menus, and has the now playing
/// page's list of it jump, reorder, take out and clear.
fn wire_queue(ui: &AppWindow, browser: &browse::Shared, playing: &Shared) {
    ui.on_queue_tracks({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row, count, next| {
            let ui = weak.unwrap();
            let (songs, start) = browse::songs(&browser, ui.get_page(), row as usize);
            let songs = run_from(songs, start, count as usize);
            enqueue(&ui, &browser, &playing, songs, next, None);
        }
    });
    ui.on_queue_page({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |how| {
            let ui = weak.unwrap();
            if ui.get_page() == Page::Artist {
                return queue_artist(&ui, &browser, &playing, how.to_string());
            }
            let (songs, _) = browse::songs(&browser, ui.get_page(), 0);
            queue_songs(&ui, &browser, &playing, songs, &how);
        }
    });
    ui.on_queue_link({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |link, how| {
            let Some(client) = browse::client(&browser) else {
                return;
            };
            let (weak, browser, playing) = (weak.clone(), browser.clone(), playing.clone());
            let (kept, offline) = (browse::downloads(&browser), browse::is_offline(&browser));
            slint::spawn_local(async move {
                let songs = songs_of(&client, &kept, offline, &link).await;
                let Some(ui) = weak.upgrade() else { return };
                match songs {
                    Ok(songs) if how == "play" => play(&ui, &browser, &playing, songs, 0),
                    Ok(songs) => enqueue(&ui, &browser, &playing, songs, how == "next", None),
                    Err(e) => {
                        browse::notify(&ui, &browser, format!("The songs couldn't be fetched: {e}"))
                    }
                }
            })
            .expect("the event loop is running");
        }
    });
    ui.on_queue_selection({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |how| {
            let ui = weak.unwrap();
            let links = browse::take_selection(&ui, &browser);
            let held = browse::songs_held(&browser, &links);
            queue_links(&ui, &browser, &playing, links, held, how.into());
        }
    });
    ui.on_jump_to({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row| {
            let mut p = playing.borrow_mut();
            let Some(index) = listed_songs(&p, row).map(|songs| songs.start) else {
                return;
            };
            if let Some(player) = &p.player {
                return player.jump(index);
            }
            p.resume = Some((index, 0.0));
            drop(p);
            resume(&weak.unwrap(), &browser, &playing, false);
        }
    });
    ui.on_play_queued_next({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row| {
            let mut p = playing.borrow_mut();
            let Some(current) = p.shown else { return };
            let Some(songs) = listed_songs(&p, row) else {
                return;
            };
            let moved = p.queue.move_after(songs, current);
            drop(p);
            requeued(&weak.unwrap(), &browser, &playing, moved);
        }
    });
    ui.on_move_queued({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |from, slot| {
            let mut p = playing.borrow_mut();
            let onto = queue_drag::onto_row(from, slot);
            let (Some(from), Some(onto)) = (listed_songs(&p, from), listed_songs(&p, onto)) else {
                return;
            };
            let moved = p.queue.move_onto(from, onto);
            drop(p);
            requeued(&weak.unwrap(), &browser, &playing, moved);
        }
    });
    ui.on_drop_slot(|lines, from, at| {
        let from = usize::try_from(from).unwrap_or_default();
        queue_drag::drop_slot(&parts(&lines), from, at).map_or(-1, |slot| slot as i32)
    });
    ui.on_step_slot(|lines, from, down| {
        let from = usize::try_from(from).unwrap_or_default();
        queue_drag::step_slot(&parts(&lines), from, down).map_or(-1, |slot| slot as i32)
    });
    ui.on_remove_queued({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row| {
            let mut p = playing.borrow_mut();
            let Some(songs) = listed_songs(&p, row) else {
                return;
            };
            let moved = p.queue.remove(songs);
            let emptied = p.queue.is_empty();
            drop(p);
            if emptied {
                clear(&weak.unwrap(), &browser, &playing);
            } else {
                requeued(&weak.unwrap(), &browser, &playing, moved);
            }
        }
    });
    ui.on_toggle_queued_work({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row| {
            let mut p = playing.borrow_mut();
            let Some(Shape::Work { key, .. }) = p.listed.get(row as usize).map(|row| row.shape)
            else {
                return;
            };
            p.queue.toggle_work(key);
            remember_expanded_works(&p.queue);
            list_queue(&weak.unwrap(), &browser, &mut p, &playing);
        }
    });
    ui.on_clear_queue({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || {
            let mut p = playing.borrow_mut();
            // What plays plays on; with nothing playing, all of it goes.
            let Some(current) = p.shown.filter(|&current| current < p.queue.len()) else {
                drop(p);
                return clear(&weak.unwrap(), &browser, &playing);
            };
            let moved = p.queue.keep_only(current);
            drop(p);
            requeued(&weak.unwrap(), &browser, &playing, moved);
        }
    });
}

/// Android follows its own routes, and pauses when one goes as it says it
/// is about to move the sound to the speaker. Elsewhere a device is always
/// chosen by name, so that its loss is seen rather than the system moving
/// the sound to another.
#[cfg(not(target_family = "wasm"))]
const SYSTEM_DEFAULT_LISTED: bool = cfg!(target_os = "android");
/// What the sound presets know the system's default by, where it is listed.
/// No device's key is this short: they start with their host's name.
#[cfg(not(target_family = "wasm"))]
const SYSTEM_DEFAULT_KEY: &str = "default";

/// Puts the sound devices there are now in the now playing sheet's menu, with
/// the one chosen marked. Looking for them can take a moment, so it happens
/// off the event loop.
#[cfg(not(target_family = "wasm"))]
fn list_devices(ui: &AppWindow, playing: &Shared) {
    let found = tokio::task::spawn_blocking(|| {
        let found = playback::devices();
        let system = (!SYSTEM_DEFAULT_LISTED)
            .then(|| playback::system_output(&found))
            .flatten();
        (found, system)
    });
    let (weak, playing) = (ui.as_weak(), playing.clone());
    slint::spawn_local(async move {
        let Ok((found, system)) = found.await else {
            return;
        };
        let Some(ui) = weak.upgrade() else { return };
        let networks = listed_networks(&playing.borrow());
        show_devices(&ui, &playing, [found, networks].concat(), system);
    })
    .expect("the event loop is running");
}

/// The `found` devices, after the system's default on Android, with the one
/// chosen marked; a choice saved as a route that was folded into another is
/// that other. With none saved or the one saved not there now, the
/// system's is the choice: its default on Android, elsewhere the `system`
/// device, saved as the choice where it is listed. The sound processing
/// follows the one chosen.
#[cfg(not(target_family = "wasm"))]
fn show_devices(
    ui: &AppWindow,
    playing: &Shared,
    found: Vec<OutputDevice>,
    system: Option<String>,
) {
    let saved = prefs::load().output_device;
    let playing_to = playing
        .borrow()
        .network_target
        .as_ref()
        .map(network_outputs::output_id);
    let mut devices = Vec::new();
    let mut titles = Vec::new();
    let mut keys = Vec::new();
    let mut entries = Vec::new();
    let (mut saved_at, mut system_at, mut playing_to_at) = (None, None, None);
    if SYSTEM_DEFAULT_LISTED {
        system_at = Some(entries.len());
        devices.push(None);
        titles.push(SharedString::from("System default"));
        keys.push(SYSTEM_DEFAULT_KEY.to_owned());
        entries.push((
            SharedString::from("System default"),
            SharedString::new(),
            OutputKind::Unknown,
        ));
    }
    for device in found {
        let is_saved = saved
            .as_ref()
            .is_some_and(|id| *id == device.id || device.same_as.contains(id));
        if is_saved {
            saved_at = Some(entries.len());
        }
        if system.as_ref() == Some(&device.id) {
            system_at = Some(entries.len());
        }
        if playing_to.as_ref() == Some(&device.id) {
            playing_to_at = Some(entries.len());
        }
        entries.push((device.name.into(), device.detail.into(), device.kind));
        titles.push(device.title.into());
        keys.push(device.key);
        devices.push(Some(device.id));
    }
    let chosen = playing_to_at.or(saved_at).or(system_at);
    if saved_at.is_none()
        && playing_to_at.is_none()
        && let Some(Some(device)) = chosen.and_then(|at| devices.get(at))
    {
        save_output(Some(device.clone()));
    }
    let icons = ui.global::<Icons>();
    let menu: Vec<MenuEntry> = entries
        .into_iter()
        .enumerate()
        .map(|(index, (label, detail, kind))| MenuEntry {
            id: index.to_string().into(),
            icon: kind_icon(&icons, kind),
            kind: said_kind(kind, &label).into(),
            checked: chosen == Some(index),
            label,
            detail,
            ..Default::default()
        })
        .collect();
    show_kind(ui, &icons, chosen.and_then(|at| menu.get(at)));
    ui.set_output_devices(ModelRc::new(VecModel::from(menu)));
    ui.set_output_name(
        chosen
            .and_then(|at| titles.get(at))
            .cloned()
            .unwrap_or_default(),
    );
    // A device on the network gets the file as it is: no sound preset to keep for it.
    let names = (titles.iter().zip(&keys))
        .filter(|(_, key)| !network_outputs::is_network(key))
        .map(|(title, key)| Name {
            label: title.clone(),
            link: key.into(),
        });
    sound::show_devices(ui, names.collect());
    let mut p = playing.borrow_mut();
    p.devices = devices;
    p.device_titles = titles;
    p.device_keys = keys;
    if let Some(chosen) = chosen.filter(|_| playing_to_at.is_none()) {
        follow_device(ui, &p, chosen);
    }
}

/// How often what the search for devices on the network found is looked at.
#[cfg(not(target_family = "wasm"))]
const NETWORK_LOOK_EVERY: Duration = Duration::from_secs(2);

/// Looks for devices on the network, or goes on looking, and has the menu follow
/// those that come and go. The search ends by itself once the menu has not
/// been opened for a while and no device is played to.
#[cfg(not(target_family = "wasm"))]
fn search_networks(ui: &AppWindow, playing: &Shared) {
    let mut p = playing.borrow_mut();
    p.networks.keep_searching(Instant::now());
    let (weak, shared) = (ui.as_weak(), playing.clone());
    p.network_watch
        .start(TimerMode::Repeated, NETWORK_LOOK_EVERY, move || {
            if let Some(ui) = weak.upgrade() {
                look_at_networks(&ui, &shared);
            }
        });
}

#[cfg(not(target_family = "wasm"))]
fn look_at_networks(ui: &AppWindow, playing: &Shared) {
    let looked = {
        let mut p = playing.borrow_mut();
        let playing_to = p.network_target.is_some();
        p.networks.look(Instant::now(), playing_to)
    };
    match looked {
        network_outputs::Search::Quiet => {}
        network_outputs::Search::Changed => list_devices(ui, playing),
        network_outputs::Search::Ended => {
            playing.borrow().network_watch.stop();
            list_devices(ui, playing);
        }
    }
}

/// The devices on the network found as outputs, and the one played to even when it
/// is no longer found, so the choice stays where it is.
#[cfg(not(target_family = "wasm"))]
fn listed_networks(playing: &Playing) -> Vec<OutputDevice> {
    let mut found = playing.networks.devices();
    let played_to = playing.network_target.as_ref();
    if let Some(target) = played_to.filter(|target| found.iter().all(|d| d.id != target.id)) {
        found.push(target.clone());
    }
    found.iter().map(network_outputs::output_device).collect()
}

/// Plays on the output at `index` of the menu: a sound device is kept as the
/// choice, a device on the network is only played to for as long as it is there. Moving
/// to or from a device on the network is another player, which carries on from where
/// the last was heard; between sound devices the one player moves.
#[cfg(not(target_family = "wasm"))]
fn choose_output(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, index: usize) {
    let mut p = playing.borrow_mut();
    let device = p.devices.get(index).cloned().flatten();
    let was_remote = p.player.as_ref().is_some_and(Active::is_network);
    let network = device
        .as_deref()
        .filter(|id| network_outputs::is_network(id));
    let found = network.and_then(|id| {
        let known = p
            .networks
            .devices()
            .into_iter()
            .chain(p.network_target.clone());
        known
            .into_iter()
            .find(|d| network_outputs::output_id(d) == id)
    });
    if network.is_some() && found.is_none() {
        // It went away since the menu was filled.
        drop(p);
        return list_devices(ui, playing);
    }
    p.network_target = found;
    mark_device(ui, &p, index);
    if network.is_none() {
        save_output(device.clone());
        // The device's own processing first, so that none of the old is
        // heard on it.
        follow_device(ui, &p, index);
    }
    let changes_player = (was_remote || p.network_target.is_some()) && p.station.is_none();
    let Some(player) = &p.player else { return };
    if !changes_player {
        return player.set_device(device);
    }
    let heard = player.status();
    player.stop();
    let queue = std::mem::take(&mut p.queue);
    drop(p);
    let at = (heard.index.unwrap_or(0), heard.position);
    begin(ui, browser, playing, queue, at, heard.paused || heard.ended);
}

/// Has the sound processing follow the `chosen` device as it becomes the
/// output: the preset linked to it, or no processing at all.
#[cfg(not(target_family = "wasm"))]
fn follow_device(ui: &AppWindow, playing: &Playing, chosen: usize) {
    let followed = (playing.device_keys.get(chosen)).and_then(|key| sound::use_device(ui, key));
    if let (Some(dsp), Some(player)) = (followed, &playing.player) {
        player.set_dsp(dsp);
    }
}

/// Keeps `device` as the one chosen, by cpal id.
#[cfg(not(target_family = "wasm"))]
fn save_output(device: Option<String>) {
    let mut saved = prefs::load();
    saved.output_device = device;
    prefs::save(&saved);
}

/// Moves the menu's mark, the name beside the button and its kind on the
/// button, to the device `chosen`.
#[cfg(not(target_family = "wasm"))]
fn mark_device(ui: &AppWindow, playing: &Playing, chosen: usize) {
    let entries = ui.get_output_devices();
    for (index, mut entry) in entries.iter().enumerate() {
        entry.checked = index == chosen;
        entries.set_row_data(index, entry);
    }
    show_kind(ui, &ui.global::<Icons>(), entries.row_data(chosen).as_ref());
    if let Some(title) = playing.device_titles.get(chosen) {
        ui.set_output_name(title.clone());
    }
}

/// Puts the kind of the device `chosen` from the menu on the button that
/// opens it; with none chosen, a plain sound device.
#[cfg(not(target_family = "wasm"))]
fn show_kind(ui: &AppWindow, icons: &Icons, chosen: Option<&MenuEntry>) {
    let (icon, kind) = chosen.map_or_else(
        || (kind_icon(icons, OutputKind::Unknown), SharedString::new()),
        |entry| (entry.icon.clone(), entry.kind.clone()),
    );
    ui.set_output_icon(icon);
    ui.set_output_kind(kind);
}

/// One of no known kind gets the plain sound device, never another kind's.
#[cfg(not(target_family = "wasm"))]
fn kind_icon(icons: &Icons, kind: OutputKind) -> SharedString {
    match kind {
        OutputKind::Unknown => icons.get_speaker(),
        OutputKind::Speaker => icons.get_loudspeaker(),
        OutputKind::Headphones => icons.get_headphones(),
        OutputKind::Headset => icons.get_headset(),
        OutputKind::Bluetooth => icons.get_bluetooth(),
        OutputKind::Usb => icons.get_usb(),
        OutputKind::Hdmi => icons.get_display(),
        OutputKind::Network(NetworkProtocol::Cast) => icons.get_cast(),
    }
}

/// What a screen reader says a device of the `kind` is before its `name`:
/// nothing where the name says it already, as Android's for wired
/// headphones does.
#[cfg(not(target_family = "wasm"))]
fn said_kind(kind: OutputKind, name: &str) -> &'static str {
    if kind.name() == name { "" } else { kind.name() }
}

/// Plays `songs` from the `start`th, in place of what played: shuffled, the
/// `start`th first, when what played was.
fn play(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    songs: Vec<Child>,
    start: usize,
) {
    let (songs, start) = playable(ui, browser, songs, start);
    if songs.is_empty() {
        return;
    }
    let shuffled = playing.borrow().queue.shuffled();
    let (queue, start) = page_queue(songs, start, shuffled);
    begin(ui, browser, playing, queue, (start, 0.0), false);
}

/// The queue of a page's `songs` and the song to play from in it: the
/// `start`th, or first when the page is `shuffled`.
fn page_queue(songs: Vec<Child>, start: usize, shuffled: bool) -> (Queue, usize) {
    let mut queue = Queue::from_page(songs, start);
    if shuffled && start < queue.len() {
        queue.shuffle(Some(start), seed());
        return (queue, 0);
    }
    (queue, start)
}

/// Plays or queues `songs` as `how` says: "play", "next", "queue", or
/// "shuffle-now", "shuffle-next" or "shuffle-last" for a random order.
fn queue_songs(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    songs: Vec<Child>,
    how: &str,
) {
    let (songs, _) = playable(ui, browser, songs, 0);
    if songs.is_empty() {
        return;
    }
    let queue_is_empty = playing.borrow().queue.is_empty();
    match plan(how, songs, queue_is_empty, seed()) {
        Plan::Play(songs) => play(ui, browser, playing, songs, 0),
        Plan::PlayShuffled(songs) => play_shuffled(ui, browser, playing, songs),
        Plan::Enqueue {
            songs,
            next,
            shuffle_seed,
        } => enqueue(ui, browser, playing, songs, next, shuffle_seed),
    }
}

/// Does `how` with every song of the artist's own albums.
fn queue_artist(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, how: String) {
    let albums = browse::own_albums(browser);
    if albums.is_empty() {
        return browse::notify(ui, browser, "They have no albums of their own.".into());
    }
    browse::notify(ui, browser, format!("Fetching {} albums…", albums.len()));
    queue_links(ui, browser, playing, albums, HashMap::new(), how);
}

/// Does `how` with the songs of `links` in their order, once all of them
/// have arrived: those `held` by link at once, the rest from the server or
/// this device. A shuffle across them needs them all, and a link fetched per
/// call keeps the window free meanwhile. Links whose songs couldn't be
/// fetched are left out, and a notice says how many.
fn queue_links(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    links: Vec<String>,
    held: HashMap<String, Child>,
    how: String,
) {
    let Some(client) = browse::client(browser) else {
        return;
    };
    let (kept, offline) = (browse::downloads(browser), browse::is_offline(browser));
    let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
    slint::spawn_local(async move {
        let mut songs = Vec::new();
        let mut failures = Vec::new();
        for link in links {
            if let Some(song) = held.get(&link) {
                songs.push(song.clone());
                continue;
            }
            match songs_of(&client, &kept, offline, &link).await {
                Ok(more) => songs.extend(more),
                Err(e) => failures.push(e),
            }
        }
        let Some(ui) = weak.upgrade() else { return };
        queue_songs(&ui, &browser, &playing, songs, &how);
        if let Some(last) = failures.last() {
            let items = match failures.len() {
                1 => "1 item".to_owned(),
                count => format!("{count} items"),
            };
            let text = format!("The songs of {items} couldn't be fetched: {last}");
            browse::notify(&ui, &browser, text);
        }
    })
    .expect("the event loop is running");
}

/// Plays `songs` in a random order in place of what played, with the
/// queue's shuffle on so that turning it off puts them back in order.
fn play_shuffled(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, songs: Vec<Child>) {
    let mut queue = Queue::from_page(songs, 0);
    queue.shuffle(None, seed());
    begin(ui, browser, playing, queue, (0, 0.0), false);
}

/// Plays the queue saved on the server from where it left off, `paused` or
/// not; false when there is none waiting.
fn resume(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, paused: bool) -> bool {
    let mut p = playing.borrow_mut();
    let Some(at) = p.resume else { return false };
    let queue = std::mem::take(&mut p.queue);
    drop(p);
    begin(ui, browser, playing, queue, at, paused);
    true
}

/// Plays `queue` from `at`, a track and seconds into it, in place of what
/// played, `paused` or not.
fn begin(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    queue: Queue,
    (start, seconds): (usize, f64),
    paused: bool,
) {
    let Some(client) = browse::client(browser).filter(|_| start < queue.len()) else {
        return;
    };
    // Each queue gets a player with the client signed in now. The last one
    // lets the device go first, since a device played on directly can only
    // be open once.
    playing.borrow_mut().player = None;
    let player = song_output(ui, browser, playing, client);
    player.set_leveling(LEVELING[ui.get_leveling().clamp(0, 2) as usize]);
    player.set_repeat(REPEAT[ui.get_repeat().clamp(0, 2) as usize]);
    player.play(tracks(&queue), start, seconds);
    // Before any of it is heard: a track starts once some of it is in.
    player.set_paused(paused);
    adopt(ui, browser, playing, player, queue, None);
}

/// Plays the radio `station` in place of what played, from now.
fn tune(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    station: radio::Station,
    cover: Image,
) {
    playing.borrow_mut().player = None;
    #[cfg(not(target_family = "wasm"))]
    if let Some(device) = &playing.borrow().network_target {
        // A live stream is played as it arrives, which a file served to a
        // device is not.
        let text = format!("Radio plays on this device, not on {}.", device.name);
        browse::notify(ui, browser, text);
    }
    let player = Active::Local(station_player(&station));
    player.play(vec![radio::track(&station)], 0, 0.0);
    adopt(
        ui,
        browser,
        playing,
        player,
        Queue::default(),
        Some(Tuned { station, cover }),
    );
}

/// The player for songs from `client`: on the device on the network chosen, or if none
/// is or it can't be played to, on this device's sound output.
#[cfg(not(target_family = "wasm"))]
fn song_output(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    client: Arc<Client>,
) -> Active {
    let target = playing.borrow().network_target.clone();
    let Some(device) = target else {
        return Active::Local(song_player(client, browser));
    };
    let fetch = fetch(client.clone(), browse::downloads(browser));
    match network_outputs::player(&device, fetch, cache()) {
        Ok(player) => Active::Network(player),
        Err(problem) => {
            let text = format!("{} can't be played to: {problem}", device.name);
            browse::notify(ui, browser, text);
            Active::Local(song_player(client, browser))
        }
    }
}

#[cfg(target_family = "wasm")]
fn song_output(
    _ui: &AppWindow,
    browser: &browse::Shared,
    _playing: &Shared,
    client: Arc<Client>,
) -> Active {
    Active::Local(song_player(client, browser))
}

/// A player for songs from `client`, or from their downloads, on the sound
/// device and with the sound processing the settings pick.
#[cfg(not(target_family = "wasm"))]
fn song_player(client: Arc<Client>, browser: &browse::Shared) -> Player {
    let fetch = fetch(client, browse::downloads(browser));
    let player = new_player(prefs::load().output_device, fetch);
    player.set_dsp(sound::dsp());
    player
}

/// A player for the radio `station`, on the sound device and with the sound
/// processing the settings pick.
#[cfg(not(target_family = "wasm"))]
fn station_player(station: &radio::Station) -> Player {
    let fetch = radio::fetch(station.stream.clone(), cache(), browse::USER_AGENT);
    let player = new_player(prefs::load().output_device, fetch);
    player.set_dsp(sound::dsp());
    player
}

/// The browser fetches and plays nothing: its player only goes through the
/// queue, for the bar and now playing to show what would play.
#[cfg(target_family = "wasm")]
fn song_player(_client: Arc<Client>, _browser: &browse::Shared) -> Player {
    Player::silent()
}

#[cfg(target_family = "wasm")]
fn station_player(_station: &radio::Station) -> Player {
    Player::silent()
}

/// Has `player` play `queue`, or else the `station`, with the bar showing
/// it from the start.
fn adopt(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    player: Active,
    queue: Queue,
    station: Option<Tuned>,
) {
    let mut p = playing.borrow_mut();
    p.player = Some(player);
    p.queue = queue;
    p.station = station;
    p.requeued = 0;
    p.resume = None;
    p.saved = None;
    p.shown = None;
    p.path.clear();
    // A station is on the bar at once: the player may have given up by the
    // first look (without a sound device), which shows no song to put there.
    if let Some(tuned) = &p.station {
        show_station(ui, browser, tuned);
    }
    let (weak, pages, ticked) = (ui.as_weak(), browser.clone(), playing.clone());
    p.tick.start(TimerMode::Repeated, TICK, move || {
        if let Some(ui) = weak.upgrade() {
            show(&ui, &pages, &ticked);
        }
    });
    drop(p);
    show(ui, browser, playing);
}

/// Puts `songs` in the queue after the song playing, or at its end, and says
/// so; with nothing queued, plays them, and with all of it played, plays
/// on from them.
fn enqueue(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &Shared,
    songs: Vec<Child>,
    next: bool,
    shuffle_seed: Option<u64>,
) {
    let (songs, _) = playable(ui, browser, songs, 0);
    let what = match &songs[..] {
        [] => return,
        [song] => song.title.clone(),
        songs => format!("{} tracks", songs.len()),
    };
    let mut p = playing.borrow_mut();
    if p.queue.is_empty() {
        drop(p);
        return play(ui, browser, playing, songs, 0);
    }
    let at = match p.shown {
        Some(current) if next => current + 1,
        _ => p.queue.len(),
    };
    let moved = p.queue.insert(at, songs, shuffle_seed);
    let ended = p
        .player
        .as_ref()
        .is_some_and(|player| player.status().ended);
    drop(p);
    requeued(ui, browser, playing, moved);
    if ended {
        playing
            .borrow()
            .player
            .iter()
            .for_each(|player| player.jump(at));
    }
    let text = if next {
        format!("Playing {what} next.")
    } else {
        format!("Added {what} to the queue.")
    };
    browse::notify(ui, browser, text);
}

/// Has the player, the bar and the queue's list follow a change to the
/// queue, where `moved` gives each song's place after it.
fn requeued(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, moved: Moved) {
    let mut guard = playing.borrow_mut();
    let p = &mut *guard;
    let place = |index: usize| moved.get(index).copied().flatten();
    // A song taken out gives its place to the first after it that stayed,
    // as in the player.
    let stayed = |index: usize| moved.get(index..).into_iter().flatten().find_map(|&at| at);
    let taken_out = p.shown.is_some_and(|shown| place(shown).is_none());
    p.shown = p.shown.and_then(stayed);
    // The song playing went with none after it, and the player stops: the
    // queue waits from its start, as one restored does.
    if taken_out && p.shown.is_none() && p.player.is_some() {
        let queue = std::mem::take(&mut p.queue);
        drop(guard);
        return begin(ui, browser, playing, queue, (0, 0.0), true);
    }
    if let Some((index, seconds)) = p.resume {
        p.resume = Some(match place(index) {
            Some(index) => (index, seconds),
            None => (stayed(index).unwrap_or(0), 0.0),
        });
        p.shown = p.resume.map(|(index, _)| index);
    }
    if let Some(player) = &p.player {
        player.requeue(tracks(&p.queue), moved);
        p.requeued += 1;
    }
    if let Some(index) = p.shown.filter(|_| taken_out) {
        show_song(ui, browser, p, playing, index);
    }
    // Saves the queue as it is now on the next tick.
    p.saved = None;
    list_queue(ui, browser, p, playing);
}

/// Takes the queue away, and the bar with it, and clears the one saved on
/// the server.
fn clear(ui: &AppWindow, browser: &browse::Shared, playing: &Shared) {
    #[cfg(target_os = "android")]
    crate::media::hide();
    let mut p = playing.borrow_mut();
    #[cfg(target_os = "macos")]
    hide_from_controls(&mut p);
    p.player = None;
    p.tick.stop();
    p.queue = Queue::default();
    p.station = None;
    p.resume = None;
    p.shown = None;
    p.saved = None;
    save(browser, &p, playing, None, 0.0);
    list_queue(ui, browser, &mut p, playing);
    browse::stop_playing_motion(browser);
    ui.set_playing(false);
}

/// Shows `scrobbler` what plays, `song` being the track playing, and logs
/// it as played on this device once it counts, when the server is told too,
/// at once or once it can be reached.
#[cfg(not(target_family = "wasm"))]
fn observe(
    browser: &browse::Shared,
    scrobbler: &mut scrobble::Scrobbler,
    song: Option<&Child>,
    position: f64,
    paused: bool,
) {
    let server = server(browser);
    let tells_server = server.is_some();
    let counted = scrobbler.observe(song, position, paused, server);
    if let (Some(song), Some(listened_at)) = (song, counted) {
        browse::log_play(browser, song, listened_at);
        if tells_server {
            browse::told_play(browser);
        }
    }
}

/// How many plays wait for the server of the account signed in.
#[cfg(not(target_family = "wasm"))]
pub fn waiting_plays(playing: &Shared, browser: &browse::Shared) -> usize {
    match browse::account(browser) {
        Some(account) => playing.borrow_mut().scrobbler.waiting(&account),
        None => 0,
    }
}

#[cfg(target_family = "wasm")]
pub fn waiting_plays(_playing: &Shared, _browser: &browse::Shared) -> usize {
    0
}

/// Tells the server of the plays waiting for the account signed in, once
/// more before it signs out, after the send under way if there is one.
#[cfg(not(target_family = "wasm"))]
pub async fn send_waiting_plays(playing: Shared, browser: browse::Shared) {
    let Some(server) = server(&browser) else {
        return;
    };
    let sending = playing.borrow_mut().scrobbler.send_last(server);
    sending.await;
}

#[cfg(target_family = "wasm")]
pub async fn send_waiting_plays(_playing: Shared, _browser: browse::Shared) {}

/// Forgets the plays still waiting for the server of the account signed in,
/// as it signs out.
#[cfg(not(target_family = "wasm"))]
pub fn forget_waiting_plays(playing: &Shared, browser: &browse::Shared) -> std::io::Result<()> {
    match browse::account(browser) {
        Some(account) => playing.borrow_mut().scrobbler.forget(&account),
        None => Ok(()),
    }
}

#[cfg(target_family = "wasm")]
pub fn forget_waiting_plays(_playing: &Shared, _browser: &browse::Shared) -> std::io::Result<()> {
    Ok(())
}

/// The server signed in to, which the scrobbler tells of what plays.
#[cfg(not(target_family = "wasm"))]
fn server(browser: &browse::Shared) -> Option<scrobble::Server> {
    Some(scrobble::Server {
        client: browse::client(browser)?,
        account: browse::account(browser)?,
    })
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
        #[cfg(target_os = "macos")]
        hide_from_controls(&mut p);
        return;
    }
    let Some(player) = &p.player else { return };
    let status = player.status();
    if status.requeues != p.requeued {
        return;
    }
    let state = (status.index, status.paused || status.ended);
    #[cfg(not(target_family = "wasm"))]
    {
        let scrobbling = &mut *p;
        let song = status.index.and_then(|index| scrobbling.queue.get(index));
        observe(
            browser,
            &mut scrobbling.scrobbler,
            song,
            status.position,
            state.1,
        );
    }
    let last = p.saved;
    #[cfg(target_os = "android")]
    if last.map(|(index, paused, _)| (index, paused)) != Some(state) {
        announce(&p, state.0, state.1, status.position);
    }
    let due = last.is_none_or(|(index, paused, at)| {
        (index, paused) != state || !paused && (status.position - at).abs() >= SAVE_EVERY
    });
    // The save's cadence also corrects the position Control Center counts
    // on its own.
    #[cfg(target_os = "macos")]
    if due {
        announce_to_controls(browser, playing, &mut p, state.0, state.1, status.position);
    }
    if due {
        save(browser, &p, playing, status.index, status.position);
        p.saved = Some((state.0, state.1, status.position));
    }
    // Outside Android it plays on the system's device in place of one not
    // there, which is the choice from then on.
    #[cfg(not(target_family = "wasm"))]
    if !SYSTEM_DEFAULT_LISTED && status.output.is_some() && status.output != p.output {
        p.output = status.output.clone();
        if prefs::load().output_device != status.output {
            save_output(status.output.clone());
            list_devices(ui, playing);
        }
    }
    ui.set_playing(true);
    ui.set_playing_paused(status.paused || status.ended);
    ui.set_playing_position(status.position as f32);
    lyrics::follow(ui);
    let problem = match (status.problem, &status.skipped) {
        (Some(problem), _) => problem,
        (None, Some((index, why))) => match p.queue.get(*index) {
            Some(song) => format!("Skipped {}: {why}", song.title),
            None => why.clone(),
        },
        (None, None) => String::new(),
    };
    ui.set_playing_problem(problem.into());
    ui.set_playing_level(status.quality.map_or(-1, |quality| quality as i32));
    ui.set_playing_quality(status.quality.map_or("", quality_name).into());
    if status.path != p.path {
        let steps: Vec<PathStep> = status
            .path
            .iter()
            .map(|step| PathStep {
                text: step.text.as_str().into(),
                quality: step.quality as i32,
            })
            .collect();
        ui.set_playing_path(ModelRc::new(VecModel::from(steps)));
        p.path = status.path;
    }
    if status.index == p.shown {
        return;
    }
    p.shown = status.index;
    if let Some(tuned) = &p.station {
        show_station(ui, browser, tuned);
    } else if let Some(index) = status.index {
        show_song(ui, browser, &p, playing, index);
    }
    list_queue(ui, browser, &mut p, playing);
}

/// The signal path's light by name, as Roon names it.
fn quality_name(quality: Quality) -> &'static str {
    match quality {
        Quality::Lossless => "Lossless",
        Quality::Enhanced => "Enhanced",
        Quality::High => "High quality",
        Quality::Low => "Low quality",
    }
}

/// Fetches the waveform of the song the bar shows, for a seek bar the
/// settings just switched back from the simple one.
pub fn show_waveform(ui: &AppWindow, browser: &browse::Shared, playing: &Shared) {
    let Some(id) = shown_song_id(&playing.borrow()) else {
        return;
    };
    let (still_playing, shown) = (playing.clone(), id.clone());
    waveform::show(ui, browse::client(browser), id, move || {
        shown_song_id(&still_playing.borrow()).is_some_and(|id| id == shown)
    });
}

/// The id of the song the bar shows.
fn shown_song_id(p: &Playing) -> Option<String> {
    p.shown
        .and_then(|shown| p.queue.get(shown))
        .map(|song| song.id.clone())
}

/// Puts the station on the bar and the now playing page, as live: its name
/// and logo, with no length, album or song to act on.
fn show_station(ui: &AppWindow, browser: &browse::Shared, tuned: &Tuned) {
    ui.set_playing_live(true);
    ui.set_playing_title(tuned.station.name.as_str().into());
    ui.set_playing_movement("".into());
    ui.set_playing_movement_place("".into());
    ui.set_queue_holds_works(false);
    let line = Name {
        label: LIVE_RADIO.into(),
        link: SharedString::new(),
    };
    ui.set_playing_names(ModelRc::new(VecModel::from(vec![line])));
    ui.set_playing_album(Name::default());
    ui.set_playing_loved(false);
    ui.set_playing_shown(ui.get_playing_shown() + 1);
    ui.set_playing_duration(0.0);
    ui.set_playing_waveform(ModelRc::default());
    ui.set_playing_cover(tuned.cover.clone());
    ui.set_playing_art(tuned.cover.clone());
    ui.set_playing_banner(Image::default());
    lyrics::clear(ui);
    browse::stop_playing_motion(browser);
}

/// Puts the `index`th song of the queue on the bar and the now playing
/// page, with its cover once it comes.
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
    ui.set_playing_live(false);
    let work_line = browse::work_line(|at| p.queue.get(at), index);
    ui.set_playing_title(
        work_line
            .as_ref()
            .map_or(song.title.as_str(), |line| line.work.as_str())
            .into(),
    );
    ui.set_playing_movement(
        work_line
            .as_ref()
            .map_or("", |line| line.movement.as_str())
            .into(),
    );
    ui.set_playing_movement_place(
        work_line
            .as_ref()
            .map_or("", |line| line.place.as_str())
            .into(),
    );
    let names = browse::song_names(song);
    ui.set_playing_names(ModelRc::new(VecModel::from(names)));
    ui.set_playing_album(Name {
        label: song.album.as_deref().unwrap_or_default().into(),
        link: song
            .album_id
            .as_ref()
            .map_or_else(SharedString::new, |id| format!("album:{id}").into()),
    });
    ui.set_playing_loved(song.starred.is_some());
    ui.set_queue_holds_works(p.queue.songs().any(|song| !song.works.is_empty()));
    ui.set_playing_shown(ui.get_playing_shown() + 1);
    ui.set_playing_duration(song.duration.unwrap_or_default() as f32);
    ui.set_playing_cover(Image::default());
    let still_playing = |playing: &Shared, id: &String| {
        let (playing, id) = (playing.clone(), id.clone());
        move || shown_song_id(&playing.borrow()).is_some_and(|shown| shown == id)
    };
    waveform::show(
        ui,
        browse::client(browser),
        song.id.clone(),
        still_playing(playing, &song.id),
    );
    lyrics::show(
        ui,
        browse::client(browser),
        song,
        still_playing(playing, &song.id),
    );
    // The banner of the last artist stays up until this one's comes.
    match song_artist_id(song).filter(|_| browse::offers_artist_banners(browser)) {
        Some(artist) => {
            let still_playing = still_playing(playing, &song.id);
            browse::playing_banner(ui, browser, artist, move |ui, banner| {
                if still_playing() {
                    ui.set_playing_banner(banner);
                }
            });
        }
        None => ui.set_playing_banner(Image::default()),
    }
    browse::playing_motion(ui, browser, song.album_id.clone(), ART_SIZE, |ui, frame| {
        ui.set_playing_art(frame);
    });
    // The motion cover of the album goes on across its tracks, held or
    // moving, so the still cover only stands in until its first frame comes,
    // or when there are none.
    if !browse::playing_motion_framed(browser) {
        ui.set_playing_art(Image::default());
    }
    if let Some(cover) = song.cover_art.clone() {
        let (playing, browser_playing) = (playing.clone(), browser.clone());
        browse::cover(ui, browser, cover, ART_SIZE, move |ui, image| {
            if playing.borrow().shown == Some(index) {
                ui.set_playing_cover(image.clone());
                if !browse::playing_motion_framed(&browser_playing) {
                    ui.set_playing_art(image);
                }
            }
        });
    }
}

/// The id of the first of a song's artists, or of its one artist on servers
/// without the list.
fn song_artist_id(song: &Child) -> Option<String> {
    song.artists
        .first()
        .map(|artist| artist.id.clone())
        .or_else(|| song.artist_id.clone())
        .filter(|id| !id.is_empty())
}

/// Which of the sheet's `lines` are movements under an open work.
fn parts(lines: &ModelRc<QueueLine>) -> Vec<bool> {
    lines.iter().map(|line| line.part).collect()
}

/// The songs the `row` of the sheet's list stands for. The rows of the
/// songs played before count back from the one playing: -1 is the one
/// above it.
fn listed_songs(p: &Playing, row: i32) -> Option<Range<usize>> {
    let row = match usize::try_from(row) {
        Ok(row) => p.listed.get(row),
        Err(_) => p
            .previous
            .len()
            .checked_sub(row.unsigned_abs() as usize)
            .and_then(|row| p.previous.get(row)),
    };
    row.map(|row| row.songs.clone())
}

/// Lists the queue on the now playing sheet from the work or song playing
/// on, and above it the songs played before, with the covers it has shown
/// before and the others once they come.
fn list_queue(ui: &AppWindow, browser: &browse::Shared, p: &mut Playing, playing: &Shared) {
    p.listed = p.queue.rows(p.shown, QUEUE_SHOWN);
    let (previous, earlier) = p.queue.previous_rows(p.shown, PREVIOUS_SHOWN);
    p.previous = previous;
    let lines: Vec<QueueLine> = p.listed.iter().map(|row| queue_line(p, row)).collect();
    let before: Vec<QueueLine> = p.previous.iter().map(|row| queue_line(p, row)).collect();
    let listed_to = p.listed.last().map_or(0, |row| row.songs.end);
    ui.set_queue_more((p.queue.len() - listed_to) as i32);
    ui.set_queue_earlier(earlier as i32);
    ui.set_shuffled(p.queue.shuffled());
    let model = Rc::new(VecModel::from(lines));
    let before_model = Rc::new(VecModel::from(before));
    ui.set_queue_lines(ModelRc::from(model.clone()));
    let playing_rows = queue_drag::playing_rows(&parts(&ui.get_queue_lines()));
    ui.set_queue_playing_rows(playing_rows as i32);
    ui.set_previous_lines(ModelRc::from(before_model.clone()));
    show_covers(ui, browser, p, playing, &p.listed, model);
    show_covers(ui, browser, p, playing, &p.previous, before_model);
}

/// Fetches the covers of `rows` that haven't been shown, and puts each on
/// its line of `model` once it comes.
fn show_covers(
    ui: &AppWindow,
    browser: &browse::Shared,
    p: &Playing,
    playing: &Shared,
    rows: &[Row],
    model: Rc<VecModel<QueueLine>>,
) {
    let wanted: Rc<Vec<Option<String>>> = Rc::new(
        rows.iter()
            .map(|row| {
                p.queue
                    .get(row.songs.start)
                    .and_then(|song| song.cover_art.clone())
            })
            .collect(),
    );
    let missing: HashSet<String> = wanted
        .iter()
        .flatten()
        .filter(|cover| !p.covers.contains_key(*cover))
        .cloned()
        .collect();
    for cover in missing {
        let (cover, wanted, model, playing) = (
            cover.clone(),
            wanted.clone(),
            model.clone(),
            playing.clone(),
        );
        browse::cover(
            ui,
            browser,
            cover.clone(),
            LINE_COVER_SIZE,
            move |_, image| {
                for (row, _) in wanted
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| id.as_ref() == Some(&cover))
                {
                    if let Some(mut line) = model.row_data(row) {
                        line.cover = image.clone();
                        model.set_row_data(row, line);
                    }
                }
                playing.borrow_mut().covers.insert(cover, image);
            },
        );
    }
}

/// The line of the sheet's list for a `row`.
fn queue_line(p: &Playing, row: &Row) -> QueueLine {
    let songs: Vec<&Child> = row.songs.clone().filter_map(|at| p.queue.get(at)).collect();
    let song = songs[0];
    let playing_at = p.shown.filter(|shown| row.songs.contains(shown));
    let (title, details) = match row.shape {
        Shape::Work { .. } => (
            work_name(song),
            work_details(&songs, playing_at.map(|at| at - row.songs.start)),
        ),
        Shape::Movement => (movement_name(song), queue_details(song)),
        Shape::Song => (song.title.clone(), queue_details(song)),
    };
    QueueLine {
        title: title.into(),
        details: details.into(),
        cover: song
            .cover_art
            .as_ref()
            .and_then(|cover| p.covers.get(cover))
            .cloned()
            .unwrap_or_default(),
        album: song
            .album_id
            .as_ref()
            .map_or_else(SharedString::new, |id| format!("album:{id}").into()),
        current: playing_at.is_some(),
        work: matches!(row.shape, Shape::Work { .. }),
        expanded: matches!(row.shape, Shape::Work { expanded: true, .. }),
        part: row.shape == Shape::Movement,
    }
}

fn work_name(song: &Child) -> String {
    song.works
        .first()
        .map_or_else(|| song.title.clone(), |work| work.name.clone())
}

fn movement_name(song: &Child) -> String {
    song.movements
        .first()
        .map_or_else(|| song.title.clone(), |movement| movement.name.clone())
}

/// A work's movements, its artists and length; while one of them plays
/// (`playing`, its place among them), which, so a closed row still says
/// where the sheet is.
fn work_details(movements: &[&Child], playing: Option<usize>) -> String {
    let count = movements.len();
    let first = match playing {
        Some(at) => format!(
            "Playing {} ({} of {count})",
            movement_name(movements[at]),
            at + 1
        ),
        None => format!("{count} movements"),
    };
    let length = movements
        .iter()
        .map(|movement| movement.duration)
        .sum::<Option<i64>>();
    [first, artists_and_length(movements[0], length)]
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A queued song's artists and length.
fn queue_details(song: &Child) -> String {
    artists_and_length(song, song.duration)
}

/// `song`'s artists, and `seconds` if there are any.
fn artists_and_length(song: &Child, seconds: Option<i64>) -> String {
    let artists: Vec<String> = browse::song_names(song)
        .into_iter()
        .map(|name| name.label.into())
        .collect();
    match seconds {
        Some(seconds) => format!("{} · {}", artists.join(", "), browse::duration(seconds)),
        None => artists.join(", "),
    }
}

/// Tells Android what plays, for its notification and lock screen.
#[cfg(target_os = "android")]
fn announce(p: &Playing, index: Option<usize>, paused: bool, seconds: f64) {
    if let Some(tuned) = &p.station {
        return crate::media::show(&tuned.station.name, LIVE_RADIO, !paused, 0.0, 0.0);
    }
    let Some(song) = index.and_then(|index| p.queue.get(index)) else {
        return crate::media::hide();
    };
    let artists = song.artist.as_deref().unwrap_or_default();
    let duration = song.duration.unwrap_or_default() as f64;
    crate::media::show(&song.title, artists, !paused, duration, seconds);
}

/// What the media controls show of what plays, with the id of its cover.
#[cfg(target_os = "macos")]
fn controls_track(
    p: &Playing,
    index: Option<usize>,
) -> Option<(String, crate::now_playing::Track, Option<String>)> {
    use crate::now_playing::Track;
    if let Some(tuned) = &p.station {
        let track = Track {
            title: tuned.station.name.clone(),
            artist: LIVE_RADIO.into(),
            album: String::new(),
            duration: 0.0,
            cover: None,
            live: true,
        };
        return Some((format!("radio:{}", tuned.station.name), track, None));
    }
    let song = index.and_then(|index| p.queue.get(index))?;
    let track = Track {
        title: song.title.clone(),
        artist: song.artist.clone().unwrap_or_default(),
        album: song.album.clone().unwrap_or_default(),
        duration: song.duration.unwrap_or_default() as f64,
        cover: None,
        live: false,
    };
    Some((song.id.clone(), track, song.cover_art.clone()))
}

/// Tells the Mac's media keys and Control Center what plays, the title and
/// artist at once and the cover when it has come.
#[cfg(target_os = "macos")]
fn announce_to_controls(
    browser: &browse::Shared,
    playing: &Shared,
    p: &mut Playing,
    index: Option<usize>,
    paused: bool,
    seconds: f64,
) {
    let Some((id, track, cover)) = controls_track(p, index) else {
        return hide_from_controls(p);
    };
    if !p.controls.announce(&id, &track, !paused, seconds) {
        return;
    }
    if let Some(cover) = cover {
        show_cover_on_controls(browser, playing, id, cover, track);
    }
}

/// Fetches the cover `cover` of the track `id`, for the controls to show
/// from a file: they are never given the server's address, which carries
/// the account's credentials.
#[cfg(target_os = "macos")]
fn show_cover_on_controls(
    browser: &browse::Shared,
    playing: &Shared,
    id: String,
    cover: String,
    track: crate::now_playing::Track,
) {
    let (browser, playing) = (browser.clone(), playing.clone());
    slint::spawn_local(async move {
        let Some(client) = browse::client(&browser) else {
            return;
        };
        let bytes = match client
            .get_cover_art(&cover, Some(CONTROLS_COVER_PIXELS))
            .await
        {
            Ok(bytes) => bytes,
            Err(problem) => return eprintln!("The cover for the media controls: {problem}"),
        };
        // A cover that comes after the controls were cleared or moved on
        // would only leave a file behind.
        if !playing.borrow().controls.shows(&id) {
            return;
        }
        let path = match crate::desktop_media::store_cover(&cover, &bytes) {
            Ok(path) => path,
            Err(problem) => return eprintln!("The cover for the media controls: {problem}"),
        };
        let mut p = playing.borrow_mut();
        let Some(status) = p.player.as_ref().map(Active::status) else {
            return;
        };
        let track = crate::now_playing::Track {
            cover: Some(path),
            ..track
        };
        let playing = !(status.paused || status.ended);
        p.controls
            .cover_ready(&id, &track, playing, status.position);
    })
    .expect("the event loop is running");
}

/// Takes what was told to the media controls down, with the covers kept
/// for them, and lets the keys go back to whichever app played last.
#[cfg(target_os = "macos")]
fn hide_from_controls(p: &mut Playing) {
    p.controls.clear();
    crate::desktop_media::remove_covers();
}

/// What a media key or Control Center's button asked for, done as the bar's
/// own controls do it.
#[cfg(target_os = "macos")]
fn press(ui: &AppWindow, command: crate::now_playing::Command) {
    use crate::now_playing::{Action, action};
    match action(command, ui.get_playing_paused()) {
        Some(Action::TogglePause) => ui.invoke_toggle_pause(),
        Some(Action::Next) => ui.invoke_next_track(),
        Some(Action::Previous) => ui.invoke_previous_track(),
        Some(Action::Seek(seconds)) => ui.invoke_seek(seconds as f32),
        None => {}
    }
}

/// Takes up the queue saved on the server, paused on the bar where it left
/// off, in place of anything from the account signed in before.
fn restore(ui: &AppWindow, browser: &browse::Shared, playing: &Shared) {
    #[cfg(target_os = "android")]
    crate::media::hide();
    #[cfg(target_os = "macos")]
    hide_from_controls(&mut playing.borrow_mut());
    #[cfg(not(target_family = "wasm"))]
    if let Some(server) = server(browser) {
        let sending = playing.borrow_mut().scrobbler.send_waiting(server);
        slint::spawn_local(sending).expect("the event loop is running");
    }
    {
        let mut p = playing.borrow_mut();
        p.player = None;
        p.tick.stop();
        p.queue = Queue::default();
        p.station = None;
        p.resume = None;
        p.shown = None;
        p.saved = None;
        p.queue_refused = false;
        p.covers.clear();
        list_queue(ui, browser, &mut p, playing);
    }
    browse::stop_playing_motion(browser);
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
            Err(error) if refuses_play_queues(&error) => {
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
        p.queue = Queue::new(queue.entry);
        p.queue.expand_works(&prefs::load().expanded_works);
        p.resume = Some((index, seconds));
        p.shown = Some(index);
        ui.set_playing(true);
        ui.set_playing_paused(true);
        ui.set_playing_position(seconds as f32);
        lyrics::follow(&ui);
        ui.set_playing_problem("".into());
        ui.set_playing_quality("".into());
        ui.set_playing_level(-1);
        ui.set_playing_path(ModelRc::default());
        show_song(&ui, &browser, &p, &playing, index);
        list_queue(&ui, &browser, &mut p, &playing);
    })
    .expect("the event loop is running");
}

/// Saves the queue and where it is on the server, for any app to carry on
/// with, or clears the one saved there when the queue is empty; a server
/// that turned it down isn't asked again. Which of its works are open is
/// kept beside it on this device.
fn save(
    browser: &browse::Shared,
    p: &Playing,
    playing: &Shared,
    index: Option<usize>,
    seconds: f64,
) {
    // A station is no queue to keep: the one saved stays for the next start.
    let Some(client) = browse::client(browser).filter(|_| !p.queue_refused && p.station.is_none())
    else {
        return;
    };
    let current = match index.and_then(|index| p.queue.get(index)) {
        Some(song) => Some(song.id.clone()),
        None if p.queue.is_empty() => None,
        // Nothing playing, as without a device, would lose where it was.
        None => return,
    };
    let ids: Vec<String> = p.queue.songs().map(|song| song.id.clone()).collect();
    remember_expanded_works(&p.queue);
    // The browser plays nothing, and a queue saved from it would take the
    // place of the one a device that plays left off at.
    if cfg!(target_family = "wasm") {
        return;
    }
    let position = current.is_some().then_some((seconds * 1000.0) as i64);
    let clearing = current.is_none();
    let playing = playing.clone();
    slint::spawn_local(async move {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let saved = client
            .save_play_queue(&ids, current.as_deref(), position)
            .await;
        // One lost on the way is made again with the next change. Only
        // OpenSubsonic's servers clear a queue, so turning that down says
        // nothing of saving one.
        if let Err(error) = saved
            && !clearing
            && refuses_play_queues(&error)
        {
            playing.borrow_mut().queue_refused = true;
        }
    })
    .expect("the event loop is running");
}

/// Whether `error` says the server will never keep a play queue: it lacks the
/// call (HTTP 404, 405 or 501), the protocol versions don't match (20, 30),
/// the user may not (50) or the trial is over (60). A server that is down,
/// restarting or out of reach is asked again with the next change.
fn refuses_play_queues(error: &opensubsonic::Error) -> bool {
    match error {
        opensubsonic::Error::Status(status) => matches!(status, 404 | 405 | 501),
        opensubsonic::Error::Api(problem) => matches!(problem.code, 20 | 30 | 50 | 60),
        _ => false,
    }
}

/// Keeps which works of `queue` the sheet lists open on this device, for the
/// queue saved on the server to open them again at the next start.
fn remember_expanded_works(queue: &Queue) {
    let expanded = queue.expanded_works();
    let mut saved = prefs::load();
    if saved.expanded_works != expanded {
        saved.expanded_works = expanded;
        prefs::save(&saved);
    }
}

/// The songs of a song's, an album's, a playlist's, a folder's or an
/// artist's link, in their order: an artist's those of their own albums.
async fn songs_of(
    client: &Client,
    kept: &offline::Downloads,
    offline: bool,
    link: &str,
) -> opensubsonic::Result<Vec<Child>> {
    Ok(match link.split_once(':') {
        Some(("song", id)) => {
            let call = client.get_song(id);
            vec![browse::saved_or(offline, || kept.song(id), call).await?]
        }
        Some(("album", id)) => album_songs(client, kept, offline, id).await?,
        Some(("playlist", id)) => {
            let call = client.get_playlist(id);
            browse::saved_or(offline, || kept.playlist(id), call)
                .await?
                .entry
        }
        Some(("folder", id)) => {
            let call = browse::whole_folder(client, id);
            browse::saved_or(offline, || kept.folder(id), call)
                .await?
                .child
        }
        Some(("artist", id)) => {
            let artist = client.get_artist(id).await?;
            let mut songs = Vec::new();
            for album in browse::own_albums_of(&artist.artist, &artist.album) {
                songs.extend(album_songs(client, kept, offline, &album.id).await?);
            }
            songs
        }
        _ => Vec::new(),
    })
}

/// The songs of the album `id`, in their order.
async fn album_songs(
    client: &Client,
    kept: &offline::Downloads,
    offline: bool,
    id: &str,
) -> opensubsonic::Result<Vec<Child>> {
    let call = client.get_album(id);
    Ok(browse::saved_or(offline, || kept.album(id), call)
        .await?
        .song)
}

/// `songs` that can play now: while the server isn't there, only those kept
/// on this device, and `start`, the index of the one to start from, moved
/// to where it is among them.
fn playable(
    ui: &AppWindow,
    browser: &browse::Shared,
    songs: Vec<Child>,
    start: usize,
) -> (Vec<Child>, usize) {
    let wanted = songs.get(start).map(|song| song.id.clone());
    let all = songs.len();
    let songs = browse::available(browser, songs);
    if songs.len() < all {
        let text = match songs.is_empty() {
            true => "None of it is downloaded.",
            false => "What isn't downloaded was left out.",
        };
        browse::notify(ui, browser, text.into());
    }
    let start = wanted
        .and_then(|id| songs.iter().position(|song| song.id == id))
        .unwrap_or(0);
    (songs, start)
}

/// A seed no shuffle had before.
fn seed() -> u64 {
    RandomState::new().hash_one(())
}

fn tracks(queue: &Queue) -> Vec<Track> {
    queue.songs().map(track).collect()
}

/// What the player plays: the station, else the queue's songs.
fn queued_tracks(p: &Playing) -> Vec<Track> {
    match &p.station {
        Some(tuned) => vec![radio::track(&tuned.station)],
        None => tracks(&p.queue),
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

/// Android's USB output can pass audio on untouched, which the player asks it
/// for; elsewhere the device is opened as it is.
#[cfg(target_os = "android")]
fn new_player(device: Option<String>, fetch: playback::Fetch) -> Player {
    Player::with_system(device, fetch, crate::bit_perfect::system())
}

#[cfg(not(any(target_os = "android", target_family = "wasm")))]
fn new_player(device: Option<String>, fetch: playback::Fetch) -> Player {
    Player::new(device, fetch)
}

/// The OpenSubsonic extension with which a server converts a song from
/// part way in.
#[cfg(not(target_family = "wasm"))]
const TRANSCODE_OFFSET: &str = "transcodeOffset";

/// Fetches tracks from `client` in the format the settings pick for the
/// connection in use when each is fetched, into files in the cache folder
/// that are gone once played. A track fetched ahead keeps the quality it was
/// fetched in if the connection changes before its turn. A conversion
/// arrives only as fast as the server makes it, so on a server that can, one
/// played from part way in is converted from there. A track kept in
/// `downloads` is read from its whole file, which seeks on disk. Runs on the
/// player's thread.
#[cfg(not(target_family = "wasm"))]
fn fetch(client: Arc<Client>, downloads: offline::Downloads) -> playback::Fetch {
    let runtime = tokio::runtime::Handle::current();
    let folder = cache();
    Box::new(move |track: &Track, seconds: f64| {
        let (format, max_bit_rate) = quality::request(&prefs::load(), quality::connection());
        // A file that went missing since is streamed instead.
        if let Some(path) = downloads.playable(&track.id, (format, max_bit_rate))
            && let Ok(download) = playback::open_file(&path)
        {
            return Ok(download);
        }
        let start = quality::stream_start(format, client.supports(TRANSCODE_OFFSET), seconds);
        // A song played from its start is asked for as before, so its
        // conversion keeps what makes it gapless.
        let time_offset = start.filter(|&start| start > 0.0);
        let mut stream = runtime
            .block_on(client.stream(&track.id, format, max_bit_rate, time_offset))
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
        Ok(match start {
            Some(start) => download.starting_at(start),
            None => download,
        })
    })
}

/// fonoteca-client in the system's cache folder, or on Android in a cache
/// folder in the app's own, the only place it may write.
#[cfg(not(target_family = "wasm"))]
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

/// What the flow test needs of a player it can't have: the queue's songs put
/// on the sheet as the player would, and the ends a queue can come to.
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;

    use super::*;

    thread_local! {
        static HANDLES: RefCell<Option<(browse::Shared, Shared)>> = const { RefCell::new(None) };
    }

    /// Keeps the first window's, since a test may start others.
    pub(super) fn keep(browser: &browse::Shared, playing: &Shared) {
        HANDLES.with(|handles| {
            handles
                .borrow_mut()
                .get_or_insert_with(|| (browser.clone(), playing.clone()));
        });
    }

    fn handles() -> (browse::Shared, Shared) {
        HANDLES.with(|handles| handles.borrow().clone().expect("the player is wired"))
    }

    /// Lists `found` as the sound devices there are, `system` the one the
    /// system plays on.
    pub(crate) fn show_devices(ui: &AppWindow, found: Vec<OutputDevice>, system: Option<String>) {
        let (_, playing) = handles();
        super::show_devices(ui, &playing, found, system);
    }

    /// Has the device found by the search for devices on the network, as
    /// one would be once found.
    pub(crate) fn find_network_device(device: network_output::Device) {
        let (_, playing) = handles();
        playing.borrow_mut().network_target = Some(device);
    }

    /// Queues `songs` and shows the `index`th, as when the player moves on.
    pub(crate) fn show_song(ui: &AppWindow, songs: Vec<Child>, index: usize) {
        let (browser, playing) = handles();
        {
            let mut p = playing.borrow_mut();
            p.queue = Queue::new(songs);
            p.shown = Some(index);
        }
        super::show_song(ui, &browser, &playing.borrow(), &playing, index);
    }

    /// Queues `songs` with the `index`th playing, shows it and lists the
    /// queue on the sheet.
    pub(crate) fn show_queue(ui: &AppWindow, songs: Vec<Child>, index: usize) {
        let (browser, playing) = handles();
        show_song(ui, songs, index);
        list_queue(ui, &browser, &mut playing.borrow_mut(), &playing);
    }

    /// Queues a page's `songs` as a play from the `start`th does and lists
    /// them on the sheet.
    pub(crate) fn play_page(ui: &AppWindow, songs: Vec<Child>, start: usize) {
        let (browser, playing) = handles();
        let (queue, start) = page_queue(songs, start, false);
        let mut p = playing.borrow_mut();
        p.queue = queue;
        p.shown = Some(start);
        list_queue(ui, &browser, &mut p, &playing);
    }

    /// The ids of the songs queued, in order, and the place of the one playing.
    pub(crate) fn queued_ids() -> (Vec<String>, Option<usize>) {
        let (_, playing) = handles();
        let p = playing.borrow();
        (
            p.queue.songs().map(|song| song.id.clone()).collect(),
            p.shown,
        )
    }

    /// The ids of the songs on screen, in their rows' order.
    pub(crate) fn page_song_ids(ui: &AppWindow) -> Vec<String> {
        let (browser, _) = handles();
        let (songs, _) = browse::songs(&browser, ui.get_page(), 0);
        songs.into_iter().map(|song| song.id).collect()
    }

    /// Whether each song queued is loved, in order.
    pub(crate) fn loved_ids() -> Vec<bool> {
        let (_, playing) = handles();
        let p = playing.borrow();
        p.queue.songs().map(|song| song.starred.is_some()).collect()
    }

    /// What is kept on this device, as the first window has it.
    pub(crate) fn downloads() -> offline::Downloads {
        browse::downloads(&handles().0)
    }

    /// Fetches song `id` as the player would.
    pub(crate) fn fetch_track(id: &str) -> Result<playback::Download, String> {
        let (browser, _) = handles();
        let client = browse::client(&browser).expect("signed in");
        let mut fetch = fetch(client, browse::downloads(&browser));
        let track = Track {
            id: id.into(),
            gain: Gain::default(),
        };
        fetch(&track, 0.0)
    }

    /// Takes the queue away, as when its last song is removed.
    pub(crate) fn clear(ui: &AppWindow) {
        let (browser, playing) = handles();
        super::clear(ui, &browser, &playing);
    }

    /// Loads the queue saved on the server again, as on a sign in.
    pub(crate) fn restore(ui: &AppWindow) {
        let (browser, playing) = handles();
        super::restore(ui, &browser, &playing);
    }

    /// Saves the queue at its `index`th song, `seconds` into it, as the
    /// player's ticks do.
    pub(crate) fn save(index: usize, seconds: f64) {
        let (browser, playing) = handles();
        super::save(&browser, &playing.borrow(), &playing, Some(index), seconds);
    }

    /// What the player says, while there is one.
    pub(crate) fn player_status() -> Option<playback::Status> {
        let (_, playing) = handles();
        playing.borrow().player.as_ref().map(Active::status)
    }

    /// Fetches the song `id` from `seconds` into it as the player does, once
    /// the server has answered, and says where in the song the download
    /// begins when it begins part way.
    pub(crate) fn fetch_song(id: &str, seconds: f64) -> Option<f64> {
        let (browser, _) = handles();
        let client = browse::client(&browser).expect("signed in");
        let mut fetch = super::fetch(client, offline::Downloads::default());
        let track = Track {
            id: id.into(),
            gain: Gain::default(),
        };
        std::thread::spawn(move || fetch(&track, seconds).map(|download| download.starts_at()))
            .join()
            .unwrap()
            .expect("fetched")
    }

    /// The name and stream of the station playing, if one is.
    pub(crate) fn tuned() -> Option<(String, String)> {
        let (_, playing) = handles();
        let p = playing.borrow();
        p.station
            .as_ref()
            .map(|tuned| (tuned.station.name.clone(), tuned.station.stream.clone()))
    }

    /// Shows the scrobbler `song` playing, `position` seconds in, as the
    /// player's ticks do.
    pub(crate) fn hear(song: &Child, position: f64) {
        let (browser, playing) = handles();
        let mut p = playing.borrow_mut();
        super::observe(&browser, &mut p.scrobbler, Some(song), position, false);
    }

    /// Shows the scrobbler `song` playing, `position` seconds in, on the
    /// server signed in to as though `account` were signed in to it.
    pub(crate) fn hear_for(account: &str, song: &Child, position: f64) {
        let (browser, playing) = handles();
        let server = scrobble::Server {
            client: browse::client(&browser).expect("signed in"),
            account: account.to_owned(),
        };
        let mut p = playing.borrow_mut();
        p.scrobbler
            .observe(Some(song), position, false, Some(server));
    }

    /// How often the scrobbler was shown the player and asked to send what
    /// waits.
    pub(crate) fn scrobbler_calls() -> (u32, u32) {
        handles().1.borrow().scrobbler.calls()
    }

    /// Shows the streaming quality saved, as at the start.
    pub(crate) fn show_saved_stream_quality(ui: &AppWindow) {
        super::show_stream_quality(ui, &prefs::load());
    }

    /// The album whose motion cover is kept.
    pub(crate) fn motion_album() -> Option<String> {
        browse::playing_motion_album(&handles().0)
    }
}
