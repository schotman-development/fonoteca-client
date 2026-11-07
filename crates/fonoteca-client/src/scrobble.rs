//! Scrobbling to the server: tells it which track just began, as playing
//! now, and of each track heard long enough, from when it began. The server
//! passes them on, to ListenBrainz too once that is linked there. Plays it
//! can't be told of yet wait on this device for its account, and go oldest
//! first, each with its own time, at the next track, the next play, the next
//! start and as the account signs out, which forgets the ones still left.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Poll, Waker};

use opensubsonic::Child;
use serde::{Deserialize, Serialize};

use crate::prefs;

/// ListenBrainz counts a listen after half the track, or this many seconds
/// of it, whichever comes first.
const LISTENED_ENOUGH_AT_MOST: f64 = 4.0 * 60.0;
/// How far the position may run ahead of the clock between two looks and
/// still be heard playing; more is a seek. The clock is what tells a seek
/// from a late look, which a phone in a pocket makes.
const SLACK_HEARD: f64 = 2.0;
/// The errors by which the server refuses a play for good: a call it finds
/// a parameter missing from, and a song it doesn't have.
const MISSING_PARAMETER: i64 = 10;
const NOT_FOUND: i64 = 70;

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

/// The server signed in to, and what names its account on this device.
pub struct Server {
    pub client: Arc<opensubsonic::Client>,
    pub account: String,
}

/// A play the server is still to be told of: the song, and when it began,
/// in seconds since 1970.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingPlay {
    pub id: String,
    pub started_at: i64,
}

#[derive(Default)]
pub struct Scrobbler {
    tracker: Tracker,
    /// The plays waiting for each account's server, by account.
    outboxes: HashMap<String, Rc<Outbox>>,
    #[cfg(test)]
    looks: u32,
    #[cfg(test)]
    sends: u32,
}

impl Scrobbler {
    /// Looks at what plays, `song` being the track playing, on `server`.
    /// Says when it began once it was heard long enough, with a server or
    /// without.
    pub fn observe(
        &mut self,
        song: Option<&Child>,
        position: f64,
        paused: bool,
        server: Option<Server>,
    ) -> Option<i64> {
        #[cfg(test)]
        {
            self.looks += 1;
        }
        let heard = heard_of(song, position, paused);
        let event = self.tracker.observe(clock(), &heard)?;
        if let (Some(song), Some(server)) = (song, server) {
            self.tell(&event, song, server);
        }
        match event {
            Event::Due { started_at } => Some(started_at),
            Event::Started => None,
        }
    }

    /// Tells `server` that `song` plays now, or that it was played, which
    /// waits behind the plays before it until the server has them all.
    fn tell(&mut self, event: &Event, song: &Child, server: Server) {
        let outbox = self.outbox(&server.account);
        match *event {
            Event::Started => announce(server.client, song.id.clone(), outbox),
            Event::Due { started_at } => {
                outbox.add(WaitingPlay {
                    id: song.id.clone(),
                    started_at,
                });
                send(server.client, outbox);
            }
        }
    }

    /// Sends the plays waiting for `server`, unless a send is under way.
    pub fn send_waiting(&mut self, server: Server) -> impl Future<Output = ()> + use<> {
        #[cfg(test)]
        {
            self.sends += 1;
        }
        let outbox = self.outbox(&server.account);
        async move { outbox.send(&server.client).await }
    }

    /// Sends the plays waiting for `server` once the send under way, if any,
    /// has ended: the last try before its account signs out.
    pub fn send_last(&mut self, server: Server) -> impl Future<Output = ()> + use<> {
        let outbox = self.outbox(&server.account);
        async move { outbox.send_last(&server.client).await }
    }

    /// How many plays wait for the server of `account`.
    pub fn waiting(&mut self, account: &str) -> usize {
        self.outbox(account).waiting.borrow().len()
    }

    /// Forgets the plays still waiting for the server of `account`, as it
    /// signs out.
    pub fn forget(&mut self, account: &str) -> std::io::Result<()> {
        if let Some(outbox) = self.outboxes.remove(account) {
            outbox.waiting.borrow_mut().clear();
        }
        prefs::forget_waiting_plays(account)
    }

    /// The plays waiting for the server of `account`, read from this device
    /// the first time.
    fn outbox(&mut self, account: &str) -> Rc<Outbox> {
        let outbox = (self.outboxes.entry(account.to_owned()))
            .or_insert_with(|| Rc::new(Outbox::load(account)));
        outbox.clone()
    }

    /// How often the player was looked at and what waits was sent, for the
    /// tests of the wiring.
    #[cfg(test)]
    pub fn calls(&self) -> (u32, u32) {
        (self.looks, self.sends)
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

/// Tells `server` that `song` plays now, and once it has answered, the plays
/// waiting for it. The listener only misses a "playing now" when this fails.
fn announce(server: Arc<opensubsonic::Client>, song: String, outbox: Rc<Outbox>) {
    slint::spawn_local(async move {
        match server.scrobble(&song, None, false).await {
            Ok(()) => outbox.send(&server).await,
            Err(e) => eprintln!("The server wasn't told {song} plays now: {e}"),
        }
    })
    .expect("the event loop is running");
}

fn send(server: Arc<opensubsonic::Client>, outbox: Rc<Outbox>) {
    slint::spawn_local(async move { outbox.send(&server).await })
        .expect("the event loop is running");
}

/// The plays the server of one account is still to be told of, oldest
/// first, kept on this device until it has them.
struct Outbox {
    account: String,
    waiting: RefCell<Vec<WaitingPlay>>,
    is_sending: Cell<bool>,
    /// The last tries waiting for the send under way to end.
    send_ended: RefCell<Vec<Waker>>,
}

impl Outbox {
    fn load(account: &str) -> Self {
        Outbox {
            account: account.to_owned(),
            waiting: RefCell::new(prefs::waiting_plays(account)),
            is_sending: Cell::new(false),
            send_ended: RefCell::default(),
        }
    }

    fn add(&self, play: WaitingPlay) {
        let mut waiting = self.waiting.borrow_mut();
        waiting.push(play);
        prefs::save_waiting_plays(&self.account, &waiting);
    }

    /// Tells `server` of what waits, unless another send is under way, which
    /// would tell it of the same plays twice.
    async fn send(&self, server: &opensubsonic::Client) {
        if self.is_sending.replace(true) {
            return;
        }
        if let Err(e) = self.send_all(server).await {
            eprintln!("Plays kept for the server: {e}");
        }
        self.is_sending.set(false);
        for waker in self.send_ended.take() {
            waker.wake();
        }
    }

    /// Tells `server` of what waits once the send under way, if any, has
    /// ended, which may have stopped short of the plays behind it.
    async fn send_last(&self, server: &opensubsonic::Client) {
        std::future::poll_fn(|context| {
            if !self.is_sending.get() {
                return Poll::Ready(());
            }
            self.send_ended.borrow_mut().push(context.waker().clone());
            Poll::Pending
        })
        .await;
        self.send(server).await;
    }

    /// Tells the server of each play in turn until none is left. A play it
    /// refuses for good is dropped. Any other failure stops it and keeps
    /// the plays for a later try: the network, the server itself or the
    /// login may be fine by then.
    async fn send_all(&self, server: &opensubsonic::Client) -> opensubsonic::Result<()> {
        loop {
            let Some(play) = self.waiting.borrow().first().cloned() else {
                return Ok(());
            };
            let time = Some(play.started_at * 1000);
            match server.scrobble(&play.id, time, true).await {
                Ok(()) => {}
                Err(e) if is_refused_for_good(&e) => {
                    eprintln!("The server refused the play of {}: {e}", play.id);
                }
                Err(e) => return Err(e),
            }
            self.remove(&play);
        }
    }

    /// Takes `play` off the front, unless its account signed out while it
    /// went, which forgot it already.
    fn remove(&self, play: &WaitingPlay) {
        let mut waiting = self.waiting.borrow_mut();
        if waiting.first() != Some(play) {
            return;
        }
        waiting.remove(0);
        prefs::save_waiting_plays(&self.account, &waiting);
    }
}

/// Whether the server refused a play in a way that telling it again won't
/// change.
fn is_refused_for_good(e: &opensubsonic::Error) -> bool {
    matches!(e, opensubsonic::Error::Api(problem) if matches!(problem.code, MISSING_PARAMETER | NOT_FOUND))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

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
    fn a_play_heard_long_enough_is_counted_for_the_log_without_a_server() {
        let song: Child =
            serde_json::from_str(r#"{"id": "1", "title": "T", "isDir": false, "duration": 1}"#)
                .unwrap();
        let mut scrobbler = Scrobbler::default();
        let began = clock() as i64;
        assert_eq!(scrobbler.observe(Some(&song), 0.0, false, None), None);
        std::thread::sleep(std::time::Duration::from_millis(600));
        let counted = scrobbler.observe(Some(&song), 0.6, false, None);
        assert!(
            counted.is_some_and(|at| (at - began).abs() <= 1),
            "{counted:?}"
        );
        assert_eq!(
            scrobbler.observe(Some(&song), 0.7, false, None),
            None,
            "once"
        );
    }

    const OK: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;

    fn reply(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn taken() -> String {
        reply("200 OK", OK)
    }

    fn down() -> String {
        reply("503 Service Unavailable", "")
    }

    fn refused(code: i64) -> String {
        let body = format!(
            r#"{{"subsonic-response":{{"status":"failed","version":"1.16.1","error":{{"code":{code},"message":"no"}}}}}}"#
        );
        reply("200 OK", &body)
    }

    /// A server that answers each call with the next of `replies` (the last
    /// one for every call after), and what it was told: each call's
    /// parameters after the login.
    fn serve(replies: Vec<String>) -> (opensubsonic::Client, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let told = Arc::new(Mutex::new(Vec::new()));
        let seen = told.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = BufReader::new(stream);
                let mut request = String::new();
                while stream.read_line(&mut request).unwrap_or(0) > 2 {}
                let line = request.lines().next().unwrap_or_default();
                let target = line.split(' ').nth(1).unwrap_or_default();
                let asked = target.split_once("&id=").map_or("", |(_, asked)| asked);
                let mut seen = seen.lock().unwrap();
                let reply = &replies[seen.len().min(replies.len() - 1)];
                seen.push(format!("id={asked}"));
                let _ = stream.get_mut().write_all(reply.as_bytes());
            }
        });
        (client(&address), told)
    }

    fn client(address: &str) -> opensubsonic::Client {
        let login = opensubsonic::Auth::Password {
            username: "u".into(),
            password: "p".into(),
        };
        opensubsonic::Client::new(address, login).unwrap()
    }

    fn waiting(id: &str, started_at: i64) -> WaitingPlay {
        WaitingPlay {
            id: id.into(),
            started_at,
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    /// An outbox for `account` holding `plays`, as a play heard adds them.
    fn outbox_with(account: &str, plays: &[WaitingPlay]) -> Outbox {
        prefs::save_waiting_plays(account, &[]);
        let outbox = Outbox::load(account);
        for play in plays {
            outbox.add(play.clone());
        }
        outbox
    }

    #[test]
    fn a_play_waits_while_the_server_cant_be_reached_and_goes_later_with_its_own_time() {
        let account = "unreachable-then-back";
        let outbox = outbox_with(account, &[waiting("s1", 1000)]);
        runtime().block_on(outbox.send(&client("http://127.0.0.1:9")));
        assert_eq!(prefs::waiting_plays(account), [waiting("s1", 1000)]);
        // Read back from this device, as at the next start.
        let after_restart = Outbox::load(account);
        let (server, told) = serve(vec![taken()]);
        runtime().block_on(after_restart.send(&server));
        assert_eq!(
            *told.lock().unwrap(),
            ["id=s1&time=1000000&submission=true"]
        );
        assert!(prefs::waiting_plays(account).is_empty());
        assert!(after_restart.waiting.borrow().is_empty());
    }

    #[test]
    fn waiting_plays_go_oldest_first_each_with_its_own_time() {
        let outbox = outbox_with("in-order", &[waiting("s1", 1000), waiting("s2", 2000)]);
        let (server, told) = serve(vec![taken()]);
        runtime().block_on(outbox.send(&server));
        assert_eq!(
            *told.lock().unwrap(),
            [
                "id=s1&time=1000000&submission=true",
                "id=s2&time=2000000&submission=true"
            ]
        );
        assert!(prefs::waiting_plays("in-order").is_empty());
    }

    #[test]
    fn a_failing_server_or_a_refused_login_keeps_the_plays_and_stops_at_the_first() {
        let wrong_login = 40;
        let generic = 0;
        for reply in [down(), refused(wrong_login), refused(generic)] {
            let account = "kept-on-failure";
            let both = [waiting("s1", 1000), waiting("s2", 2000)];
            let outbox = outbox_with(account, &both);
            let (server, told) = serve(vec![reply.clone()]);
            runtime().block_on(outbox.send(&server));
            assert_eq!(told.lock().unwrap().len(), 1, "{reply}");
            assert_eq!(prefs::waiting_plays(account), both, "{reply}");
        }
    }

    #[test]
    fn a_play_the_server_refuses_for_good_is_dropped_and_the_next_still_goes() {
        for code in [MISSING_PARAMETER, NOT_FOUND] {
            let account = "refused-for-good";
            let outbox = outbox_with(account, &[waiting("gone", 1000), waiting("s2", 2000)]);
            let (server, told) = serve(vec![refused(code), taken()]);
            runtime().block_on(outbox.send(&server));
            assert_eq!(
                *told.lock().unwrap(),
                [
                    "id=gone&time=1000000&submission=true",
                    "id=s2&time=2000000&submission=true"
                ]
            );
            assert!(prefs::waiting_plays(account).is_empty(), "{code}");
        }
    }

    #[test]
    fn a_send_under_way_is_not_joined_by_another() {
        let outbox = outbox_with("sending", &[waiting("s1", 1000)]);
        let (server, told) = serve(vec![taken()]);
        outbox.is_sending.set(true);
        runtime().block_on(outbox.send(&server));
        assert!(told.lock().unwrap().is_empty());
        assert_eq!(outbox.waiting.borrow().len(), 1);
        // One that is done lets the next go.
        outbox.is_sending.set(false);
        runtime().block_on(outbox.send(&server));
        assert_eq!(told.lock().unwrap().len(), 1);
        assert!(!outbox.is_sending.get());
    }

    #[test]
    fn an_account_signing_out_sends_its_plays_once_more_and_then_forgets_the_rest() {
        let account = "signing-out";
        prefs::save_waiting_plays(account, &[waiting("s1", 1000), waiting("s2", 2000)]);
        let mut scrobbler = Scrobbler::default();
        let (server, told) = serve(vec![taken(), down()]);
        let server = Server {
            client: Arc::new(server),
            account: account.into(),
        };
        runtime().block_on(scrobbler.send_waiting(server));
        assert_eq!(
            told.lock().unwrap()[0],
            "id=s1&time=1000000&submission=true"
        );
        assert_eq!(prefs::waiting_plays(account), [waiting("s2", 2000)]);
        scrobbler.forget(account).unwrap();
        assert!(prefs::waiting_plays(account).is_empty());
        assert!(!scrobbler.outboxes.contains_key(account));
    }

    #[test]
    fn an_account_signing_out_waits_for_the_send_under_way_and_tries_the_plays_behind_it() {
        let account = "sending-as-it-signs-out";
        let plays = [
            waiting("s1", 1000),
            waiting("s2", 2000),
            waiting("s3", 3000),
        ];
        prefs::save_waiting_plays(account, &plays);
        let mut scrobbler = Scrobbler::default();
        let (server, told) = serve(vec![taken(), down()]);
        let server = Arc::new(server);
        let leaving = Server {
            client: server.clone(),
            account: account.into(),
        };
        let outbox = scrobbler.outbox(account);
        let tasks = tokio::task::LocalSet::new();
        tasks.block_on(&runtime(), async {
            let under_way = tokio::task::spawn_local(async move { outbox.send(&server).await });
            while !scrobbler.outbox(account).is_sending.get() {
                tokio::task::yield_now().await;
            }
            scrobbler.send_last(leaving).await;
            assert!(under_way.is_finished(), "the send under way ended first");
        });
        // The send under way stopped at s2; the last try tried it again.
        assert_eq!(
            *told.lock().unwrap(),
            [
                "id=s1&time=1000000&submission=true",
                "id=s2&time=2000000&submission=true",
                "id=s2&time=2000000&submission=true"
            ]
        );
        scrobbler.forget(account).unwrap();
        assert!(prefs::waiting_plays(account).is_empty());
    }

    #[test]
    fn a_send_under_way_as_its_account_signs_out_goes_no_further_and_keeps_nothing() {
        let account = "taken-while-signing-out";
        prefs::save_waiting_plays(account, &[waiting("s1", 1000), waiting("s2", 2000)]);
        let mut scrobbler = Scrobbler::default();
        let (server, told) = serve(vec![taken()]);
        let outbox = scrobbler.outbox(account);
        let sending = outbox.clone();
        let tasks = tokio::task::LocalSet::new();
        tasks.block_on(&runtime(), async {
            let under_way = tokio::task::spawn_local(async move { sending.send(&server).await });
            while !outbox.is_sending.get() {
                tokio::task::yield_now().await;
            }
            scrobbler.forget(account).unwrap();
            under_way.await.unwrap();
        });
        // The server took s1 as the account signed out.
        assert_eq!(
            *told.lock().unwrap(),
            ["id=s1&time=1000000&submission=true"]
        );
        assert!(outbox.waiting.borrow().is_empty());
        assert!(prefs::waiting_plays(account).is_empty());
    }
}
