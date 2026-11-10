//! A queue played on a device on the network, in place of a sound device of
//! this one. The audio engine stays out of it: a device on the network decodes the
//! file itself, so each track is fetched as for any player, kept in a file
//! and served to the device, which is told to play it. A track ends when the
//! device says so, and the next is loaded then, so tracks don't play
//! gaplessly.
//!
//! The sound processing, volume leveling and bit-perfect output belong to
//! the engine and do not reach the device; the commands for them are
//! ignored here.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use network_output::{FileServer, Media, NetworkOutput, OutputState};
use playback::{Fetch, RESTART_AFTER, Repeat, Status, Track};

/// How often the device is asked how it is.
const TICK: Duration = Duration::from_millis(250);

/// Connects to the device again, for playing on once it is back.
pub type Connect = Box<dyn FnMut() -> Box<dyn NetworkOutput> + Send>;

enum Command {
    Play(Vec<Track>, usize, f64),
    Requeue(Vec<Track>, Vec<Option<usize>>),
    Jump(usize),
    Next,
    Previous,
    Seek(f64),
    Repeat(Repeat),
    Paused(bool),
    Stop,
}

pub struct NetworkPlayer {
    status: Arc<Mutex<Status>>,
    commands: Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl NetworkPlayer {
    /// A player on the device called `name` that `connect` connects to.
    /// Tracks come from `fetch` and wait in files in `folder` for the device
    /// to be served them by `files`.
    pub fn new(
        name: String,
        mut connect: Connect,
        files: FileServer,
        fetch: Fetch,
        folder: PathBuf,
    ) -> Self {
        let status = Arc::new(Mutex::new(Status::default()));
        let (commands, received) = channel();
        let worker = Worker {
            name,
            output: connect(),
            connect,
            files,
            fetch,
            folder,
            on_disk: None,
            files_made: 0,
            queue: Vec::new(),
            repeat: Repeat::Off,
            paused: false,
            pending: None,
            loads: 0,
            unavailable: false,
            status: status.clone(),
        };
        let worker = std::thread::Builder::new()
            .name("network-player".into())
            .spawn(move || worker.run(&received))
            .expect("a thread for the network player");
        NetworkPlayer {
            status,
            commands,
            worker: Some(worker),
        }
    }

    fn send(&self, command: Command) {
        // The worker only ends with the player.
        let _ = self.commands.send(command);
    }

    pub fn play(&self, queue: Vec<Track>, start: usize, seconds: f64) {
        self.send(Command::Play(queue, start, seconds));
    }

    pub fn requeue(&self, queue: Vec<Track>, moved: Vec<Option<usize>>) {
        self.send(Command::Requeue(queue, moved));
    }

    pub fn jump(&self, index: usize) {
        self.send(Command::Jump(index));
    }

    pub fn next(&self) {
        self.send(Command::Next);
    }

    pub fn previous(&self) {
        self.send(Command::Previous);
    }

    pub fn seek(&self, seconds: f64) {
        self.send(Command::Seek(seconds));
    }

    pub fn set_repeat(&self, repeat: Repeat) {
        self.send(Command::Repeat(repeat));
    }

    pub fn set_paused(&self, paused: bool) {
        self.send(Command::Paused(paused));
    }

    /// Ends what plays on the device, for another app or output to use it.
    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for NetworkPlayer {
    /// Waits for the worker, which has the device told to pause as it goes.
    fn drop(&mut self) {
        // Closing the channel ends the worker.
        let (closed, _) = channel();
        self.commands = closed;
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            eprintln!("The network player ended in a panic.");
        }
    }
}

struct Worker {
    name: String,
    output: Box<dyn NetworkOutput>,
    connect: Connect,
    files: FileServer,
    fetch: Fetch,
    folder: PathBuf,
    /// The file the device plays now.
    on_disk: Option<PathBuf>,
    files_made: u64,
    queue: Vec<Track>,
    repeat: Repeat,
    paused: bool,
    /// The track to load, and from where, once the commands waiting are
    /// taken: loading is slow, and the last of several asks is the one that
    /// counts.
    pending: Option<(usize, f64)>,
    /// How many tracks this connection was told to load.
    loads: u64,
    unavailable: bool,
    status: Arc<Mutex<Status>>,
}

impl Worker {
    fn run(mut self, commands: &Receiver<Command>) {
        loop {
            match commands.recv_timeout(TICK) {
                Ok(command) => {
                    self.handle(command);
                    while let Ok(command) = commands.try_recv() {
                        self.handle(command);
                    }
                    self.settle();
                }
                Err(RecvTimeoutError::Timeout) => self.tick(),
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn show(&self, change: impl FnOnce(&mut Status)) {
        change(&mut self.status.lock().unwrap());
    }

    fn index(&self) -> Option<usize> {
        self.status.lock().unwrap().index
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Play(queue, start, seconds) => {
                self.queue = queue;
                self.start(start, seconds);
            }
            Command::Requeue(queue, moved) => self.requeue(queue, &moved),
            Command::Jump(index) => self.start(index, 0.0),
            Command::Next => match self
                .index()
                .and_then(|index| following(index, self.queue.len(), self.repeat))
            {
                Some(next) => self.start(next, 0.0),
                None => self.stop(),
            },
            Command::Previous => {
                let (index, position) = {
                    let status = self.status.lock().unwrap();
                    (status.index, status.position)
                };
                match index {
                    Some(index) if position > RESTART_AFTER || index == 0 => self.start(index, 0.0),
                    Some(index) => self.start(index - 1, 0.0),
                    None => {}
                }
            }
            Command::Seek(seconds) => self.seek(seconds.max(0.0)),
            Command::Repeat(repeat) => self.repeat = repeat,
            Command::Paused(paused) => self.set_paused(paused),
            Command::Stop => self.stop(),
        }
    }

    fn start(&mut self, index: usize, seconds: f64) {
        self.pending = Some((index, seconds));
        self.show(|status| {
            status.index = Some(index);
            status.position = seconds;
            status.ended = false;
            status.stopped = None;
            status.skipped = None;
            status.problem = None;
        });
    }

    fn seek(&mut self, seconds: f64) {
        if let Some((_, from)) = self.pending.as_mut() {
            *from = seconds;
        } else {
            self.output.seek(seconds);
        }
        self.show(|status| status.position = seconds);
    }

    fn set_paused(&mut self, paused: bool) {
        if !paused && self.unavailable {
            return self.reconnect();
        }
        self.paused = paused;
        self.show(|status| status.paused = paused);
        if self.pending.is_none() {
            self.output.set_paused(paused);
        }
    }

    /// Plays on after the device went away and came back: the track again,
    /// from where it was heard.
    fn reconnect(&mut self) {
        self.output = (self.connect)();
        self.unavailable = false;
        self.loads = 0;
        self.paused = false;
        let (index, position) = {
            let status = self.status.lock().unwrap();
            (status.index, status.position)
        };
        self.show(|status| status.paused = false);
        match index {
            Some(index) => self.start(index, position),
            None => self.show(|status| status.problem = None),
        }
    }

    fn stop(&mut self) {
        self.pending = None;
        self.output.stop();
        self.show(|status| status.ended = true);
    }

    /// Takes up `queue` in place of the one before, where `moved` gives each
    /// track of that one its place in `queue`. A track playing that was
    /// taken out gives way to the first after it that stayed.
    fn requeue(&mut self, queue: Vec<Track>, moved: &[Option<usize>]) {
        self.queue = queue;
        self.show(|status| status.requeues += 1);
        let Some(index) = self.index() else { return };
        let place = |index: usize| moved.get(index).copied().flatten();
        match place(index) {
            Some(placed) => {
                // A track waiting to load is the one the status shows.
                self.pending = self.pending.map(|(_, seconds)| (placed, seconds));
                self.show(|status| status.index = Some(placed));
            }
            None => match (index..moved.len()).find_map(place) {
                Some(next) => self.start(next, 0.0),
                None => self.stop(),
            },
        }
    }

    /// Loads the track waiting to be loaded; one that can't be fetched is
    /// skipped for the next.
    fn settle(&mut self) {
        while let Some((index, seconds)) = self.pending.take() {
            match self.host(index) {
                Ok(hosted) => {
                    self.loads += 1;
                    self.output.load(Media {
                        url: hosted.url,
                        content_type: hosted.content_type.to_owned(),
                        start: seconds,
                        paused: self.paused,
                    });
                }
                Err(why) => {
                    match (index + 1 < self.queue.len()).then_some(index + 1) {
                        Some(next) => self.start(next, 0.0),
                        None => self.stop(),
                    }
                    self.show(|status| status.skipped = Some((index, why)));
                }
            }
        }
    }

    /// Fetches the `index`th track into a file and has it served.
    fn host(&mut self, index: usize) -> Result<network_output::Hosted, String> {
        let track = self
            .queue
            .get(index)
            .ok_or("The track isn't in the queue.")?;
        let mut download = (self.fetch)(track, 0.0)?;
        std::fs::create_dir_all(&self.folder).map_err(saving)?;
        let path = self.folder.join(format!(
            "network-{}-{}",
            std::process::id(),
            self.files_made
        ));
        self.files_made += 1;
        let copied = File::create(&path).and_then(|mut file| io::copy(&mut download, &mut file));
        let hosted = copied
            .and_then(|_| self.files.serve(&path))
            .inspect_err(|_| remove(&path))
            .map_err(saving)?;
        if let Some(played) = self.on_disk.replace(path) {
            remove(&played);
        }
        Ok(hosted)
    }

    /// Follows the device: where it is in the track, and when it ends or is
    /// gone.
    fn tick(&mut self) {
        if self.pending.is_some() || self.unavailable {
            return;
        }
        let status = match self.output.status() {
            Ok(status) => status,
            Err(_) => return self.lose(),
        };
        // What the device says belongs to a track before the last one loaded.
        if status.loads < self.loads {
            return;
        }
        match status.state {
            OutputState::Loading => {}
            OutputState::Playing | OutputState::Paused => {
                let paused = status.state == OutputState::Paused;
                self.paused = paused;
                self.show(|shown| {
                    shown.position = status.position;
                    shown.paused = paused;
                });
            }
            OutputState::Finished => self.advance(),
            OutputState::Failed(why) => self.show(|shown| shown.problem = Some(why)),
        }
        self.settle();
    }

    /// Plays on once the track ends: the queue's next, or the same again.
    fn advance(&mut self) {
        match self
            .index()
            .and_then(|index| after(index, self.queue.len(), self.repeat))
        {
            Some(next) => self.start(next, 0.0),
            None => self.show(|status| status.ended = true),
        }
    }

    /// The device went away: the queue waits paused, and plays on from the
    /// same place only when asked.
    fn lose(&mut self) {
        self.unavailable = true;
        self.paused = true;
        let problem = format!("{} is unavailable.", self.name);
        self.show(|status| {
            status.paused = true;
            status.problem = Some(problem);
        });
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(played) = self.on_disk.take() {
            remove(&played);
        }
    }
}

fn saving(problem: io::Error) -> String {
    format!("The track couldn't be saved to disk: {problem}")
}

fn remove(path: &std::path::Path) {
    if let Err(problem) = std::fs::remove_file(path) {
        eprintln!("A track kept for a network output couldn't be removed: {problem}");
    }
}

/// The track after the `index`th as Next goes: the queue's next, or its
/// first after its last when all of it repeats.
fn following(index: usize, length: usize, repeat: Repeat) -> Option<usize> {
    if index + 1 < length {
        Some(index + 1)
    } else {
        (repeat == Repeat::All && length > 0).then_some(0)
    }
}

/// The track that plays once the `index`th ends.
fn after(index: usize, length: usize, repeat: Repeat) -> Option<usize> {
    match repeat {
        Repeat::One => Some(index),
        _ => following(index, length, repeat),
    }
}

#[cfg(test)]
mod tests {
    use network_output::{OutputStatus, Unavailable};
    use playback::Gain;

    use super::*;

    /// What the stand-in device was told, and what it answers.
    #[derive(Default)]
    struct Told {
        loads: Vec<Media>,
        paused: Vec<bool>,
        seeks: Vec<f64>,
        stops: u32,
        answer: Option<Result<OutputStatus, Unavailable>>,
    }

    impl Told {
        /// The device says `state`, `position` seconds in, of the track
        /// loaded last.
        fn says(&mut self, state: OutputState, position: f64) {
            self.answer = Some(Ok(OutputStatus {
                loads: self.loads.len() as u64,
                state,
                position,
            }));
        }
    }

    struct StandIn(Arc<Mutex<Told>>);

    impl NetworkOutput for StandIn {
        fn load(&mut self, media: Media) {
            self.0.lock().unwrap().loads.push(media);
        }

        fn set_paused(&mut self, paused: bool) {
            self.0.lock().unwrap().paused.push(paused);
        }

        fn seek(&mut self, seconds: f64) {
            self.0.lock().unwrap().seeks.push(seconds);
        }

        fn stop(&mut self) {
            self.0.lock().unwrap().stops += 1;
        }

        fn status(&self) -> Result<OutputStatus, Unavailable> {
            let told = self.0.lock().unwrap();
            told.answer.clone().unwrap_or(Ok(OutputStatus {
                loads: told.loads.len() as u64,
                state: OutputState::Loading,
                position: 0.0,
            }))
        }
    }

    /// The folder the tracks are kept in, removed after the worker that
    /// keeps them has gone.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    struct Setup {
        worker: Worker,
        told: Arc<Mutex<Told>>,
        connects: Arc<Mutex<u32>>,
        folder: PathBuf,
        _scratch: Scratch,
    }

    impl Setup {
        /// A worker on a stand-in device that fetches a file for every track
        /// but the one with the id "bad".
        fn new() -> Setup {
            let folder = std::env::temp_dir().join(format!(
                "remote-player-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&folder).unwrap();
            let told = Arc::new(Mutex::new(Told::default()));
            let connects = Arc::new(Mutex::new(0));
            let (reconnected, counted) = (told.clone(), connects.clone());
            let connect: Connect = Box::new(move || {
                *counted.lock().unwrap() += 1;
                Box::new(StandIn(reconnected.clone()))
            });
            let source = folder.join("source");
            let fetch: Fetch = Box::new(move |track, _| {
                if track.id == "bad" {
                    return Err("The track couldn't be fetched.".into());
                }
                std::fs::write(&source, b"fLaC and then the music").unwrap();
                playback::open_file(&source).map_err(|e| e.to_string())
            });
            let worker = Worker {
                name: "Kitchen".into(),
                output: Box::new(StandIn(told.clone())),
                connect,
                files: FileServer::start([127, 0, 0, 1].into()).unwrap(),
                fetch,
                folder: folder.clone(),
                on_disk: None,
                files_made: 0,
                queue: Vec::new(),
                repeat: Repeat::Off,
                paused: false,
                pending: None,
                loads: 0,
                unavailable: false,
                status: Arc::default(),
            };
            Setup {
                worker,
                told,
                connects,
                _scratch: Scratch(folder.clone()),
                folder,
            }
        }

        fn run(&mut self, command: Command) {
            self.worker.handle(command);
            self.worker.settle();
        }

        fn status(&self) -> Status {
            self.worker.status.lock().unwrap().clone()
        }

        fn loads(&self) -> usize {
            self.told.lock().unwrap().loads.len()
        }
    }

    fn tracks(ids: &[&str]) -> Vec<Track> {
        ids.iter()
            .map(|id| Track {
                id: (*id).into(),
                gain: Gain::default(),
            })
            .collect()
    }

    #[test]
    fn a_track_is_hosted_and_the_device_told_to_load_it_from_where_asked() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["a", "b", "c"]), 1, 12.0));
        let told = setup.told.lock().unwrap();
        let [load] = &told.loads[..] else {
            panic!("one load: {:?}", told.loads)
        };
        assert!(load.url.starts_with("http://127.0.0.1:"), "{}", load.url);
        assert_eq!(
            (load.content_type.as_str(), load.start, load.paused),
            ("audio/flac", 12.0, false)
        );
        drop(told);
        let status = setup.status();
        assert_eq!(
            (status.index, status.position, status.output),
            (Some(1), 12.0, None)
        );
    }

    #[test]
    fn a_pause_asked_before_the_track_loads_loads_it_paused() {
        let mut setup = Setup::new();
        setup.worker.handle(Command::Play(tracks(&["a"]), 0, 0.0));
        setup.worker.handle(Command::Paused(true));
        setup.worker.settle();
        let told = setup.told.lock().unwrap();
        assert!(told.loads[0].paused);
        assert!(
            told.paused.is_empty(),
            "no pause on a device with nothing playing yet"
        );
        drop(told);
        assert!(setup.status().paused);
    }

    #[test]
    fn a_track_the_device_finished_is_followed_by_the_next_until_the_queue_ends() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["a", "b"]), 0, 0.0));
        setup.told.lock().unwrap().says(OutputState::Finished, 0.0);
        setup.worker.tick();
        assert_eq!((setup.loads(), setup.status().index), (2, Some(1)));
        // What the device says of the track before is not heard again.
        setup.told.lock().unwrap().answer = Some(Ok(OutputStatus {
            loads: 1,
            state: OutputState::Finished,
            position: 0.0,
        }));
        setup.worker.tick();
        assert_eq!(setup.loads(), 2);
        setup.told.lock().unwrap().says(OutputState::Finished, 0.0);
        setup.worker.tick();
        let status = setup.status();
        assert_eq!(
            (setup.loads(), status.ended, status.index),
            (2, true, Some(1))
        );
    }

    #[test]
    fn repeating_loads_the_same_track_or_starts_the_queue_over() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["a", "b"]), 1, 0.0));
        setup.run(Command::Repeat(Repeat::One));
        setup.told.lock().unwrap().says(OutputState::Finished, 0.0);
        setup.worker.tick();
        assert_eq!((setup.loads(), setup.status().index), (2, Some(1)));
        setup.run(Command::Repeat(Repeat::All));
        setup.told.lock().unwrap().says(OutputState::Finished, 0.0);
        setup.worker.tick();
        assert_eq!((setup.loads(), setup.status().index), (3, Some(0)));
    }

    #[test]
    fn previous_goes_back_a_track_early_in_one_and_to_its_start_later() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["a", "b", "c"]), 2, 0.0));
        setup.told.lock().unwrap().says(OutputState::Playing, 1.0);
        setup.worker.tick();
        setup.run(Command::Previous);
        assert_eq!((setup.loads(), setup.status().index), (2, Some(1)));
        setup.told.lock().unwrap().says(OutputState::Playing, 40.0);
        setup.worker.tick();
        setup.run(Command::Previous);
        let status = setup.status();
        assert_eq!(
            (setup.loads(), status.index, status.position),
            (3, Some(1), 0.0)
        );
        setup.run(Command::Next);
        assert_eq!((setup.loads(), setup.status().index), (4, Some(2)));
        setup.run(Command::Next);
        assert!(setup.status().ended, "Next past the end stops");
        assert_eq!(setup.told.lock().unwrap().stops, 1);
    }

    #[test]
    fn a_seek_reaches_the_device_and_the_bar() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["a"]), 0, 0.0));
        setup.run(Command::Seek(95.0));
        assert_eq!(setup.told.lock().unwrap().seeks, [95.0]);
        assert_eq!(setup.status().position, 95.0);
    }

    #[test]
    fn what_the_device_plays_and_where_shows_in_the_status() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["a"]), 0, 0.0));
        setup.told.lock().unwrap().says(OutputState::Playing, 31.5);
        setup.worker.tick();
        let status = setup.status();
        assert_eq!((status.position, status.paused), (31.5, false));
        setup.told.lock().unwrap().says(OutputState::Paused, 31.5);
        setup.worker.tick();
        assert!(setup.status().paused, "paused from another app too");
    }

    #[test]
    fn a_device_that_goes_pauses_the_queue_and_plays_on_only_when_asked() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["a", "b"]), 1, 0.0));
        setup.told.lock().unwrap().says(OutputState::Playing, 70.0);
        setup.worker.tick();
        setup.told.lock().unwrap().answer = Some(Err(Unavailable("gone".into())));
        setup.worker.tick();
        let status = setup.status();
        assert_eq!(status.problem.as_deref(), Some("Kitchen is unavailable."));
        assert!(status.paused);
        setup.worker.tick();
        assert_eq!(
            (setup.loads(), *setup.connects.lock().unwrap()),
            (1, 0),
            "no resuming by itself"
        );
        setup.told.lock().unwrap().answer = None;
        setup.run(Command::Paused(false));
        let status = setup.status();
        assert_eq!(
            (status.problem, status.paused, status.index),
            (None, false, Some(1))
        );
        assert_eq!((setup.loads(), *setup.connects.lock().unwrap()), (2, 1));
        assert_eq!(setup.told.lock().unwrap().loads[1].start, 70.0);
    }

    #[test]
    fn a_track_taken_out_of_the_queue_gives_way_to_the_next_that_stayed() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["a", "b", "c"]), 1, 0.0));
        setup.run(Command::Requeue(
            tracks(&["a", "c"]),
            vec![Some(0), None, Some(1)],
        ));
        let status = setup.status();
        assert_eq!(
            (status.index, status.requeues, setup.loads()),
            (Some(1), 1, 2)
        );
        // One moved keeps playing, under its new place.
        setup.run(Command::Requeue(
            tracks(&["c", "a"]),
            vec![Some(1), Some(0)],
        ));
        let status = setup.status();
        assert_eq!(
            (status.index, status.requeues, setup.loads()),
            (Some(0), 2, 2)
        );
        // The one playing taken out with none after it ends the queue.
        setup.run(Command::Requeue(tracks(&[]), vec![None, None]));
        assert!(setup.status().ended);
    }

    #[test]
    fn a_track_that_cant_be_fetched_is_skipped_for_the_next() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["bad", "b"]), 0, 0.0));
        let status = setup.status();
        assert_eq!((status.index, setup.loads()), (Some(1), 1));
        assert_eq!(status.skipped.map(|(index, _)| index), Some(0));
        setup.run(Command::Play(tracks(&["a", "bad"]), 1, 0.0));
        assert!(setup.status().ended, "nothing after it plays");
    }

    #[test]
    fn each_track_is_kept_in_its_own_file_and_the_last_one_is_removed_when_replaced() {
        let mut setup = Setup::new();
        setup.run(Command::Play(tracks(&["a", "b"]), 0, 0.0));
        let kept = |folder: &PathBuf| {
            std::fs::read_dir(folder)
                .unwrap()
                .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
                .filter(|name| name.starts_with("network-"))
                .count()
        };
        assert_eq!(kept(&setup.folder), 1);
        setup.run(Command::Jump(1));
        assert_eq!(kept(&setup.folder), 1);
    }

    #[test]
    fn next_follows_the_queue_as_next_does_for_a_sound_device() {
        assert_eq!(following(0, 3, Repeat::Off), Some(1));
        assert_eq!(following(2, 3, Repeat::Off), None);
        assert_eq!(following(2, 3, Repeat::All), Some(0));
        assert_eq!(following(0, 0, Repeat::All), None);
        assert_eq!(after(2, 3, Repeat::One), Some(2));
        assert_eq!(after(2, 3, Repeat::Off), None);
    }
}
