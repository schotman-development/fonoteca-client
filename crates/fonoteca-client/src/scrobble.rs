//! Scrobbling to ListenBrainz while a token is set: tells it which track
//! just began, and submits a listen once a track was heard long enough.
//! Listens that can't be submitted yet wait in the preferences, and go out
//! at the next listen and at the next start. The server is told of each
//! track heard long enough too, for the albums it lists as played last.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use listenbrainz::{AdditionalInfo, Client, Listen, TrackMetadata};
use opensubsonic::Child;

use crate::{prefs, store};

/// ListenBrainz counts a listen after half the track, or this many seconds
/// of it, whichever comes first.
const LISTENED_ENOUGH_AT_MOST: f64 = 4.0 * 60.0;
/// How far the position may run ahead of the clock between two looks and
/// still be heard playing; more is a seek. The clock is what tells a seek
/// from a late look, which a phone in a pocket makes.
const SLACK_HEARD: f64 = 2.0;
const APP_NAME: &str = "Fonoteca";

/// What the player says, as of one look at it.
pub struct Heard<'a> {
    /// The track playing, by its id.
    pub song: Option<&'a str>,
    /// Its length in seconds, when known.
    pub duration: Option<f64>,
    pub position: f64,
    pub paused: bool,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// The track has begun, or goes on after the pause it was found in.
    Started,
    /// The track was heard long enough; it began at `started_at`, in
    /// seconds since 1970.
    Due { started_at: i64 },
}

/// Follows the player look by look and says when a track begins and when
/// it was heard long enough. It counts the seconds that played, so a seek
/// to the middle of a track is not a listen.
#[derive(Default)]
pub struct Tracker {
    play: Option<Play>,
}

struct Play {
    song: String,
    started_at: Option<i64>,
    heard: f64,
    position: f64,
    /// The clock at the last look, in seconds since 1970.
    at: f64,
    is_counted: bool,
}

impl Play {
    fn new(song: &str, position: f64, at: f64) -> Self {
        Play {
            song: song.to_owned(),
            started_at: None,
            heard: 0.0,
            position,
            at,
            is_counted: false,
        }
    }

    /// Whether the track jumped back to its start, as when it repeats, is
    /// played again or restarted: it is a new play, not a seek.
    fn has_restarted(&self, heard: &Heard) -> bool {
        heard.position <= SLACK_HEARD && self.position - heard.position > SLACK_HEARD
    }
}

impl Tracker {
    /// `at` is the clock, in seconds since 1970.
    pub fn observe(&mut self, at: f64, heard: &Heard) -> Option<Event> {
        let Some(song) = heard.song else {
            self.play = None;
            return None;
        };
        let continues =
            (self.play.as_ref()).is_some_and(|p| p.song == song && !p.has_restarted(heard));
        if !continues {
            self.play = Some(Play::new(song, heard.position, at));
        }
        let play = self.play.as_mut().expect("set above");
        let step = heard.position - play.position;
        let elapsed = (at - play.at).max(0.0);
        play.position = heard.position;
        play.at = at;
        if heard.paused {
            return None;
        }
        if (0.0..=elapsed + SLACK_HEARD).contains(&step) {
            play.heard += step.min(elapsed);
        }
        let Some(started_at) = play.started_at else {
            play.started_at = Some(at as i64);
            return Some(Event::Started);
        };
        if play.is_counted || play.heard < listened_enough(heard.duration) {
            return None;
        }
        play.is_counted = true;
        Some(Event::Due { started_at })
    }
}

fn listened_enough(duration: Option<f64>) -> f64 {
    duration.map_or(LISTENED_ENOUGH_AT_MOST, |duration| {
        (duration / 2.0).min(LISTENED_ENOUGH_AT_MOST)
    })
}

/// What ListenBrainz needs of `song`, or none when it lacks an artist or
/// a title, which it refuses.
fn metadata(song: &Child) -> Option<TrackMetadata> {
    let artist_name = song.artist.clone().filter(|artist| !artist.is_empty())?;
    if song.title.is_empty() {
        return None;
    }
    Some(TrackMetadata {
        artist_name,
        track_name: song.title.clone(),
        release_name: song.album.clone(),
        additional_info: AdditionalInfo {
            recording_mbid: song.music_brainz_id.clone(),
            tracknumber: song.track.map(|track| track.to_string()),
            duration: song
                .duration
                .and_then(|seconds| u64::try_from(seconds).ok()),
            media_player: Some(APP_NAME.to_owned()),
            submission_client: Some(APP_NAME.to_owned()),
        },
    })
}

#[derive(Default)]
pub struct Scrobbler {
    tracker: Tracker,
    outbox: Rc<Outbox>,
    #[cfg(test)]
    looks: u32,
    #[cfg(test)]
    flushes: Cell<u32>,
}

impl Scrobbler {
    /// With the listens an earlier run left waiting.
    pub fn load() -> Self {
        Scrobbler {
            outbox: Rc::new(Outbox::new(prefs::waiting_listens())),
            ..Self::default()
        }
    }

    /// Looks at what plays, `song` being the track playing, on `server`.
    pub fn observe(
        &mut self,
        song: Option<&Child>,
        position: f64,
        paused: bool,
        server: Option<Arc<opensubsonic::Client>>,
    ) {
        #[cfg(test)]
        {
            self.looks += 1;
        }
        let heard = heard_of(song, position, paused);
        let Some(event) = self.tracker.observe(clock(), &heard) else {
            return;
        };
        if let (Event::Due { started_at }, Some(song), Some(server)) = (&event, song, server) {
            report(server, song.id.clone(), *started_at);
        }
        let Some(track_metadata) = song.and_then(metadata) else {
            return;
        };
        let outbox = self.outbox.clone();
        slint::spawn_local(async move {
            react(client().await, &outbox, event, track_metadata).await;
        })
        .expect("the event loop is running");
    }

    /// Submits the listens waiting, if any.
    pub fn flush(&self) {
        #[cfg(test)]
        self.flushes.set(self.flushes.get() + 1);
        let outbox = self.outbox.clone();
        slint::spawn_local(async move {
            if let Some(client) = client().await {
                outbox.flush(&client).await;
            }
        })
        .expect("the event loop is running");
    }

    /// How often the player was looked at and the waiting listens were
    /// flushed, for the tests of the wiring.
    #[cfg(test)]
    pub fn calls(&self) -> (u32, u32) {
        (self.looks, self.flushes.get())
    }
}

/// The clock, in seconds since 1970.
fn clock() -> f64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0.0, |since| since.as_secs_f64())
}

fn heard_of(song: Option<&Child>, position: f64, paused: bool) -> Heard<'_> {
    Heard {
        song: song.map(|song| song.id.as_str()),
        duration: song
            .and_then(|song| song.duration)
            .filter(|seconds| *seconds > 0)
            .map(|seconds| seconds as f64),
        position,
        paused,
    }
}

/// Acts on `event` when there is a token, and not otherwise: without one
/// nothing is sent and no listen is kept.
async fn react(
    client: Option<Client>,
    outbox: &Outbox,
    event: Event,
    track_metadata: TrackMetadata,
) {
    let Some(client) = client else { return };
    match event {
        Event::Started => announce(&client, &track_metadata).await,
        Event::Due { started_at } => {
            outbox.add(Listen {
                listened_at: started_at,
                track_metadata,
            });
            outbox.flush(&client).await;
        }
    }
}

/// The client for the saved token, if there is one.
async fn client() -> Option<Client> {
    let token = store::load_listenbrainz().await?.token;
    // Tests must never reach the real ListenBrainz, which is someone's account.
    #[cfg(not(test))]
    let client = Client::new(token);
    #[cfg(test)]
    let client = Client::with_root(token, "http://127.0.0.1:9/1/");
    client
        .inspect_err(|e| eprintln!("ListenBrainz: no client: {e}"))
        .ok()
}

/// Tells `server` that `song` was played from `started_at`, in seconds since
/// 1970. A play it misses is only missing from its counts and lists.
fn report(server: Arc<opensubsonic::Client>, song: String, started_at: i64) {
    slint::spawn_local(async move {
        if let Err(e) = server.scrobble(&song, Some(started_at * 1000)).await {
            eprintln!("The server missed the play of {song}: {e}");
        }
    })
    .expect("the event loop is running");
}

/// A track just began. The listener only misses a stat when this fails.
async fn announce(client: &Client, track_metadata: &TrackMetadata) {
    if let Err(e) = client.playing_now(track_metadata).await {
        eprintln!("ListenBrainz playing now: {e}");
    }
}

/// The listens not yet submitted, oldest first.
#[derive(Default)]
struct Outbox {
    waiting: RefCell<Vec<Listen>>,
    is_flushing: Cell<bool>,
}

impl Outbox {
    fn new(waiting: Vec<Listen>) -> Self {
        Outbox {
            waiting: RefCell::new(waiting),
            is_flushing: Cell::new(false),
        }
    }

    fn add(&self, listen: Listen) {
        let mut waiting = self.waiting.borrow_mut();
        waiting.push(listen);
        prefs::save_waiting_listens(&waiting);
    }

    /// Submits what waits, unless another flush is under way, which would
    /// send the same listens twice.
    async fn flush(&self, client: &Client) {
        if self.waiting.borrow().is_empty() || self.is_flushing.replace(true) {
            return;
        }
        if let Err(e) = self.submit_all(client).await {
            eprintln!("ListenBrainz listens kept for later: {e}");
        }
        self.is_flushing.set(false);
    }

    /// Submits the listens in batches until none is left. Any failure but
    /// a listen refused as malformed stops it and keeps every listen for a
    /// later try, since a token that is invalid now may be replaced. A batch
    /// refused as malformed is split into single listens, so that only the
    /// ones it refuses are dropped.
    async fn submit_all(&self, client: &Client) -> listenbrainz::Result<()> {
        let mut batch_size = listenbrainz::MAX_LISTENS_PER_REQUEST;
        loop {
            let batch: Vec<Listen> = self
                .waiting
                .borrow()
                .iter()
                .take(batch_size)
                .cloned()
                .collect();
            if batch.is_empty() {
                return Ok(());
            }
            match client.submit_listens(&batch).await {
                Ok(()) => self.remove_first(batch.len()),
                Err(e) if !e.is_malformed_listen() => return Err(e),
                Err(e) => {
                    eprintln!("ListenBrainz refused {} listens: {e}", batch.len());
                    if batch.len() == 1 {
                        self.remove_first(1);
                    }
                    batch_size = 1;
                }
            }
        }
    }

    fn remove_first(&self, count: usize) {
        let mut waiting = self.waiting.borrow_mut();
        // New listens only join at the end while a batch is out.
        let count = count.min(waiting.len());
        waiting.drain(..count);
        prefs::save_waiting_listens(&waiting);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    fn heard(song: &str, duration: f64, position: f64, paused: bool) -> Heard<'_> {
        Heard {
            song: Some(song),
            duration: Some(duration),
            position,
            paused,
        }
    }

    /// The clock when the tests begin.
    const START: f64 = 1000.0;
    /// How often the player is looked at, in seconds.
    const TICK: f64 = 0.25;

    /// Plays `song` from `from` to `to` seconds in the player's ticks, the
    /// `clock` going on with them, and returns the events.
    fn play(
        tracker: &mut Tracker,
        clock: &mut f64,
        song: &str,
        duration: f64,
        from: f64,
        to: f64,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        let mut position = from;
        while position <= to {
            events.extend(tracker.observe(*clock, &heard(song, duration, position, false)));
            position += TICK;
            *clock += TICK;
        }
        events
    }

    fn due_after(duration: f64) -> f64 {
        let mut tracker = Tracker::default();
        let mut position = 0.0;
        loop {
            let at = START + position;
            let event = tracker.observe(at, &heard("a", duration, position, false));
            if matches!(event, Some(Event::Due { .. })) {
                return position;
            }
            position += TICK;
            assert!(position <= duration, "never due");
        }
    }

    #[test]
    fn a_listen_is_due_at_half_the_track_or_four_minutes() {
        assert_eq!(due_after(30.0), 15.0);
        assert_eq!(due_after(7.0 * 60.0), 210.0);
        assert_eq!(due_after(10.0 * 60.0), 240.0);
    }

    #[test]
    fn a_track_of_unknown_length_is_due_after_four_minutes() {
        let mut tracker = Tracker::default();
        let unknown = |position| Heard {
            song: Some("a"),
            duration: None,
            position,
            paused: false,
        };
        let mut due = None;
        for step in 0..=1000 {
            let position = step as f64 * TICK;
            let event = tracker.observe(START + position, &unknown(position));
            if due.is_none() && matches!(event, Some(Event::Due { .. })) {
                due = Some(position);
            }
        }
        assert_eq!(due, Some(240.0));
    }

    #[test]
    fn a_track_begins_once_and_is_counted_once_at_the_time_it_began() {
        let mut tracker = Tracker::default();
        let events = play(&mut tracker, &mut START.clone(), "a", 20.0, 0.0, 20.0);
        assert_eq!(events, [Event::Started, Event::Due { started_at: 1000 }]);
    }

    #[test]
    fn a_seek_past_the_half_is_not_a_listen() {
        let mut tracker = Tracker::default();
        let mut clock = START;
        let mut events = play(&mut tracker, &mut clock, "a", 100.0, 0.0, 10.0);
        events.extend(play(&mut tracker, &mut clock, "a", 100.0, 80.0, 90.0));
        assert_eq!(events, [Event::Started]);
        // 20 s were heard of the 50 s the half needs.
        let events = play(&mut tracker, &mut clock, "a", 100.0, 90.25, 100.0);
        assert!(events.is_empty());
    }

    #[test]
    fn a_pause_does_not_count_and_playing_goes_on_after_it() {
        let mut tracker = Tracker::default();
        let mut clock = START;
        let mut events = play(&mut tracker, &mut clock, "a", 100.0, 0.0, 40.0);
        for _ in 0..1000 {
            events.extend(tracker.observe(clock, &heard("a", 100.0, 40.0, true)));
            clock += TICK;
        }
        assert_eq!(events, [Event::Started]);
        events.extend(play(&mut tracker, &mut clock, "a", 100.0, 40.25, 50.0));
        assert_eq!(events, [Event::Started, Event::Due { started_at: 1000 }]);
    }

    #[test]
    fn a_track_found_paused_begins_when_it_plays() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.observe(5.0, &heard("a", 100.0, 30.0, true)), None);
        assert_eq!(
            tracker.observe(9.0, &heard("a", 100.0, 30.0, false)),
            Some(Event::Started)
        );
        let due = tracker.observe(9.0, &heard("a", 100.0, 30.0, false));
        assert_eq!(due, None);
    }

    #[test]
    fn the_next_track_and_a_repeat_each_count_again() {
        let mut tracker = Tracker::default();
        let mut clock = START;
        let mut events = play(&mut tracker, &mut clock, "a", 20.0, 0.0, 20.0);
        events.extend(play(&mut tracker, &mut clock, "b", 20.0, 0.0, 20.0));
        events.extend(play(&mut tracker, &mut clock, "b", 20.0, 0.0, 20.0));
        let started = events.iter().filter(|e| **e == Event::Started).count();
        assert_eq!((started, events.len()), (3, 6));
    }

    #[test]
    fn a_track_played_again_from_its_start_is_a_new_play_though_it_did_not_end() {
        let mut tracker = Tracker::default();
        let mut clock = START;
        let mut events = play(&mut tracker, &mut clock, "a", 100.0, 0.0, 60.0);
        events.extend(play(&mut tracker, &mut clock, "a", 100.0, 0.25, 60.0));
        let started = events.iter().filter(|e| **e == Event::Started).count();
        assert_eq!((started, events.len()), (2, 4), "{events:?}");
    }

    #[test]
    fn a_seek_back_to_the_middle_is_not_a_new_play() {
        let mut tracker = Tracker::default();
        let mut clock = START;
        let mut events = play(&mut tracker, &mut clock, "a", 100.0, 0.0, 60.0);
        events.extend(play(&mut tracker, &mut clock, "a", 100.0, 30.0, 60.0));
        assert_eq!(events, [Event::Started, Event::Due { started_at: 1000 }]);
    }

    #[test]
    fn nothing_playing_forgets_the_track() {
        let mut tracker = Tracker::default();
        let mut clock = START;
        play(&mut tracker, &mut clock, "a", 100.0, 0.0, 10.0);
        let nothing = Heard {
            song: None,
            duration: None,
            position: 0.0,
            paused: true,
        };
        assert_eq!(tracker.observe(clock, &nothing), None);
        let events = play(&mut tracker, &mut clock, "a", 100.0, 0.0, 1.0);
        assert_eq!(events, [Event::Started]);
    }

    #[test]
    fn a_live_station_has_no_song_and_is_never_counted() {
        let mut tracker = Tracker::default();
        let station = Heard {
            song: None,
            duration: None,
            position: 0.0,
            paused: false,
        };
        for minute in 0..30 {
            let heard = Heard {
                position: f64::from(minute) * 60.0,
                ..station
            };
            assert_eq!(tracker.observe(START + heard.position, &heard), None);
        }
    }

    #[test]
    fn playing_on_with_the_phone_asleep_is_heard_though_the_looks_are_late() {
        // Looks a minute apart, the position keeping up with the clock.
        let mut tracker = Tracker::default();
        let looks = [(0.0, 0.0), (60.0, 60.0), (120.0, 120.0)];
        let events: Vec<_> = looks
            .iter()
            .filter_map(|(position, since)| {
                tracker.observe(START + since, &heard("a", 200.0, *position, false))
            })
            .collect();
        assert_eq!(events, [Event::Started, Event::Due { started_at: 1000 }]);
    }

    #[test]
    fn a_seek_between_late_looks_is_still_a_seek() {
        // A minute passes, and the position moves 90 s on: 30 s were skipped.
        let mut tracker = Tracker::default();
        tracker.observe(START, &heard("a", 200.0, 0.0, false));
        tracker.observe(START + 60.0, &heard("a", 200.0, 90.0, false));
        let after = tracker.observe(START + 100.0, &heard("a", 200.0, 130.0, false));
        assert_eq!(after, None, "only 40 s were heard of the 100 s needed");
    }

    #[test]
    fn a_song_without_a_length_does_not_count_as_one_of_no_length() {
        let song = |duration: i64| -> Child {
            serde_json::from_str(&format!(
                r#"{{"id": "1", "title": "T", "isDir": false, "duration": {duration}}}"#
            ))
            .unwrap()
        };
        assert_eq!(heard_of(Some(&song(0)), 0.0, false).duration, None);
        assert_eq!(heard_of(Some(&song(90)), 0.0, false).duration, Some(90.0));
    }

    #[test]
    fn what_listenbrainz_is_told_of_a_song_is_its_own_tags() {
        let song: Child = serde_json::from_str(
            r#"{"id": "1", "title": "Title", "artist": "Artist", "album": "Album",
                "musicBrainzId": "8f3471b5-7e6a-48da-86a9-c1c07a0f47ae",
                "isDir": false, "track": 3, "duration": 90}"#,
        )
        .unwrap();
        let metadata = metadata(&song).unwrap();
        assert_eq!(metadata.track_name, "Title");
        assert_eq!(metadata.artist_name, "Artist");
        assert_eq!(metadata.release_name.as_deref(), Some("Album"));
        let info = metadata.additional_info;
        assert_eq!(
            info.recording_mbid.as_deref(),
            Some("8f3471b5-7e6a-48da-86a9-c1c07a0f47ae")
        );
        assert_eq!(info.tracknumber.as_deref(), Some("3"));
        assert_eq!(info.duration, Some(90));
    }

    /// Answers each request with the next status of `statuses` (the last
    /// one for every request after), and records the bodies.
    fn serve(statuses: Vec<u16>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let root = format!("http://{}/1/", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                respond(stream, &statuses, &seen);
            }
        });
        (root, bodies)
    }

    fn respond(mut stream: std::net::TcpStream, statuses: &[u16], seen: &Mutex<Vec<String>>) {
        let mut request = Vec::new();
        let mut chunk = [0; 4096];
        let body = loop {
            let read = stream.read(&mut chunk).unwrap_or(0);
            request.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&request).into_owned();
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = head
                .to_lowercase()
                .lines()
                .find_map(|line| line.strip_prefix("content-length: ")?.parse().ok())
                .unwrap_or(0usize);
            if body.len() >= length || read == 0 {
                break body.to_owned();
            }
        };
        let mut seen = seen.lock().unwrap();
        let status = statuses[seen.len().min(statuses.len() - 1)];
        seen.push(body);
        let payload = if status == 200 {
            r#"{"status": "ok"}"#
        } else {
            r#"{"code": 1, "error": "no"}"#
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
    }

    fn listen(at: i64) -> Listen {
        Listen {
            listened_at: at,
            track_metadata: TrackMetadata {
                artist_name: "A".into(),
                track_name: format!("T{at}"),
                release_name: None,
                additional_info: AdditionalInfo::default(),
            },
        }
    }

    fn submit(statuses: Vec<u16>, waiting: Vec<Listen>) -> (Vec<i64>, Vec<String>, bool) {
        let _saving = persisting();
        let (root, bodies) = serve(statuses);
        let client = Client::with_root("token", &root).unwrap();
        let outbox = Outbox::new(waiting);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(outbox.submit_all(&client));
        let left = outbox
            .waiting
            .borrow()
            .iter()
            .map(|l| l.listened_at)
            .collect();
        let bodies = bodies.lock().unwrap().clone();
        (left, bodies, result.is_ok())
    }

    #[test]
    fn waiting_listens_go_out_together_as_an_import() {
        let (left, bodies, ok) = submit(vec![200], vec![listen(1), listen(2)]);
        assert!(ok && left.is_empty());
        assert_eq!(bodies.len(), 1);
        assert!(
            bodies[0].contains(r#""listen_type":"import""#),
            "{}",
            bodies[0]
        );
    }

    #[test]
    fn a_failing_server_or_network_keeps_the_listens() {
        for status in [500, 503, 429] {
            let (left, bodies, ok) = submit(vec![status], vec![listen(1), listen(2)]);
            assert!(!ok);
            assert_eq!((left, bodies.len()), (vec![1, 2], 1), "status {status}");
        }
        let unreachable = Client::with_root("t", "http://127.0.0.1:9/1/").unwrap();
        let outbox = Outbox::new(vec![listen(1)]);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert!(runtime.block_on(outbox.submit_all(&unreachable)).is_err());
        assert_eq!(outbox.waiting.borrow().len(), 1);
    }

    #[test]
    fn a_listen_listenbrainz_refuses_is_dropped_and_the_others_still_go() {
        // The batch is refused; then 1 is refused alone and 2 is taken.
        let (left, bodies, ok) = submit(vec![400, 400, 200], vec![listen(1), listen(2)]);
        assert!(ok && left.is_empty());
        assert_eq!(bodies.len(), 3);
        assert!(bodies[2].contains(r#""listen_type":"single""#));
        assert!(bodies[2].contains("T2"));
    }

    #[test]
    fn a_refused_token_or_address_keeps_the_listens_until_a_later_try_succeeds() {
        let _saving = persisting();
        for status in [401, 403, 404] {
            let (root, bodies) = serve(vec![status, 200]);
            let client = Client::with_root("token", &root).unwrap();
            let outbox = Outbox::new(vec![listen(1), listen(2)]);
            let runtime = runtime();
            assert!(runtime.block_on(outbox.submit_all(&client)).is_err());
            assert_eq!(outbox.waiting.borrow().len(), 2, "status {status}");
            assert_eq!(bodies.lock().unwrap().len(), 1, "stopped at the first");
            assert!(runtime.block_on(outbox.submit_all(&client)).is_ok());
            assert!(outbox.waiting.borrow().is_empty(), "status {status}");
            assert_eq!(bodies.lock().unwrap().len(), 2);
        }
    }

    #[test]
    fn waiting_listens_survive_a_restart() {
        let _saving = persisting();
        let outbox = Outbox::new(Vec::new());
        outbox.add(listen(7));
        let after_restart = Scrobbler::load();
        assert_eq!(
            after_restart.outbox.waiting.borrow().as_slice(),
            [listen(7)]
        );
    }

    #[test]
    fn a_song_without_an_artist_is_not_submitted() {
        let song: Child =
            serde_json::from_str(r#"{"id": "1", "title": "T", "isDir": false}"#).unwrap();
        assert!(metadata(&song).is_none());
    }

    /// The tests that save the waiting listens share one saved value.
    static PERSISTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn persisting() -> std::sync::MutexGuard<'static, ()> {
        PERSISTING.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn without_a_token_nothing_is_announced_and_no_listen_is_kept() {
        let outbox = Outbox::new(Vec::new());
        let track = listen(1).track_metadata;
        runtime().block_on(async {
            react(None, &outbox, Event::Started, track.clone()).await;
            for _ in 0..3 {
                react(None, &outbox, Event::Due { started_at: 5 }, track.clone()).await;
            }
        });
        assert!(outbox.waiting.borrow().is_empty());
    }

    #[test]
    fn with_a_token_a_due_listen_is_submitted() {
        let _saving = persisting();
        let (root, bodies) = serve(vec![200]);
        let client = Client::with_root("token", &root).unwrap();
        let outbox = Outbox::new(Vec::new());
        let track = listen(1).track_metadata;
        runtime().block_on(react(
            Some(client),
            &outbox,
            Event::Due { started_at: 5 },
            track,
        ));
        assert!(outbox.waiting.borrow().is_empty());
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].contains(r#""listened_at":5"#), "{}", bodies[0]);
    }

    #[test]
    fn a_track_that_begins_is_announced_as_playing_now_without_a_time() {
        let (root, bodies) = serve(vec![200]);
        let client = Client::with_root("token", &root).unwrap();
        let outbox = Outbox::new(Vec::new());
        let track = listen(1).track_metadata;
        runtime().block_on(react(Some(client), &outbox, Event::Started, track));
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        assert!(
            bodies[0].contains(r#""listen_type":"playing_now""#),
            "{}",
            bodies[0]
        );
        assert!(!bodies[0].contains("listened_at"), "{}", bodies[0]);
        assert!(outbox.waiting.borrow().is_empty(), "it is not a listen");
    }

    #[test]
    fn a_flush_under_way_is_not_joined_by_another() {
        let _saving = persisting();
        let (root, bodies) = serve(vec![200]);
        let client = Client::with_root("token", &root).unwrap();
        let outbox = Outbox::new(vec![listen(1)]);
        outbox.is_flushing.set(true);
        runtime().block_on(outbox.flush(&client));
        assert!(bodies.lock().unwrap().is_empty());
        assert_eq!(outbox.waiting.borrow().len(), 1);
        // One that is done lets the next go.
        outbox.is_flushing.set(false);
        runtime().block_on(outbox.flush(&client));
        assert_eq!(bodies.lock().unwrap().len(), 1);
        assert!(!outbox.is_flushing.get());
    }
}
