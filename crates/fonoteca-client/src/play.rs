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
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use opensubsonic::{Child, Client};
use playback::{Gain, Leveling, Player, Quality, Repeat, Step, Track};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::queue::{Moved, Plan, Queue, Row, Shape, plan, run_from};
use crate::{
    AppWindow, Icons, MenuEntry, Name, Page, PathStep, QueueLine, browse, prefs, quality, sound,
    waveform,
};

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

#[derive(Default)]
struct Playing {
    player: Option<Player>,
    /// The songs queued; the one the bar shows and the steps it shows for
    /// it.
    queue: Queue,
    shown: Option<usize>,
    path: Vec<Step>,
    /// How many of the queue's changes the player was handed. Until it has
    /// made them all, the track it says plays counts in the queue before.
    requeued: u64,
    /// The rows of the now playing sheet's list of the queue, and the
    /// covers it has shown, by their ids.
    listed: Vec<Row>,
    covers: HashMap<String, Image>,
    /// Where the queue saved on the server left off, until it plays.
    resume: Option<(usize, f64)>,
    /// The track, whether it was paused and the seconds into it when the
    /// queue was last saved; and whether the server turned the saving down.
    saved: Option<(Option<usize>, bool, f64)>,
    queue_refused: bool,
    /// The sound devices by cpal id, none for the default.
    devices: Vec<Option<String>>,
    tick: Timer,
}

type Shared = Rc<RefCell<Playing>>;

pub fn wire(ui: &AppWindow, browser: &browse::Shared) {
    let playing = Shared::default();
    #[cfg(test)]
    testing::keep(browser, &playing);
    let saved = prefs::load();
    ui.set_leveling(saved.leveling.clamp(0, 2));
    ui.set_repeat(saved.repeat.clamp(0, 2));
    show_stream_quality(ui, &saved);
    ui.set_can_play(true);
    list_devices(ui, &playing);

    ui.on_now_playing_moving_changed({
        let browser = browser.clone();
        move |moving| browse::move_playing_motion(&browser, moving)
    });
    ui.on_library_started({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move || restore(&weak.unwrap(), &browser, &playing)
    });
    ui.on_play_tracks({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |row| {
            let ui = weak.unwrap();
            let (songs, start) = browse::songs(&browser, ui.get_page(), row as usize);
            play(&ui, &browser, &playing, songs, start);
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
                player.play(tracks(&p.queue), 0, 0.0);
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
    ui.on_list_devices({
        let (weak, playing) = (ui.as_weak(), playing.clone());
        move || list_devices(&weak.unwrap(), &playing)
    });
    ui.on_choose_output({
        let (weak, playing) = (ui.as_weak(), playing.clone());
        move |index| {
            let p = playing.borrow();
            let device = p.devices.get(index as usize).cloned().flatten();
            mark_device(&weak.unwrap(), index as usize);
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
    sound::wire(ui, {
        let playing = playing.clone();
        move |dsp| {
            if let Some(player) = &playing.borrow().player {
                player.set_dsp(dsp);
            }
        }
    });
}

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
            slint::spawn_local(async move {
                let songs = songs_of(&client, &link).await;
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
            let moved = p.queue.move_to(songs, current + 1);
            drop(p);
            requeued(&weak.unwrap(), &browser, &playing, moved);
        }
    });
    ui.on_move_queued({
        let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
        move |from, to| {
            let mut p = playing.borrow_mut();
            let (Some(from), Some(onto)) = (listed_songs(&p, from), listed_songs(&p, to)) else {
                return;
            };
            let moved = p.queue.move_onto(from, onto);
            drop(p);
            requeued(&weak.unwrap(), &browser, &playing, moved);
        }
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

/// Puts the sound devices there are now in the now playing sheet's menu, with
/// the one chosen marked. Looking for them can take a moment, so it happens
/// off the event loop.
fn list_devices(ui: &AppWindow, playing: &Shared) {
    let found = tokio::task::spawn_blocking(playback::devices);
    let (weak, playing) = (ui.as_weak(), playing.clone());
    slint::spawn_local(async move {
        let Ok(found) = found.await else { return };
        let Some(ui) = weak.upgrade() else { return };
        show_devices(&ui, &playing, found);
    })
    .expect("the event loop is running");
}

/// The `found` devices by cpal id and name, after the system's default.
fn show_devices(ui: &AppWindow, playing: &Shared, found: Vec<(String, String)>) {
    let mut names = vec![(SharedString::from("System default"), SharedString::new())];
    let mut devices = vec![None];
    for (id, name) in found {
        let pcm = id.strip_prefix("alsa:").unwrap_or(&id);
        names.push((SharedString::from(name), SharedString::from(pcm)));
        devices.push(Some(id));
    }
    let chosen = devices
        .iter()
        .position(|id| *id == prefs::load().output_device)
        .unwrap_or(0);
    let icons = ui.global::<Icons>();
    let entries: Vec<MenuEntry> = names
        .into_iter()
        .enumerate()
        .map(|(index, (label, detail))| MenuEntry {
            id: index.to_string().into(),
            label,
            detail,
            icon: device_icon(&icons, index == chosen),
            ..Default::default()
        })
        .collect();
    ui.set_output_devices(ModelRc::new(VecModel::from(entries)));
    playing.borrow_mut().devices = devices;
}

/// Moves the menu's mark to the device `chosen`.
fn mark_device(ui: &AppWindow, chosen: usize) {
    let icons = ui.global::<Icons>();
    let entries = ui.get_output_devices();
    for (index, mut entry) in entries.iter().enumerate() {
        entry.icon = device_icon(&icons, index == chosen);
        entries.set_row_data(index, entry);
    }
}

/// A check for the device in use; the others get a blank of its width, so
/// their names line up with it.
fn device_icon(icons: &Icons, in_use: bool) -> SharedString {
    if in_use {
        icons.get_check()
    } else {
        icons.get_blank()
    }
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

/// Does `how` with every song of the artist's own albums, once all of them
/// have arrived: a shuffle across albums needs them all, and an album fetched
/// per call keeps the window free meanwhile.
fn queue_artist(ui: &AppWindow, browser: &browse::Shared, playing: &Shared, how: String) {
    let Some(client) = browse::client(browser) else {
        return;
    };
    let albums = browse::own_albums(browser);
    if albums.is_empty() {
        return browse::notify(ui, browser, "They have no albums of their own.".into());
    }
    browse::notify(ui, browser, format!("Fetching {} albums…", albums.len()));
    let (weak, browser, playing) = (ui.as_weak(), browser.clone(), playing.clone());
    slint::spawn_local(async move {
        let mut songs = Vec::new();
        for link in albums {
            match songs_of(&client, &link).await {
                Ok(album) => songs.extend(album),
                Err(e) => {
                    let Some(ui) = weak.upgrade() else { return };
                    let text = format!("The songs couldn't be fetched: {e}");
                    return browse::notify(&ui, &browser, text);
                }
            }
        }
        if let Some(ui) = weak.upgrade() {
            queue_songs(&ui, &browser, &playing, songs, &how);
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
    let mut p = playing.borrow_mut();
    let device = prefs::load().output_device;
    // Each queue gets a player with the client signed in now. The last one
    // lets the device go first, since a device played on directly can only
    // be open once.
    p.player = None;
    let player = new_player(device, fetch(client));
    player.set_leveling(LEVELING[ui.get_leveling().clamp(0, 2) as usize]);
    player.set_repeat(REPEAT[ui.get_repeat().clamp(0, 2) as usize]);
    player.set_dsp(sound::dsp());
    player.play(tracks(&queue), start, seconds);
    // Before any of it is heard: a track starts once some of it is in.
    player.set_paused(paused);
    p.player = Some(player);
    p.queue = queue;
    p.requeued = 0;
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
    p.player = None;
    p.tick.stop();
    p.queue = Queue::default();
    p.resume = None;
    p.shown = None;
    p.saved = None;
    save(browser, &p, playing, None, 0.0);
    list_queue(ui, browser, &mut p, playing);
    browse::stop_playing_motion(browser);
    ui.set_playing(false);
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
    if status.requeues != p.requeued {
        return;
    }
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
    if let Some(index) = status.index {
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
    ui.set_playing_title(song.title.as_str().into());
    let names = browse::song_names(song);
    ui.set_playing_names(ModelRc::new(VecModel::from(names)));
    ui.set_playing_album(Name {
        label: song.album.as_deref().unwrap_or_default().into(),
        link: song
            .album_id
            .as_ref()
            .map_or_else(SharedString::new, |id| format!("album:{id}").into()),
    });
    ui.set_playing_duration(song.duration.unwrap_or_default() as f32);
    ui.set_playing_cover(Image::default());
    let (playing_now, song_id) = (playing.clone(), song.id.clone());
    waveform::show(ui, browse::client(browser), song.id.clone(), move || {
        let p = playing_now.borrow();
        p.shown
            .and_then(|shown| p.queue.get(shown))
            .is_some_and(|song| song.id == song_id)
    });
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

/// The songs the `row` of the sheet's list stands for.
fn listed_songs(p: &Playing, row: i32) -> Option<Range<usize>> {
    p.listed.get(row as usize).map(|row| row.songs.clone())
}

/// Lists the queue on the now playing sheet from the work or song playing
/// on, with the covers it has shown before and the others once they come.
fn list_queue(ui: &AppWindow, browser: &browse::Shared, p: &mut Playing, playing: &Shared) {
    p.listed = p.queue.rows(p.shown, QUEUE_SHOWN);
    let lines: Vec<QueueLine> = p.listed.iter().map(|row| queue_line(p, row)).collect();
    let listed_to = p.listed.last().map_or(0, |row| row.songs.end);
    ui.set_queue_more((p.queue.len() - listed_to) as i32);
    ui.set_shuffled(p.queue.shuffled());
    let model = Rc::new(VecModel::from(lines));
    ui.set_queue_lines(ModelRc::from(model.clone()));
    let wanted: Rc<Vec<Option<String>>> = Rc::new(
        p.listed
            .iter()
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
        p.queue = Queue::default();
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
        p.queue = Queue::new(queue.entry);
        p.resume = Some((index, seconds));
        p.shown = Some(index);
        ui.set_playing(true);
        ui.set_playing_paused(true);
        ui.set_playing_position(seconds as f32);
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
/// that turned it down isn't asked again.
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
    let current = match index.and_then(|index| p.queue.get(index)) {
        Some(song) => Some(song.id.clone()),
        None if p.queue.is_empty() => None,
        // Nothing playing, as without a device, would lose where it was.
        None => return,
    };
    let ids: Vec<String> = p.queue.songs().map(|song| song.id.clone()).collect();
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
        if let Err(opensubsonic::Error::Status(_) | opensubsonic::Error::Api(_)) = saved
            && !clearing
        {
            playing.borrow_mut().queue_refused = true;
        }
    })
    .expect("the event loop is running");
}

/// The songs of an album's or a playlist's link, in their order.
async fn songs_of(client: &Client, link: &str) -> opensubsonic::Result<Vec<Child>> {
    Ok(match link.split_once(':') {
        Some(("album", id)) => client.get_album(id).await?.song,
        Some(("playlist", id)) => client.get_playlist(id).await?.entry,
        _ => Vec::new(),
    })
}

/// A seed no shuffle had before.
fn seed() -> u64 {
    RandomState::new().hash_one(())
}

fn tracks(queue: &Queue) -> Vec<Track> {
    queue.songs().map(track).collect()
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

#[cfg(not(target_os = "android"))]
fn new_player(device: Option<String>, fetch: playback::Fetch) -> Player {
    Player::new(device, fetch)
}

/// Fetches tracks from `client` in the format the settings pick for the
/// connection in use when each is fetched, into files in the cache folder
/// that are gone once played. A track fetched ahead keeps the quality it was
/// fetched in if the connection changes before its turn. Runs on the
/// player's thread.
fn fetch(client: Arc<Client>) -> playback::Fetch {
    let runtime = tokio::runtime::Handle::current();
    let folder = cache();
    Box::new(move |track: &Track| {
        let (format, max_bit_rate) = quality::request(&prefs::load(), quality::connection());
        let mut stream = runtime
            .block_on(client.stream(&track.id, format, max_bit_rate))
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

    /// Lists `found` as the sound devices there are.
    pub(crate) fn show_devices(ui: &AppWindow, found: Vec<(String, String)>) {
        let (_, playing) = handles();
        super::show_devices(ui, &playing, found);
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

    /// Fetches the song `id` as the player does, once the server has answered.
    pub(crate) fn fetch_song(id: &str) {
        let (browser, _) = handles();
        let client = browse::client(&browser).expect("signed in");
        let mut fetch = super::fetch(client);
        let track = Track {
            id: id.into(),
            gain: Gain::default(),
        };
        std::thread::spawn(move || fetch(&track).map(drop))
            .join()
            .unwrap()
            .expect("fetched");
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
