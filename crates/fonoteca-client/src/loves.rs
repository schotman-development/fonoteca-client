//! Hearts both ways between ListenBrainz and the server. A heart set or
//! cleared in the app goes to ListenBrainz by the song's recording ID; a love
//! added or removed on ListenBrainz stars or unstars the matching song on
//! the server. Each side is compared with a kept copy of what both agreed on
//! at the last sync, so a heart removed on one side is not put back from the
//! other, and a heart that could not be sent is found again next time.
//!
//! Songs are told apart by their recording ID. A starred song without one
//! can't be sent, so it stays on the server and is counted. The first sync
//! has no kept copy, which makes it a union: nothing is removed from either
//! side.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use listenbrainz as lb;
use opensubsonic::{Child, Client};
use serde::{Deserialize, Serialize};
use slint::ComponentHandle;

use crate::browse::matching::{Found, lookup};
use crate::browse::{self, Shared};
use crate::{AppWindow, prefs};

/// Songs starred in one call.
const STAR_BATCH: usize = 50;
/// How long a love the library has no song of goes unsearched. Searching
/// costs the server one or two calls per love, which a large account's
/// loves that are not in the library would repeat at every sync.
const UNMATCHED_RECHECK_SECS: u64 = 7 * 24 * 60 * 60;

/// What ListenBrainz and the server agreed on at the last sync, kept per
/// ListenBrainz user and server account. It holds no secret.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Kept {
    /// The recordings loved on one side and starred on the other.
    loved: BTreeSet<String>,
    /// The server song starred for a recording, where it was found by its
    /// title and artist and so has not the recording's ID.
    songs: BTreeMap<String, String>,
    /// When a love was last searched for in vain, in seconds since 1970.
    unmatched: BTreeMap<String, u64>,
}

impl Kept {
    fn was_searched_lately(&self, recording: &str, now: u64) -> bool {
        (self.unmatched.get(recording)).is_some_and(|at| now < at + UNMATCHED_RECHECK_SECS)
    }
}

/// What each recording needs, by where it is loved, starred or was agreed.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    /// Loved on ListenBrainz since the last sync: star it.
    star: Vec<String>,
    /// Unloved on ListenBrainz since the last sync: unstar it.
    unstar: Vec<String>,
    /// Starred on the server since the last sync: love it.
    love: Vec<String>,
    /// Unstarred on the server since the last sync: clear its love.
    clear: Vec<String>,
    /// Loved and starred already.
    agreed: BTreeSet<String>,
}

fn plan(loved: &BTreeSet<String>, starred: &BTreeSet<String>, kept: &BTreeSet<String>) -> Plan {
    let mut plan = Plan::default();
    for recording in loved.union(starred) {
        let recording = recording.clone();
        match (
            loved.contains(&recording),
            starred.contains(&recording),
            kept.contains(&recording),
        ) {
            (true, true, _) => {
                plan.agreed.insert(recording);
            }
            (true, false, false) => plan.star.push(recording),
            (true, false, true) => plan.clear.push(recording),
            (false, true, false) => plan.love.push(recording),
            (false, true, true) => plan.unstar.push(recording),
            (false, false, _) => {}
        }
    }
    plan
}

/// The server's starred songs by the recording each stands for, and how
/// many have none.
fn starred_by_recording(songs: &[Child], kept: &Kept) -> (BTreeMap<String, Vec<String>>, usize) {
    let found_for: HashMap<&str, &str> = (kept.songs.iter())
        .map(|(recording, song)| (song.as_str(), recording.as_str()))
        .collect();
    let mut by_recording: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unsendable = 0;
    for song in songs {
        let recording = (found_for.get(song.id.as_str()).map(|r| r.to_string()))
            .or_else(|| song.music_brainz_id.as_ref().map(|r| r.to_lowercase()));
        match recording {
            Some(recording) => by_recording
                .entry(recording)
                .or_default()
                .push(song.id.clone()),
            None => unsendable += 1,
        }
    }
    (by_recording, unsendable)
}

/// The track to look for in the library for a love, when ListenBrainz said
/// what it is called.
fn wanted(love: &lb::Feedback, recording: &str) -> Option<lb::PlaylistTrack> {
    let named = love.track_metadata.as_ref()?;
    Some(lb::PlaylistTrack {
        recording_mbid: Some(recording.to_owned()),
        title: named.track_name.clone(),
        artist: named.artist_name.clone(),
        artists: vec![named.artist_name.clone()],
        album: named.release_name.clone(),
        duration_ms: None,
    })
}

/// Both sides of a sync.
pub struct Sides<'a> {
    pub listenbrainz: &'a lb::Client,
    pub user: &'a str,
    pub server: &'a Client,
    pub found: &'a Found,
}

/// What a sync did.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The server songs it starred and unstarred.
    pub starred: Vec<String>,
    pub unstarred: Vec<String>,
    /// How many starred songs have no recording ID, so can't be sent.
    pub unsendable: usize,
    /// What went wrong; the hearts it concerns are tried again at the next sync.
    pub problems: Vec<String>,
    /// To keep for the next sync.
    pub kept: Kept,
}

pub async fn reconcile(sides: &Sides<'_>, kept: Kept, now: u64) -> Outcome {
    let (loves, starred) = match fetch(sides).await {
        Ok(fetched) => fetched,
        Err(problem) => {
            return Outcome {
                problems: vec![problem],
                kept,
                ..Outcome::default()
            };
        }
    };
    let (by_recording, unsendable) = starred_by_recording(&starred, &kept);
    let loved_keys = loves.keys().cloned().collect();
    let starred_keys = by_recording.keys().cloned().collect();
    let plan = plan(&loved_keys, &starred_keys, &kept.loved);
    let mut run = Run {
        sides,
        was: kept,
        next: Kept::default(),
        outcome: Outcome {
            unsendable,
            ..Outcome::default()
        },
    };
    run.next.loved = plan.agreed.clone();
    // Kept until the call that removes them succeeds.
    run.next
        .loved
        .extend(plan.clear.iter().chain(&plan.unstar).cloned());
    run.star_new(&plan.star, &loves, now).await;
    run.unstar_gone(&plan.unstar, &by_recording).await;
    run.love_new(&plan.love).await;
    run.clear_gone(&plan.clear).await;
    run.finish()
}

/// What ListenBrainz loves, by recording, and the server's starred songs.
async fn fetch(sides: &Sides<'_>) -> Result<(BTreeMap<String, lb::Feedback>, Vec<Child>), String> {
    let loves = (sides.listenbrainz.loved_recordings(sides.user).await)
        .map_err(|e| format!("loves couldn't be fetched from ListenBrainz: {e}"))?;
    let starred = (sides.server.get_starred2(None).await)
        .map_err(|e| format!("stars couldn't be fetched from the server: {e}"))?;
    let loves = loves.into_iter().filter_map(|love| {
        let recording = love.recording_mbid.as_ref()?.to_lowercase();
        Some((recording, love))
    });
    Ok((loves.collect(), starred.song))
}

struct Run<'a, 'b> {
    sides: &'a Sides<'b>,
    was: Kept,
    next: Kept,
    outcome: Outcome,
}

impl Run<'_, '_> {
    fn finish(mut self) -> Outcome {
        let loved = &self.next.loved;
        let songs = (self.was.songs.iter())
            .filter(|(recording, _)| loved.contains(*recording))
            .map(|(recording, song)| (recording.clone(), song.clone()));
        let songs: Vec<_> = songs.collect();
        for (recording, song) in songs {
            self.next.songs.entry(recording).or_insert(song);
        }
        self.outcome.kept = self.next;
        self.outcome
    }

    fn fail(&mut self, what: &str, e: impl std::fmt::Display) {
        self.outcome.problems.push(format!("{what}: {e}"));
    }

    /// Stars the library's songs of what is newly loved.
    async fn star_new(
        &mut self,
        recordings: &[String],
        loves: &BTreeMap<String, lb::Feedback>,
        now: u64,
    ) {
        let mut to_star = Vec::new();
        for recording in recordings {
            if self.was.was_searched_lately(recording, now) {
                self.next
                    .unmatched
                    .insert(recording.clone(), self.was.unmatched[recording]);
                continue;
            }
            let found = match wanted(&loves[recording], recording) {
                Some(track) => lookup(self.sides.found, self.sides.server, &track).await,
                None => Ok(None),
            };
            match found {
                Ok(Some(song)) => to_star.push((recording, song.id)),
                Ok(None) => {
                    self.next.unmatched.insert(recording.clone(), now);
                }
                Err(e) => return self.fail("a love couldn't be looked up on the server", e),
            }
        }
        for batch in to_star.chunks(STAR_BATCH) {
            let ids: Vec<&str> = batch.iter().map(|(_, id)| id.as_str()).collect();
            if let Err(e) = self.sides.server.star(&ids, &[], &[]).await {
                return self.fail("songs couldn't be starred on the server", e);
            }
            for (recording, id) in batch {
                self.next.loved.insert((*recording).clone());
                self.next.songs.insert((*recording).clone(), id.clone());
                self.outcome.starred.push(id.clone());
            }
        }
    }

    /// Unstars the songs of what is no longer loved.
    async fn unstar_gone(&mut self, recordings: &[String], songs: &BTreeMap<String, Vec<String>>) {
        for recording in recordings {
            let ids: Vec<&str> = songs[recording].iter().map(String::as_str).collect();
            if let Err(e) = self.sides.server.unstar(&ids, &[], &[]).await {
                return self.fail("songs couldn't be unstarred on the server", e);
            }
            self.next.loved.remove(recording);
            self.outcome
                .unstarred
                .extend(songs[recording].iter().cloned());
        }
    }

    async fn love_new(&mut self, recordings: &[String]) {
        for recording in recordings {
            match self.send(recording, lb::Score::Love).await {
                Some(true) => {
                    self.next.loved.insert(recording.clone());
                }
                Some(false) => {}
                None => return,
            }
        }
    }

    async fn clear_gone(&mut self, recordings: &[String]) {
        for recording in recordings {
            match self.send(recording, lb::Score::Clear).await {
                Some(true) => {
                    self.next.loved.remove(recording);
                }
                Some(false) => {}
                None => return,
            }
        }
    }

    /// Whether the heart was sent, or none when the sending should stop: a
    /// recording ListenBrainz refuses as malformed is skipped so that it
    /// doesn't hold up the others, but a rate limit, a bad token or no
    /// network would fail every heart after it too.
    async fn send(&mut self, recording: &str, score: lb::Score) -> Option<bool> {
        let sent = self
            .sides
            .listenbrainz
            .submit_feedback(recording, score)
            .await;
        let Err(e) = sent else { return Some(true) };
        self.fail("a heart couldn't be sent to ListenBrainz", &e);
        e.is_malformed_listen().then_some(false)
    }
}

/// Whether a sync is under way, and whether another was asked for meanwhile.
#[derive(Default)]
pub struct State {
    is_running: bool,
    is_wanted_again: bool,
}

impl State {
    /// Whether the caller may sync now; if one is under way, it will run
    /// again when it is done.
    fn begin(&mut self) -> bool {
        if self.is_running {
            self.is_wanted_again = true;
            return false;
        }
        self.is_running = true;
        true
    }

    /// Whether to sync again.
    fn end(&mut self) -> bool {
        self.is_running = false;
        std::mem::take(&mut self.is_wanted_again)
    }
}

/// Syncs the hearts with ListenBrainz, when there is a token, at start and
/// after each heart. Asked again while one runs, it runs once more when
/// that one is done, since a heart may have been set after it fetched the
/// stars.
pub fn sync(ui: &AppWindow, browser: &Shared) {
    let Some(client) = browse::client(browser) else {
        return;
    };
    if !browse::loves_state(browser).begin() {
        return;
    }
    // Empty when signed in with an API key, whose user the server names.
    let name = ui.get_username().to_string();
    let found = browse::found(browser);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let outcome = run(&client, name, &found).await;
        if let (Some(outcome), Some(ui)) = (outcome, weak.upgrade()) {
            report(&outcome);
            browse::show_synced_hearts(
                &ui,
                &browser,
                &client,
                &outcome.starred,
                &outcome.unstarred,
            );
        }
        let is_wanted_again = browse::loves_state(&browser).end();
        if is_wanted_again && let Some(ui) = weak.upgrade() {
            sync(&ui, &browser);
        }
    })
    .expect("the event loop is running");
}

fn report(outcome: &Outcome) {
    for problem in &outcome.problems {
        eprintln!("ListenBrainz loves: {problem}");
    }
    if outcome.unsendable > 0 {
        eprintln!(
            "ListenBrainz loves: {} starred songs have no recording ID and stay on the server",
            outcome.unsendable
        );
    }
}

/// Syncs, or does nothing without a ListenBrainz token.
async fn run(server: &Client, name: String, found: &Found) -> Option<Outcome> {
    let (listenbrainz, user) = crate::browse::created_for::account().await?;
    let name = match name.is_empty() {
        false => name,
        true => match server.token_info().await {
            Ok(info) => info.username,
            Err(e) => {
                eprintln!("ListenBrainz loves: the server's user is unknown: {e}");
                return None;
            }
        },
    };
    let account = format!("{user}|{}|{name}", server.address());
    let sides = Sides {
        listenbrainz: &listenbrainz,
        user: &user,
        server,
        found,
    };
    let outcome = reconcile(&sides, prefs::kept_loves(&account), prefs::now()).await;
    prefs::save_kept_loves(&account, &outcome.kept);
    Some(outcome)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    const NOW: u64 = 2_000_000_000;

    fn set(recordings: &[&str]) -> BTreeSet<String> {
        recordings.iter().map(|r| r.to_string()).collect()
    }

    #[test]
    fn a_sync_asked_for_while_one_runs_runs_once_more_after_it() {
        let mut state = State::default();
        assert!(state.begin());
        assert!(!state.begin());
        assert!(!state.begin());
        assert!(state.end());
        assert!(state.begin());
        assert!(!state.end());
    }

    #[test]
    fn each_recording_is_sent_to_the_side_that_lacks_it() {
        let planned = plan(
            &set(&["both", "new-love", "cleared-star", "unloved"]),
            &set(&["both", "new-star", "cleared-star2", "unloved"]),
            &set(&["cleared-star", "unloved", "gone-everywhere"]),
        );
        assert_eq!(planned.agreed, set(&["both", "unloved"]));
        assert_eq!(planned.star, ["new-love"]);
        assert_eq!(planned.clear, ["cleared-star"]);
        assert_eq!(planned.love, ["cleared-star2", "new-star"]);
        assert!(planned.unstar.is_empty());
    }

    #[test]
    fn a_love_removed_on_listenbrainz_unstars_and_a_star_removed_on_the_server_clears() {
        let planned = plan(&set(&["a"]), &set(&["b"]), &set(&["a", "b"]));
        assert_eq!(planned.clear, ["a"]);
        assert_eq!(planned.unstar, ["b"]);
        assert!(planned.star.is_empty() && planned.love.is_empty());
    }

    #[test]
    fn without_a_kept_copy_the_first_sync_removes_nothing() {
        let planned = plan(&set(&["a", "b"]), &set(&["b", "c"]), &set(&[]));
        assert_eq!(planned.star, ["a"]);
        assert_eq!(planned.love, ["c"]);
        assert!(planned.clear.is_empty() && planned.unstar.is_empty());
        assert_eq!(planned.agreed, set(&["b"]));
    }

    fn song(id: &str, title: &str, artist: &str, recording: Option<&str>) -> Child {
        Child {
            id: id.into(),
            title: title.into(),
            artist: Some(artist.into()),
            music_brainz_id: recording.map(str::to_owned),
            ..Child::default()
        }
    }

    #[test]
    fn starred_songs_are_told_apart_by_recording() {
        let kept = Kept {
            songs: BTreeMap::from([("r9".to_owned(), "found".to_owned())]),
            ..Kept::default()
        };
        let songs = [
            song("own", "A", "X", Some("R1")),
            song("twin", "A", "X", Some("r1")),
            song("found", "B", "X", Some("other")),
            song("none", "C", "X", None),
            song("none-too", "D", "X", None),
        ];
        let (by_recording, unsendable) = starred_by_recording(&songs, &kept);
        assert_eq!(by_recording["r1"], ["own", "twin"]);
        assert_eq!(by_recording["r9"], ["found"]);
        assert!(!by_recording.contains_key("other"));
        assert_eq!(unsendable, 2);
    }

    #[test]
    fn a_love_is_looked_up_by_the_names_listenbrainz_gave() {
        let love = |named| lb::Feedback {
            user_id: "joe".into(),
            recording_mbid: Some("r1".into()),
            recording_msid: None,
            score: lb::Score::Love,
            created: None,
            track_metadata: named,
        };
        assert_eq!(wanted(&love(None), "r1"), None);
        let named = lb::FeedbackTrack {
            artist_name: "Nils Frahm".into(),
            track_name: "Familiar".into(),
            release_name: Some("All Melody".into()),
        };
        let track = wanted(&love(Some(named)), "r1").unwrap();
        assert_eq!(track.recording_mbid.as_deref(), Some("r1"));
        assert_eq!(
            (track.title.as_str(), track.artist.as_str()),
            ("Familiar", "Nils Frahm")
        );
        assert_eq!(track.artists, ["Nils Frahm"]);
        assert_eq!(track.album.as_deref(), Some("All Melody"));
    }

    #[test]
    fn kept_copies_are_apart_per_account() {
        let kept = Kept {
            loved: set(&["r1"]),
            ..Kept::default()
        };
        prefs::save_kept_loves("joe|https://a/rest/|me", &kept);
        assert_eq!(prefs::kept_loves("joe|https://a/rest/|me"), kept);
        assert_eq!(prefs::kept_loves("joe|https://b/rest/|me"), Kept::default());
        prefs::save_kept_loves("ann|https://a/rest/|me", &Kept::default());
        assert_eq!(prefs::kept_loves("joe|https://a/rest/|me"), kept);
    }

    /// Answers each request with `answer(request line, body)`, a status and
    /// a body, and logs the request line and the body.
    fn serve(
        answer: impl Fn(&str, &str) -> (u16, String) + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let seen = log.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut request = Vec::new();
                let mut chunk = [0; 4096];
                let (line, body) = loop {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    request.extend_from_slice(&chunk[..read]);
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let length = (head.to_lowercase().lines())
                        .find_map(|line| line.strip_prefix("content-length: ")?.parse().ok())
                        .unwrap_or(0usize);
                    if body.len() >= length || read == 0 {
                        let line = head.lines().next().unwrap_or_default();
                        break (line.to_owned(), body.to_owned());
                    }
                };
                seen.lock().unwrap().push(format!("{line} {body}"));
                let (status, json) = answer(&line, &body);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        (address, log)
    }

    /// A love as ListenBrainz answers: with the names only when asked for.
    fn love_json(recording: &str, title: &str, artist: &str, has_names: bool) -> String {
        let names = match has_names {
            true => {
                format!(r#","track_metadata":{{"artist_name":"{artist}","track_name":"{title}"}}"#)
            }
            false => String::new(),
        };
        format!(
            r#"{{"recording_mbid":"{recording}","recording_msid":null,"score":1,"user_id":"joe"{names}}}"#
        )
    }

    fn song_json(song: &Child) -> String {
        let mbid = song.music_brainz_id.as_deref();
        format!(
            r#"{{"id":"{}","isDir":false,"title":"{}","artist":"{}","musicBrainzId":{}}}"#,
            song.id,
            song.title,
            song.artist.as_deref().unwrap(),
            mbid.map_or("null".to_owned(), |m| format!("\"{m}\"")),
        )
    }

    fn subsonic(body: &str) -> (u16, String) {
        let body = if body.is_empty() {
            String::new()
        } else {
            format!(",{body}")
        };
        (
            200,
            format!(r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1"{body}}}}}"#),
        )
    }

    /// A ListenBrainz that loves `loves` (recording, title, artist) and
    /// answers every feedback with `feedback_status`, unless the recording
    /// is in `refused`, which gets a 400.
    fn listenbrainz_with(
        loves: Vec<(&'static str, &'static str, &'static str)>,
        feedback_status: u16,
        refused: &'static [&'static str],
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        serve(move |line, body| {
            if line.starts_with("POST") {
                let status = match refused.iter().any(|r| body.contains(r)) {
                    true => 400,
                    false => feedback_status,
                };
                return (status, r#"{"status":"ok","error":"no"}"#.to_owned());
            }
            let number = |name: &str| -> usize {
                let at = line.find(&format!("{name}=")).unwrap() + name.len() + 1;
                let digits: String = line[at..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits.parse().unwrap()
            };
            let (offset, count) = (number("offset"), number("count"));
            let has_names = line.contains("metadata=true");
            let page: Vec<String> = (loves.iter().skip(offset).take(count))
                .map(|(r, t, a)| love_json(r, t, a, has_names))
                .collect();
            let json = format!(
                r#"{{"count":{},"feedback":[{}],"offset":{offset},"total_count":{}}}"#,
                page.len(),
                page.join(","),
                loves.len()
            );
            (200, json)
        })
    }

    /// A server whose library is `library`, with `starred` of it starred.
    fn server_with(
        library: Vec<Child>,
        starred: Vec<&'static str>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        serve(move |line, _| {
            let songs = |only: &dyn Fn(&Child) -> bool| -> String {
                let songs: Vec<String> =
                    library.iter().filter(|s| only(s)).map(song_json).collect();
                songs.join(",")
            };
            if line.contains("/rest/getStarred2") {
                let starred = songs(&|s| starred.contains(&s.id.as_str()));
                return subsonic(&format!(r#""starred2":{{"song":[{starred}]}}"#));
            }
            if line.contains("/rest/search3") {
                return subsonic(&format!(
                    r#""searchResult3":{{"song":[{}]}}"#,
                    songs(&|_| true)
                ));
            }
            subsonic("")
        })
    }

    fn library() -> Vec<Child> {
        vec![
            song("s1", "Familiar", "Nils Frahm", Some("r1")),
            song("s3", "Ritual", "Jon Hopkins", Some("r3")),
            song("s4", "Halo", "Beyonce", None),
        ]
    }

    struct Setup {
        listenbrainz: Arc<Mutex<Vec<String>>>,
        server: Arc<Mutex<Vec<String>>>,
        outcome: Outcome,
    }

    impl Setup {
        fn calls(log: &Arc<Mutex<Vec<String>>>, what: &str) -> Vec<String> {
            let log = log.lock().unwrap();
            log.iter()
                .filter(|call| call.contains(what))
                .cloned()
                .collect()
        }
        fn stars(&self) -> Vec<String> {
            Self::calls(&self.server, "/rest/star?")
        }
        fn unstars(&self) -> Vec<String> {
            Self::calls(&self.server, "/rest/unstar?")
        }
        fn searches(&self) -> usize {
            Self::calls(&self.server, "/rest/search3").len()
        }
        fn feedback(&self) -> Vec<String> {
            Self::calls(&self.listenbrainz, "recording-feedback")
        }
    }

    fn sync_with(
        loves: Vec<(&'static str, &'static str, &'static str)>,
        feedback_status: u16,
        refused: &'static [&'static str],
        starred: Vec<&'static str>,
        kept: Kept,
    ) -> Setup {
        let (lb_address, listenbrainz) = listenbrainz_with(loves, feedback_status, refused);
        let (address, server) = server_with(library(), starred);
        let lb_client = lb::Client::with_root("t", &format!("{lb_address}/1/")).unwrap();
        let client = Client::new(&address, opensubsonic::Auth::None).unwrap();
        let found = Found::default();
        let sides = Sides {
            listenbrainz: &lb_client,
            user: "joe",
            server: &client,
            found: &found,
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let outcome = runtime.block_on(reconcile(&sides, kept, NOW));
        Setup {
            listenbrainz,
            server,
            outcome,
        }
    }

    const FAMILIAR: (&str, &str, &str) = ("r1", "Familiar", "Nils Frahm");
    const GONE: (&str, &str, &str) = ("r2", "Not Here", "Nobody");

    #[test]
    fn the_first_sync_merges_both_sides_and_removes_nothing() {
        // Loved on ListenBrainz: Familiar (in the library), Not Here (not).
        // Starred on the server: Ritual, and Halo, which has no recording ID.
        let synced = sync_with(
            vec![FAMILIAR, GONE],
            200,
            &[],
            vec!["s3", "s4"],
            Kept::default(),
        );
        assert_eq!(synced.stars().len(), 1);
        assert!(synced.stars()[0].contains("id=s1"));
        assert!(synced.unstars().is_empty());
        let feedback = synced.feedback();
        assert_eq!(feedback.len(), 1);
        assert!(
            feedback[0].contains(r#""recording_mbid":"r3""#)
                && feedback[0].contains(r#""score":1"#)
        );
        let outcome = synced.outcome;
        assert_eq!(outcome.starred, ["s1"]);
        assert_eq!(outcome.unsendable, 1);
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        assert_eq!(outcome.kept.loved, set(&["r1", "r3"]));
        assert_eq!(outcome.kept.unmatched.keys().collect::<Vec<_>>(), ["r2"]);
    }

    #[test]
    fn a_heart_cleared_in_the_app_is_cleared_on_listenbrainz_and_not_loved_again() {
        let kept = Kept {
            loved: set(&["r1"]),
            ..Kept::default()
        };
        let synced = sync_with(vec![FAMILIAR], 200, &[], vec![], kept);
        let feedback = synced.feedback();
        assert_eq!(feedback.len(), 1);
        assert!(feedback[0].contains(r#""score":0"#));
        assert!(synced.stars().is_empty());
        assert_eq!(synced.outcome.kept.loved, set(&[]));
    }

    #[test]
    fn a_love_removed_on_listenbrainz_unstars_the_song() {
        let kept = Kept {
            loved: set(&["r1"]),
            songs: BTreeMap::from([("r1".to_owned(), "s1".to_owned())]),
            ..Kept::default()
        };
        let synced = sync_with(vec![], 200, &[], vec!["s1"], kept);
        assert!(synced.outcome.kept.songs.is_empty());
        assert_eq!(synced.unstars().len(), 1);
        assert!(synced.unstars()[0].contains("id=s1"));
        assert!(synced.feedback().is_empty());
        assert_eq!(synced.outcome.unstarred, ["s1"]);
        assert_eq!(synced.outcome.kept.loved, set(&[]));
    }

    #[test]
    fn a_heart_that_could_not_be_sent_is_kept_to_send_next_time() {
        // Starred in the app, not yet loved on ListenBrainz.
        let synced = sync_with(vec![], 503, &[], vec!["s1"], Kept::default());
        assert_eq!(synced.outcome.kept.loved, set(&[]));
        assert_eq!(synced.outcome.problems.len(), 1);
        // Cleared in the app, still loved on ListenBrainz.
        let kept = Kept {
            loved: set(&["r1"]),
            ..Kept::default()
        };
        let synced = sync_with(vec![FAMILIAR], 503, &[], vec![], kept.clone());
        assert_eq!(synced.outcome.kept.loved, kept.loved);
        assert_eq!(synced.outcome.problems.len(), 1);
    }

    #[test]
    fn a_failure_stops_the_sending_but_a_refused_recording_does_not() {
        let hearts = vec!["s1", "s3"];
        let down = sync_with(vec![], 503, &[], hearts.clone(), Kept::default());
        assert_eq!(down.feedback().len(), 1);
        let refused = sync_with(vec![], 200, &["r1"], hearts, Kept::default());
        assert_eq!(refused.feedback().len(), 2);
        assert_eq!(refused.outcome.kept.loved, set(&["r3"]));
    }

    #[test]
    fn a_song_found_by_its_names_stays_agreed_on_the_next_sync() {
        // The library's Halo has no recording ID; ListenBrainz loves r5.
        let halo = ("r5", "Halo", "Beyonce");
        let first = sync_with(vec![halo], 200, &[], vec![], Kept::default());
        assert_eq!(first.outcome.starred, ["s4"]);
        let kept = first.outcome.kept;
        assert_eq!(kept.songs["r5"], "s4");
        let next = sync_with(vec![halo], 200, &[], vec!["s4"], kept.clone());
        assert!(next.stars().is_empty() && next.unstars().is_empty() && next.feedback().is_empty());
        assert_eq!(next.outcome.kept.loved, set(&["r5"]));
        assert_eq!(next.outcome.kept.songs["r5"], "s4");
        assert_eq!(next.outcome.unsendable, 0);
    }

    #[test]
    fn a_love_the_library_lacks_is_searched_again_only_after_a_week() {
        let lately = Kept {
            unmatched: BTreeMap::from([("r2".to_owned(), NOW - UNMATCHED_RECHECK_SECS + 1)]),
            ..Kept::default()
        };
        let skipped = sync_with(vec![GONE], 200, &[], vec![], lately.clone());
        assert_eq!(skipped.searches(), 0);
        assert_eq!(skipped.outcome.kept.unmatched, lately.unmatched);
        let old = Kept {
            unmatched: BTreeMap::from([("r2".to_owned(), NOW - UNMATCHED_RECHECK_SECS)]),
            ..Kept::default()
        };
        let searched = sync_with(vec![GONE], 200, &[], vec![], old);
        assert!(searched.searches() > 0);
        assert_eq!(searched.outcome.kept.unmatched["r2"], NOW);
    }

    #[test]
    fn a_failed_fetch_changes_nothing() {
        let kept = Kept {
            loved: set(&["r1"]),
            ..Kept::default()
        };
        let (address, _) = serve(|_, _| (500, "{}".to_owned()));
        let lb_client = lb::Client::with_root("t", &format!("{address}/1/")).unwrap();
        let (address, _) = server_with(library(), vec![]);
        let client = Client::new(&address, opensubsonic::Auth::None).unwrap();
        let found = Found::default();
        let sides = Sides {
            listenbrainz: &lb_client,
            user: "joe",
            server: &client,
            found: &found,
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let outcome = runtime.block_on(reconcile(&sides, kept.clone(), NOW));
        assert_eq!(outcome.kept, kept);
        assert_eq!(outcome.problems.len(), 1);
    }

    #[test]
    fn every_page_of_loves_is_read() {
        const NAMES: [&str; 3] = ["r1", "r3", "r2"];
        let loves: Vec<(&'static str, &'static str, &'static str)> = (0..205)
            .map(|n| {
                (
                    &*Box::leak(format!("m{n}").into_boxed_str()),
                    NAMES[n % 3],
                    "X",
                )
            })
            .collect();
        let (lb_address, log) = listenbrainz_with(loves, 200, &[]);
        let lb_client = lb::Client::with_root("t", &format!("{lb_address}/1/")).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let all = runtime.block_on(lb_client.loved_recordings("joe")).unwrap();
        assert_eq!(all.len(), 205);
        assert_eq!(log.lock().unwrap().len(), 3);
    }
}
