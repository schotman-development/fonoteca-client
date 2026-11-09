//! The answers to the calls pages read to draw themselves, kept so that a
//! page seen before shows at once. An answer is fresh for an hour and handed
//! out without asking the server. After that it is aged: handed out only to
//! a page that shows it at once and then asks again. A call that changes the
//! server forgets the answers it makes out of date, or ages those it changes
//! only the counts in. Past [`MOST_KEPT`], those shown longest ago are
//! forgotten.
//!
//! Time comes from the clock the app hands in, and saving is up to the app:
//! it gets the answers as JSON to save and hands them back at the next
//! start. Reading them back and looking through them for what a heart names
//! take tens of milliseconds, so the app and the client do both off the UI
//! thread, and nothing serialises or scans while holding the lock.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// How long, in seconds, an answer is handed out without asking the server
/// again: an hour, the user's pick.
pub const FRESH_FOR: u64 = 60 * 60;
/// How much the answers may come to, in bytes of their calls and text,
/// before those shown longest ago are forgotten: 64 MB, the user's pick. A
/// library of 640 albums comes to 27 MB once every page has been seen.
const MOST_KEPT: usize = 64 * 1024 * 1024;
/// The shape of saved answers; those saved in another are ignored.
const FORMAT: u32 = 1;
/// The calls whose answers are kept: what the pages read to draw themselves.
/// Searches aren't, not even the empty one the tracks page lists every song
/// with: on a library of 9,000 songs that is 21 MB. Neither are songs or
/// covers. The scan status is, so that Home shows the last known track count
/// the moment the app opens; the Settings page asks the server every time,
/// with [`crate::Client::asking_the_server`], as a kept answer says nothing
/// of what it is doing now.
const KEPT: [&str; 21] = [
    "getAlbumList2",
    "getAlbum",
    "getAlbumInfo2",
    "getArtists",
    "getArtist",
    "getArtistInfo2",
    "getTopSongs",
    "getSong",
    "getPlaylists",
    "getPlaylist",
    "getPlaylistFolders",
    "getGenres",
    "getIndexes",
    "getMusicDirectory",
    "getTags",
    "getTag",
    "getComposers",
    "getCompositions",
    "getInternetRadioStations",
    "getRadioCatalogue",
    "getScanStatus",
];
/// The kept answers, and the clock that says how old they are.
pub struct Answers {
    kept: Mutex<Kept>,
    /// Set once those saved are read back, which every call waits for.
    loaded: OnceLock<()>,
    /// Seconds since 1970.
    clock: fn() -> u64,
}

#[derive(Default)]
struct Kept {
    answers: HashMap<String, Answer>,
    /// Counts the calls that changed the server, so that an answer asked for
    /// before one of them, which may be from before it, isn't kept.
    writes: u64,
    /// Counts the changes to the answers, and says which one was saved
    /// last, so that a save that failed is made again.
    changes: u64,
    saved: u64,
    /// Counts the answers handed out and kept, to tell which was shown
    /// longest ago.
    shown: u64,
    /// How many looks through the answers for what a call named are under
    /// way. Meanwhile every answer is handed out as aged, as it may be out
    /// of date.
    searches: usize,
    /// Whether the account signed out, after which nothing is saved again.
    forgotten: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Answer {
    /// When it came, by the clock.
    at: u64,
    /// When it was last handed out or kept, by [`Kept::shown`].
    shown: u64,
    /// The `subsonic-response` object, as JSON, shared with the saves and
    /// searches that read it without the lock.
    #[serde(with = "shared_text")]
    text: Arc<str>,
}

/// The answers as they are saved.
#[derive(Serialize, Deserialize)]
struct Saved {
    format: u32,
    answers: HashMap<String, Answer>,
}

/// The answers as JSON to save, for [`Answers::saved`] once they are.
pub struct Unsaved {
    pub json: String,
    change: u64,
}

/// A kept answer, and whether it is aged.
pub(crate) struct Found {
    pub answer: Map<String, Value>,
    pub aged: bool,
}

/// A look through the answers, as they were when a call changed the
/// server, for those naming one of its values.
pub(crate) struct Search {
    named: HashSet<String>,
    answers: Vec<(String, Arc<str>)>,
}

/// The answers a [`Search`] found.
pub(crate) struct Outdated(Vec<(String, Arc<str>)>);

impl Answers {
    pub fn new(clock: fn() -> u64) -> Self {
        let answers = Self::loading(clock);
        answers.loaded.set(()).expect("just made");
        answers
    }

    /// None yet, and every call waits until [`Answers::load`] has them.
    pub fn loading(clock: fn() -> u64) -> Self {
        Self {
            kept: Mutex::default(),
            loaded: OnceLock::new(),
            clock,
        }
    }

    /// Takes the answers in `saved`, as [`Answers::unsaved`] gave them, and
    /// lets the calls waiting for them go on. A save that can't be read, or
    /// of another format, holds none.
    pub fn load(&self, saved: Option<&str>) {
        let saved = saved
            .and_then(|saved| serde_json::from_str::<Saved>(saved).ok())
            .filter(|saved| saved.format == FORMAT);
        if let Some(saved) = saved {
            let mut kept = self.lock();
            if !kept.forgotten {
                kept.shown = saved
                    .answers
                    .values()
                    .map(|answer| answer.shown)
                    .max()
                    .unwrap_or(0);
                kept.answers = saved.answers;
                kept.trim();
            }
        }
        let _ = self.loaded.set(());
    }

    /// Whether those saved are in, and calls need not wait.
    pub(crate) fn is_loaded(&self) -> bool {
        self.loaded.get().is_some()
    }

    /// Blocks until those saved are in.
    pub(crate) fn wait_until_loaded(&self) {
        self.loaded.wait();
    }

    /// The answers as JSON to save, when they changed since last saved.
    pub fn unsaved(&self) -> Option<Unsaved> {
        let (answers, change) = {
            let kept = self.lock();
            if kept.changes == kept.saved || kept.forgotten {
                return None;
            }
            (kept.answers.clone(), kept.changes)
        };
        let saved = Saved {
            format: FORMAT,
            answers,
        };
        let json = serde_json::to_string(&saved).expect("answers are JSON");
        Some(Unsaved { json, change })
    }

    /// Notes that `unsaved` was saved, so that it isn't again unless the
    /// answers changed since.
    pub fn saved(&self, unsaved: &Unsaved) {
        let mut kept = self.lock();
        kept.saved = kept.saved.max(unsaved.change);
    }

    /// Ages every answer, as if it came an hour ago, so that each is asked
    /// for again the next time a page reads it, which still shows it at once.
    pub fn age_all(&self) {
        let an_hour_ago = (self.clock)().saturating_sub(FRESH_FOR);
        let mut kept = self.lock();
        for answer in kept.answers.values_mut() {
            answer.at = an_hour_ago;
        }
        kept.changes += 1;
    }

    /// Forgets every answer, on sign out, and has none saved from now on.
    pub fn forget(&self) {
        let mut kept = self.lock();
        kept.answers.clear();
        kept.forgotten = true;
    }

    /// The answer kept for `read`, if one is and it can be read.
    pub(crate) fn find(&self, read: &str) -> Option<Found> {
        let now = (self.clock)();
        let (text, aged) = {
            let mut kept = self.lock();
            kept.shown += 1;
            let (shown, searching) = (kept.shown, kept.searches > 0);
            // Handing one out isn't a change worth saving for: the order
            // is saved with the next.
            let answer = kept.answers.get_mut(read)?;
            answer.shown = shown;
            (answer.text.clone(), searching || !is_fresh(answer.at, now))
        };
        Some(Found {
            answer: serde_json::from_str(&text).ok()?,
            aged,
        })
    }

    /// How many calls changed the server so far, for [`Answers::keep`].
    pub(crate) fn writes(&self) -> u64 {
        self.lock().writes
    }

    /// Keeps `answer` for `read`, unless a call changed the server since
    /// there had been `writes`. Whether it says something else than the one
    /// kept before.
    pub(crate) fn keep(&self, read: String, answer: &Map<String, Value>, writes: u64) -> bool {
        let text: Arc<str> = serde_json::to_string(answer)
            .expect("answers are JSON")
            .into();
        let at = (self.clock)();
        let mut kept = self.lock();
        let is_news = kept.answers.get(&read).is_none_or(|old| old.text != text);
        if kept.writes == writes {
            kept.shown += 1;
            let shown = kept.shown;
            kept.answers.insert(read, Answer { at, shown, text });
            kept.changes += 1;
            kept.trim();
        }
        is_news
    }

    /// Forgets the answers that `call`, if it changes the server, makes out
    /// of date by their call, and gives the look through the rest for what
    /// it named, if it needs one, for [`Answers::forget_outdated`].
    pub(crate) fn called(&self, call: &str, params: &[(&str, String)]) -> Option<Search> {
        let change = Change::of(call, params)?;
        let mut kept = self.lock();
        kept.writes += 1;
        let before = kept.answers.len();
        kept.answers.retain(|read, _| !change.outdates_call(read));
        if kept.answers.len() != before {
            kept.changes += 1;
        }
        if let Change::Ages(reads) = change {
            let an_hour_ago = (self.clock)().saturating_sub(FRESH_FOR);
            let aged = (kept.answers.iter_mut())
                .filter(|(read, _)| reads.iter().any(|reads| read.starts_with(reads)));
            for (_, answer) in aged {
                answer.at = an_hour_ago;
            }
            kept.changes += 1;
        }
        if !change.names() {
            return None;
        }
        kept.searches += 1;
        // Spelled as the answers' text has them, and looked up all at once:
        // a sync of hearts stars fifty songs in one call.
        let named = (params.iter())
            .map(|(_, value)| serde_json::to_string(value).expect("strings are JSON"))
            .map(|quoted| quoted[1..quoted.len() - 1].to_owned())
            .collect();
        let answers = (kept.answers.iter())
            .map(|(read, answer)| (read.clone(), answer.text.clone()))
            .collect();
        Some(Search { named, answers })
    }

    /// Forgets what a search found, of the answers still as it saw them, and
    /// hands them out as before.
    pub(crate) fn forget_outdated(&self, outdated: Outdated) {
        let mut kept = self.lock();
        kept.searches -= 1;
        for (read, text) in outdated.0 {
            let same = (kept.answers.get(&read)).is_some_and(|now| Arc::ptr_eq(&now.text, &text));
            if same {
                kept.answers.remove(&read);
                kept.changes += 1;
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, Kept> {
        self.kept
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Kept {
    /// Forgets the answers shown longest ago while they come to more than
    /// [`MOST_KEPT`].
    fn trim(&mut self) {
        let size = |read: &str, answer: &Answer| read.len() + answer.text.len();
        let mut total: usize = (self.answers.iter())
            .map(|(read, answer)| size(read, answer))
            .sum();
        if total <= MOST_KEPT {
            return;
        }
        let mut by_showing: Vec<(u64, String)> = (self.answers.iter())
            .map(|(read, answer)| (answer.shown, read.clone()))
            .collect();
        by_showing.sort_unstable();
        for (_, read) in by_showing {
            if total <= MOST_KEPT {
                break;
            }
            let answer = self.answers.remove(&read).expect("listed just now");
            total -= size(&read, &answer);
        }
    }
}

impl Search {
    /// The answers that name one of the values.
    pub(crate) fn run(self) -> Outdated {
        let named = self.named;
        let answers = self.answers.into_iter();
        Outdated(
            answers
                .filter(|(_, text)| names_any(text, &named))
                .collect(),
        )
    }
}

/// Saves and reads back an answer's shared text as a plain string.
mod shared_text {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(text: &Arc<str>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Arc<str>, D::Error> {
        String::deserialize(deserializer).map(Arc::from)
    }
}

/// The key a call's answer is kept under, when it is one of those kept: its
/// name and its parameters.
pub(crate) fn read_key(call: &str, params: &[(&str, String)]) -> Option<String> {
    if !KEPT.contains(&call) {
        return None;
    }
    let params = serde_json::to_string(params).expect("parameters are JSON");
    Some(format!("{call}{params}"))
}

/// Whether the answer says something that stops being true within the hour
/// it would be kept for: a scan under way, whose count is a partial one and
/// which nothing would age when it ends.
pub(crate) fn is_unsettled(call: &str, answer: &Map<String, Value>) -> bool {
    call == "getScanStatus"
        && answer
            .get("scanStatus")
            .and_then(|status| status.get("scanning"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Whether an answer that came at `at` is fresh `now`. One from later than
/// now, as when the device's clock was set back, isn't.
fn is_fresh(at: u64, now: u64) -> bool {
    at <= now && now - at < FRESH_FOR
}

/// What a call that changes the server makes out of date.
enum Change {
    /// Every answer: a scan may change anything.
    Everything,
    /// The answers that name one of the call's ids, which carry the hearts
    /// and ratings it changes.
    Naming,
    /// The answers to the calls whose names start so.
    Reads(&'static str),
    /// Both of these.
    ReadsAndNaming(&'static str),
    /// None, but the answers to the calls whose names start with one of
    /// these are aged: shown at once and asked for again.
    Ages(&'static [&'static str]),
}

impl Change {
    fn of(call: &str, params: &[(&str, String)]) -> Option<Self> {
        match call {
            "startScan" => Some(Self::Everything),
            "star" | "unstar" | "setRating" => Some(Self::Naming),
            "createPlaylist"
            | "updatePlaylist"
            | "deletePlaylist"
            | "movePlaylist"
            | "createPlaylistFolder"
            | "updatePlaylistFolder"
            | "deletePlaylistFolder" => Some(Self::Reads("getPlaylist")),
            "createTag" | "updateTag" | "deleteTag" => Some(Self::ReadsAndNaming("getTag")),
            "createInternetRadioStation"
            | "updateInternetRadioStation"
            | "deleteInternetRadioStation" => Some(Self::Reads("getInternetRadioStations")),
            // Saying what plays now counts no play.
            "scrobble" if is_playing_now(params) => None,
            // A play raises only the counts in the album lists and in
            // Fonoteca's composers and compositions, so they still show at
            // once, and are asked for again.
            "scrobble" => Some(Self::Ages(&[
                "getAlbumList2",
                "getComposers",
                "getCompositions",
            ])),
            _ => None,
        }
    }

    /// Whether the answer to `read` is out of date by its call alone.
    fn outdates_call(&self, read: &str) -> bool {
        match self {
            Self::Everything => true,
            Self::Naming | Self::Ages(_) => false,
            Self::Reads(reads) | Self::ReadsAndNaming(reads) => read.starts_with(reads),
        }
    }

    /// Whether the answers that name one of the call's values are out of
    /// date too.
    fn names(&self) -> bool {
        matches!(self, Self::Naming | Self::ReadsAndNaming(_))
    }
}

/// Whether a scrobble's `params` only say what plays now.
fn is_playing_now(params: &[(&str, String)]) -> bool {
    (params.iter()).any(|(name, value)| *name == "submission" && value == "false")
}

/// Whether one of the strings in the JSON `text`, its keys too, is one of
/// `named`, as JSON escapes it. One pass, however many are named.
fn names_any(text: &str, named: &HashSet<String>) -> bool {
    let mut rest = text;
    while let Some(quote) = rest.find('"') {
        let string = &rest[quote + 1..];
        let end = string_end(string);
        if named.contains(&string[..end]) {
            return true;
        }
        rest = string.get(end + 1..).unwrap_or_default();
    }
    false
}

/// Where the JSON string `text` begins inside of ends: at its closing quote,
/// or at the end of `text`. JSON escapes only ASCII characters, so skipping
/// an escape keeps to the characters' bounds.
fn string_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'"' => return at,
            _ => at += 1,
        }
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    use super::*;

    fn answer(body: &str) -> Map<String, Value> {
        serde_json::from_str(body).unwrap()
    }

    fn params(pairs: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
        pairs
            .iter()
            .map(|(name, value)| (*name, value.to_string()))
            .collect()
    }

    fn key(call: &str, pairs: &[(&'static str, &str)]) -> String {
        read_key(call, &params(pairs)).unwrap()
    }

    /// When the tests' answers come, by a clock that stands still.
    const NOW: u64 = 1_000_000;

    /// Makes `call`, looking through the answers for what it names at once.
    fn call(answers: &Answers, call: &str, params: &[(&str, String)]) {
        if let Some(search) = answers.called(call, params) {
            answers.forget_outdated(search.run());
        }
    }

    fn from_saved(saved: &str) -> Answers {
        let answers = Answers::loading(|| NOW);
        answers.load(Some(saved));
        answers
    }

    /// Notes the answers as saved.
    fn save(answers: &Answers) -> String {
        let unsaved = answers.unsaved().expect("changed");
        answers.saved(&unsaved);
        unsaved.json
    }

    fn kept(reads: &[(&str, &str)]) -> Answers {
        let answers = Answers::new(|| NOW);
        for (read, body) in reads {
            answers.keep(read.to_string(), &answer(body), answers.writes());
        }
        answers
    }

    #[test]
    fn an_answer_is_fresh_for_an_hour() {
        assert!(is_fresh(NOW, NOW));
        assert!(is_fresh(NOW, NOW + FRESH_FOR - 1));
        assert!(!is_fresh(NOW, NOW + FRESH_FOR));
        assert!(!is_fresh(NOW + 1, NOW), "from later than now");
    }

    #[test]
    fn found_answers_say_whether_they_are_aged() {
        static CLOCK: AtomicU64 = AtomicU64::new(NOW);
        let answers = Answers::new(|| CLOCK.load(Relaxed));
        let read = key("getAlbum", &[("id", "a1")]);
        assert!(answers.find(&read).is_none());
        answers.keep(read.clone(), &answer(r#"{"album":{"id":"a1"}}"#), 0);
        let found = answers.find(&read).unwrap();
        assert!(!found.aged);
        assert_eq!(found.answer, answer(r#"{"album":{"id":"a1"}}"#));
        CLOCK.store(NOW + FRESH_FOR, Relaxed);
        assert!(answers.find(&read).unwrap().aged);
    }

    #[test]
    fn page_reads_are_kept_and_searches_are_not() {
        assert_eq!(key("getAlbum", &[("id", "a1")]), r#"getAlbum[["id","a1"]]"#);
        assert!(read_key("getScanStatus", &[]).is_some());
        assert_ne!(
            key("getAlbum", &[("id", "a1")]),
            key("getAlbum", &[("id", "a2")])
        );
        // The stars are asked for as they are now, to sync hearts by.
        let calls = ["search3", "getCoverArt", "getUser", "star", "getStarred2"];
        for call in calls {
            assert!(read_key(call, &[]).is_none(), "{call}");
        }
    }

    #[test]
    fn keeping_says_whether_the_answer_is_news() {
        let answers = kept(&[]);
        let read = key("getPlaylists", &[]);
        assert!(answers.keep(read.clone(), &answer(r#"{"n":1}"#), 0));
        assert!(!answers.keep(read.clone(), &answer(r#"{"n":1}"#), 0));
        assert!(answers.keep(read.clone(), &answer(r#"{"n":2}"#), 0));
        assert_eq!(answers.find(&read).unwrap().answer, answer(r#"{"n":2}"#));
    }

    #[test]
    fn an_answer_asked_for_before_a_change_is_not_kept() {
        let answers = kept(&[]);
        let read = key("getPlaylists", &[]);
        let writes = answers.writes();
        call(&answers, "createPlaylist", &params(&[("name", "Mix")]));
        assert!(answers.keep(read.clone(), &answer(r#"{"n":1}"#), writes));
        assert!(answers.find(&read).is_none());
        answers.keep(read.clone(), &answer(r#"{"n":1}"#), answers.writes());
        assert!(answers.find(&read).is_some());
    }

    #[test]
    fn reads_change_nothing() {
        let answers = kept(&[]);
        call(&answers, "getScanStatus", &[]);
        call(&answers, "savePlayQueue", &params(&[("id", "s1")]));
        assert_eq!(answers.writes(), 0);
    }

    #[test]
    fn a_heart_outdates_the_answers_naming_it() {
        let album = key("getAlbum", &[("id", "a1")]);
        let other = key("getAlbum", &[("id", "a2")]);
        let answers = kept(&[
            (&album, r#"{"album":{"id":"a1","song":[{"id":"s1"}]}}"#),
            (&other, r#"{"album":{"id":"a2","song":[{"id":"s10"}]}}"#),
        ]);
        save(&answers);
        call(&answers, "star", &params(&[("id", "s1")]));
        assert!(answers.find(&album).is_none());
        assert!(answers.find(&other).is_some(), "s10 isn't s1");
        assert!(!save(&answers).contains("a1"), "saved without it");
        call(&answers, "unstar", &params(&[("id", "s10")]));
        assert!(answers.find(&other).is_none());
    }

    #[test]
    fn a_rating_outdates_the_answers_naming_the_song() {
        let album = key("getAlbum", &[("id", "a1")]);
        let other = key("getAlbum", &[("id", "a2")]);
        let answers = kept(&[
            (
                &album,
                r#"{"album":{"id":"a1","song":[{"id":"s1","userRating":2}]}}"#,
            ),
            (&other, r#"{"album":{"id":"a2","song":[{"id":"s2"}]}}"#),
        ]);
        call(
            &answers,
            "setRating",
            &params(&[("id", "s1"), ("rating", "4")]),
        );
        assert!(answers.find(&album).is_none());
        assert!(answers.find(&other).is_some());
    }

    #[test]
    fn a_batch_of_synced_hearts_outdates_what_names_any_of_them() {
        let album = key("getAlbum", &[("id", "a1")]);
        let playlist = key("getPlaylist", &[("id", "p1")]);
        let notes = key("getAlbumInfo2", &[("id", "a2")]);
        let answers = kept(&[
            (&album, r#"{"album":{"id":"a1","song":[{"id":"s49"}]}}"#),
            (
                &playlist,
                r#"{"playlist":{"id":"p1","entry":[{"id":"s70","title":"Say \"s8\""}]}}"#,
            ),
            (&notes, r#"{"albumInfo":{"notes":"s1 and s8"}}"#),
        ]);
        let stars: Vec<_> = (1..=50).map(|n| ("id", format!("s{n}"))).collect();
        call(&answers, "star", &stars);
        assert!(answers.find(&album).is_none());
        assert!(
            answers.find(&playlist).is_some(),
            "a title's s8 isn't an id"
        );
        assert!(answers.find(&notes).is_some(), "nor are notes");
    }

    #[test]
    fn a_playlist_change_outdates_the_playlists() {
        let list = key("getPlaylists", &[]);
        let playlist = key("getPlaylist", &[("id", "p1")]);
        let folders = key("getPlaylistFolders", &[]);
        let album = key("getAlbum", &[("id", "a1")]);
        let changes = [
            "createPlaylist",
            "updatePlaylist",
            "deletePlaylist",
            "movePlaylist",
            "createPlaylistFolder",
            "updatePlaylistFolder",
            "deletePlaylistFolder",
        ];
        for change in changes {
            let answers = kept(&[
                (&list, "{}"),
                (&playlist, "{}"),
                (&folders, "{}"),
                (&album, r#"{"album":{"id":"a1"}}"#),
            ]);
            call(&answers, change, &params(&[("playlistId", "p1")]));
            assert!(answers.find(&list).is_none(), "{change}");
            assert!(answers.find(&playlist).is_none(), "{change}");
            assert!(answers.find(&folders).is_none(), "{change}");
            assert!(answers.find(&album).is_some(), "{change}");
        }
    }

    #[test]
    fn a_tag_change_outdates_the_tags_and_what_names_it() {
        let tags = key("getTags", &[]);
        let song = key("getSong", &[("id", "s1")]);
        let album = key("getAlbum", &[("id", "a1")]);
        for change in ["createTag", "updateTag", "deleteTag"] {
            let answers = kept(&[
                (&tags, "{}"),
                (&song, r#"{"song":{"id":"s1","tagIds":["t1"]}}"#),
                (&album, r#"{"album":{"id":"a1"}}"#),
            ]);
            call(&answers, change, &params(&[("tagId", "t1")]));
            assert!(answers.find(&tags).is_none(), "{change}");
            assert!(answers.find(&song).is_none(), "{change}");
            assert!(answers.find(&album).is_some(), "{change}");
        }
    }

    #[test]
    fn a_saved_station_outdates_the_stations() {
        let stations = key("getInternetRadioStations", &[]);
        let catalogue = key("getRadioCatalogue", &[]);
        let changes = [
            "createInternetRadioStation",
            "updateInternetRadioStation",
            "deleteInternetRadioStation",
        ];
        for change in changes {
            let answers = kept(&[(&stations, "{}"), (&catalogue, "{}")]);
            call(&answers, change, &params(&[("streamUrl", "https://x")]));
            assert!(answers.find(&stations).is_none(), "{change}");
            assert!(answers.find(&catalogue).is_some(), "{change}");
        }
    }

    #[test]
    fn a_play_ages_the_album_lists_and_keeps_them() {
        let library = key("getAlbumList2", &[("type", "alphabeticalByName")]);
        let album = key("getAlbum", &[("id", "a1")]);
        let answers = kept(&[(&library, r#"{"albumList2":{}}"#), (&album, "{}")]);
        save(&answers);
        call(&answers, "scrobble", &params(&[("id", "s1")]));
        assert!(answers.find(&library).unwrap().aged);
        assert!(
            !answers.find(&album).unwrap().aged,
            "its page shows no plays"
        );
        assert!(answers.unsaved().is_some(), "saved aged");
    }

    #[test]
    fn a_play_ages_the_composers_and_compositions_too() {
        let composers = key("getComposers", &[]);
        let pieces = key("getCompositions", &[]);
        let answers = kept(&[
            (&composers, r#"{"composers":{}}"#),
            (&pieces, r#"{"compositions":{}}"#),
        ]);
        call(&answers, "scrobble", &params(&[("id", "s1")]));
        assert!(answers.find(&composers).unwrap().aged);
        assert!(answers.find(&pieces).unwrap().aged);
    }

    #[test]
    fn saying_what_plays_now_keeps_the_album_lists_fresh() {
        let library = key("getAlbumList2", &[("type", "alphabeticalByName")]);
        let answers = kept(&[(&library, r#"{"albumList2":{}}"#)]);
        let writes = answers.writes();
        let playing_now = params(&[("id", "s1"), ("submission", "false")]);
        call(&answers, "scrobble", &playing_now);
        assert!(!answers.find(&library).unwrap().aged);
        assert_eq!(answers.writes(), writes, "the reads under way are kept");
    }

    #[test]
    fn a_scan_outdates_everything() {
        let album = key("getAlbum", &[("id", "a1")]);
        let answers = kept(&[(&album, "{}")]);
        call(&answers, "startScan", &[]);
        assert!(answers.find(&album).is_none());
    }

    #[test]
    fn saved_answers_come_back_and_bad_saves_hold_none() {
        let read = key("getGenres", &[]);
        let answers = kept(&[(&read, r#"{"genres":{}}"#)]);
        let saved = save(&answers);
        assert!(answers.unsaved().is_none(), "nothing changed since");
        let again = from_saved(&saved);
        assert_eq!(
            again.find(&read).unwrap().answer,
            answer(r#"{"genres":{}}"#)
        );
        let other_format = saved.replace(r#""format":1"#, r#""format":0"#);
        for bad in ["", "not json", "{}", other_format.as_str()] {
            assert!(from_saved(bad).find(&read).is_none(), "{bad}");
        }
        let unreadable = saved.replace(r#"{\"genres\":{}}"#, "garbled");
        assert!(from_saved(&unreadable).find(&read).is_none());
    }

    #[test]
    fn forgotten_answers_are_gone_and_never_saved() {
        let read = key("getGenres", &[]);
        let answers = kept(&[(&read, "{}")]);
        answers.forget();
        assert!(answers.find(&read).is_none());
        answers.keep(read, &answer("{}"), answers.writes());
        assert!(answers.unsaved().is_none());
    }

    #[test]
    fn aging_all_keeps_them_but_aged() {
        let read = key("getGenres", &[]);
        let answers = kept(&[(&read, "{}")]);
        save(&answers);
        answers.age_all();
        assert!(answers.find(&read).unwrap().aged);
        assert!(answers.unsaved().is_some());
    }

    #[test]
    fn a_save_that_failed_is_made_again() {
        let answers = kept(&[(&key("getGenres", &[]), "{}")]);
        let failed = answers.unsaved().unwrap();
        let again = answers.unsaved().expect("not saved yet");
        assert_eq!(again.json, failed.json);
        answers.keep(key("getIndexes", &[]), &answer("{}"), answers.writes());
        answers.saved(&again);
        assert!(answers.unsaved().is_some(), "a change came since");
        save(&answers);
        assert!(answers.unsaved().is_none());
    }

    #[test]
    fn past_the_most_kept_those_shown_longest_ago_go_first() {
        // Four of these come to more than the most kept, three don't.
        let big = |n: usize| {
            format!(
                r#"{{"album":{{"id":"a{n}","notes":"{}"}}}}"#,
                "x".repeat(MOST_KEPT / 4 + 1)
            )
        };
        let read = |n: usize| key("getAlbum", &[("id", &format!("a{n}"))]);
        let answers = kept(&[]);
        let keep = |n: usize| answers.keep(read(n), &answer(&big(n)), answers.writes());
        let has = |n: usize| answers.find(&read(n)).is_some();
        keep(1);
        keep(2);
        keep(3);
        assert!(has(1), "shown again, after 2 and 3");
        keep(4);
        assert!(
            !answers.lock().answers.contains_key(&read(2)),
            "shown longest ago"
        );
        assert!(has(3) && has(1) && has(4));
        keep(5);
        assert!(!answers.lock().answers.contains_key(&read(3)));
        assert!(has(1) && has(4) && has(5));
        let saved = save(&answers);
        let again = from_saved(&saved);
        again.keep(read(6), &answer(&big(6)), again.writes());
        assert!(
            !again.lock().answers.contains_key(&read(1)),
            "the order is saved too"
        );
    }

    #[test]
    fn while_a_heart_is_looked_for_every_answer_is_aged() {
        let album = key("getAlbum", &[("id", "a1")]);
        let answers = kept(&[(&album, r#"{"album":{"id":"a1","song":[{"id":"s1"}]}}"#)]);
        let search = answers.called("star", &params(&[("id", "s1")])).unwrap();
        assert!(answers.find(&album).unwrap().aged);
        let outdated = search.run();
        // Asked for after the star, it is as the server is now.
        let now = r#"{"album":{"id":"a1","song":[{"id":"s1","starred":"x"}]}}"#;
        answers.keep(album.clone(), &answer(now), answers.writes());
        answers.forget_outdated(outdated);
        let found = answers.find(&album).expect("kept after the star");
        assert!(!found.aged);
        assert_eq!(found.answer, answer(now));
    }
}
