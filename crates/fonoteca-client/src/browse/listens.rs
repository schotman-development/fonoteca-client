//! The listening history: what was played on this device, from its own log
//! of [`plays`], and with a ListenBrainz token the listens it has from
//! elsewhere too, newest first, a hundred to a page. "history" is the newest
//! page, "history:before:<time>" the page before a time and
//! "history:after:<time>" the page after one, so Back and Forward go through
//! the pages seen. A play on this device shows at once and plays. A listen
//! from elsewhere is greyed until the whole page is looked up in the library,
//! and those it has light up together; [`matching`] says which songs are the
//! library's.

use std::cmp::Reverse;

use listenbrainz as lb;

use super::created_for::{account, is_missing, missing_row, missing_song};
use super::matching::{lookup, pick};
use super::*;
use crate::plays::{self, Play};

/// The plays and listens on a page. ListenBrainz is asked for as many
/// listens, so that none of them falls between two pages.
const PAGE: usize = 100;
const DAY: i64 = 24 * 60 * 60;

/// What the page says while it looks for the listens from elsewhere in the
/// library.
const FINDING: &str = "Finding these in your library…";

/// A row of the history: a play on this device, or a listen ListenBrainz
/// has from somewhere else.
#[derive(Debug, Clone, PartialEq)]
enum Heard {
    Here(Play),
    Elsewhere(lb::Listened),
}

impl Heard {
    /// When it began, in seconds since 1970.
    fn at(&self) -> i64 {
        match self {
            Heard::Here(play) => play.listened_at,
            Heard::Elsewhere(listen) => listen.listened_at,
        }
    }
}

/// A page of the history, newest first, and whether there is more on
/// either side of it.
#[derive(Debug, PartialEq)]
struct Timeline {
    heard: Vec<Heard>,
    has_newer: bool,
    has_older: bool,
}

/// The listens the link's `id` asks for: "before:<time>", "after:<time>",
/// else the newest.
fn around(id: &str) -> lb::Around {
    let asked = match id.split_once(':') {
        Some(("before", at)) => at.parse().ok().map(lb::Around::Before),
        Some(("after", at)) => at.parse().ok().map(lb::Around::After),
        _ => None,
    };
    asked.unwrap_or(lb::Around::Newest)
}

/// The page `around` asks for, at most `count` long, of `plays`, every play
/// on this device, and `listens`, ListenBrainz's page for it. A listen that
/// is one of the plays, sent from here, shows once, as the play.
fn merge(plays: &[Play], listens: lb::ListensPage, around: lb::Around, count: usize) -> Timeline {
    let is_on_page = |at: i64| match around {
        lb::Around::Newest => true,
        lb::Around::Before(time) => at < time,
        lb::Around::After(time) => at > time,
    };
    let here = plays.iter().filter(|play| is_on_page(play.listened_at));
    let mut heard: Vec<Heard> = here.cloned().map(Heard::Here).collect();
    let elsewhere = (listens.listens.into_iter())
        .filter(|listen| !plays.iter().any(|play| is_sent(play, listen)));
    heard.extend(elsewhere.map(Heard::Elsewhere));
    heard.sort_by_key(|one| Reverse(one.at()));
    match around {
        // The page after a time is of those nearest it, the oldest.
        lb::Around::After(_) => {
            let newer = heard.len().saturating_sub(count);
            heard.drain(..newer);
        }
        _ => heard.truncate(count),
    }
    let newest = heard.first().map(Heard::at);
    let oldest = heard.last().map(Heard::at);
    let has_newer = newest.is_some_and(|newest| {
        plays.iter().any(|play| play.listened_at > newest)
            || listens.newest_at.is_some_and(|at| at > newest)
    });
    let has_older = oldest.is_some_and(|oldest| {
        plays.iter().any(|play| play.listened_at < oldest)
            || listens.oldest_at.is_some_and(|at| at < oldest)
    });
    Timeline {
        heard,
        has_newer,
        has_older,
    }
}

/// Whether `listen` is `play`, sent to ListenBrainz from here: heard at the
/// same time, and of its recording, else with its title and an artist.
fn is_sent(play: &Play, listen: &lb::Listened) -> bool {
    play.listened_at == listen.listened_at && pick(&listen.track, &[play.song()]).is_some()
}

/// The links of the pages of newer and of older plays and listens, "" for
/// none.
fn beside(timeline: &Timeline) -> (String, String) {
    let newer = (timeline.heard.first())
        .filter(|_| timeline.has_newer)
        .map(|newest| format!("history:after:{}", newest.at()));
    let older = (timeline.heard.last())
        .filter(|_| timeline.has_older)
        .map(|oldest| format!("history:before:{}", oldest.at()));
    (newer.unwrap_or_default(), older.unwrap_or_default())
}

/// How long before `now` a listen at `at` was, short enough to fit where a
/// row has its length, so the rows stay in line on a phone: "now", "5m",
/// "3h", "2d", "1w", "4mo", "2y".
// ponytail: relative times only, since std can't tell the device's UTC
// offset for a date and clock time; add one with a time zone source.
fn ago(at: i64, now: i64) -> String {
    const UNITS: [(i64, &str); 6] = [
        (365 * DAY, "y"),
        (30 * DAY, "mo"),
        (7 * DAY, "w"),
        (DAY, "d"),
        (60 * 60, "h"),
        (60, "m"),
    ];
    let seconds = now - at;
    match UNITS.iter().find(|(length, _)| seconds >= *length) {
        Some(&(length, unit)) => format!("{}{unit}", seconds / length),
        None => "now".into(),
    }
}

/// The songs of a page before any is looked up: a play's own, and a
/// missing one for a listen from elsewhere.
fn first_songs(heard: &[Heard]) -> Vec<Child> {
    (heard.iter())
        .map(|heard| match heard {
            Heard::Here(play) => play.song(),
            Heard::Elsewhere(listen) => missing_song(&listen.track),
        })
        .collect()
}

/// The row of a play or listen with its song: greyed for a listen the
/// library lacks, and with when it was heard where a row has its length.
fn heard_row(heard: &Heard, song: &Child, now: i64) -> Track {
    let row = match heard {
        Heard::Elsewhere(listen) if is_missing(song) => missing_row(&listen.track),
        _ => song_track(song),
    };
    Track {
        duration: ago(heard.at(), now).into(),
        ..row
    }
}

/// Puts the plays and listens on the page with their songs, a row each.
fn show_heard(browser: &Shared, heard: &[Heard], songs: Vec<Child>) {
    let now = crate::prefs::now() as i64;
    let rows = (heard.iter().zip(&songs))
        .map(|(heard, song)| heard_row(heard, song, now))
        .collect();
    show_songs(&mut browser.borrow_mut(), songs, rows);
}

/// Puts `timeline` on the page, its listens from elsewhere greyed, with the
/// links to the pages beside it.
fn show_timeline(ui: &AppWindow, browser: &Shared, timeline: &Timeline) {
    let (newer, older) = beside(timeline);
    ui.set_newer_listens(newer.into());
    ui.set_older_listens(older.into());
    show_heard(browser, &timeline.heard, first_songs(&timeline.heard));
    show_downloads(ui, browser);
}

/// The window, while it still shows the visit `visit` of the page.
fn still_on(weak: &slint::Weak<AppWindow>, browser: &Shared, visit: i32) -> Option<AppWindow> {
    let ui = weak.upgrade()?;
    (browser.borrow().visit == visit).then_some(ui)
}

/// Shows the page the link's `id` asks for: this device's plays at once,
/// then with ListenBrainz's listens merged in, which it looks up.
pub(super) fn open_page(ui: &AppWindow, browser: &Shared, id: &str) {
    let visit = browser.borrow().visit;
    let around = around(id);
    let account = account_key(&browser.borrow()).map(str::to_owned);
    ui.set_newer_listens("".into());
    ui.set_older_listens("".into());
    ui.set_loading(true);
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let plays = match &account {
            Some(account) => plays::read(account).await,
            None => Vec::new(),
        };
        let Some(ui) = still_on(&weak, &browser, visit) else {
            return;
        };
        let played_here = merge(&plays, lb::ListensPage::default(), around, PAGE);
        show_timeline(&ui, &browser, &played_here);
        let fetched = fetch(around).await;
        let Some(ui) = still_on(&weak, &browser, visit) else {
            return;
        };
        ui.set_loading(false);
        let listens = match fetched {
            Ok(Some(listens)) => listens,
            Ok(None) => return,
            Err(e) => return ui.set_page_problem(e.into()),
        };
        let timeline = merge(&plays, listens, around, PAGE);
        show_timeline(&ui, &browser, &timeline);
        let has_elsewhere =
            (timeline.heard.iter()).any(|heard| matches!(heard, Heard::Elsewhere(_)));
        if has_elsewhere {
            ui.set_header_facts(FINDING.into());
            find_songs(&weak, &browser, &timeline.heard, visit).await;
        }
    })
    .expect("the event loop is running");
}

/// ListenBrainz's page of the listens `around` asks for, none without a
/// token.
async fn fetch(around: lb::Around) -> Result<Option<lb::ListensPage>, String> {
    let Some((client, user)) = account().await else {
        return Ok(None);
    };
    (client.listens(&user, around, PAGE as u32).await)
        .map(Some)
        .map_err(|e| format!("Your listens on ListenBrainz couldn't be fetched: {e}"))
}

/// Looks for the library's song of each listen from elsewhere, one search
/// after another, and then shows the rows of all those it has at once.
/// Listens of a track looked up before this session take no search.
async fn find_songs(weak: &slint::Weak<AppWindow>, browser: &Shared, heard: &[Heard], visit: i32) {
    let (Some(client), cache) = ({
        let b = browser.borrow();
        (b.client.clone(), b.found.clone())
    }) else {
        return;
    };
    let mut songs = Vec::with_capacity(heard.len());
    for entry in heard {
        let listen = match entry {
            Heard::Here(play) => {
                songs.push(play.song());
                continue;
            }
            Heard::Elsewhere(listen) => listen,
        };
        let found = lookup(&cache, &client, &listen.track).await;
        let Some(ui) = still_on(weak, browser, visit) else {
            return;
        };
        match found {
            Ok(found) => songs.push(found.unwrap_or_else(|| missing_song(&listen.track))),
            Err(e) => {
                ui.set_header_facts("".into());
                return ui.set_page_problem(format!("Your server didn't answer: {e}").into());
            }
        }
    }
    let Some(ui) = still_on(weak, browser, visit) else {
        return;
    };
    show_heard(browser, heard, songs);
    ui.set_header_facts("".into());
    show_downloads(&ui, browser);
}

#[cfg(test)]
mod tests {
    use super::super::matching::fixtures::{song, wanted};
    use super::*;

    fn played(at: i64, title: &str, artist: &str, recording: Option<&str>) -> Play {
        Play::of(&song(&format!("s{at}"), title, artist, recording), at)
    }

    fn listened(at: i64, title: &str, artist: &str, recording: Option<&str>) -> lb::Listened {
        lb::Listened {
            listened_at: at,
            track: wanted(title, artist, recording),
        }
    }

    /// When each was heard, and whether here.
    fn times(timeline: &Timeline) -> Vec<(i64, bool)> {
        (timeline.heard.iter())
            .map(|heard| (heard.at(), matches!(heard, Heard::Here(_))))
            .collect()
    }

    fn page(listens: Vec<lb::Listened>, newest_at: i64, oldest_at: i64) -> lb::ListensPage {
        lb::ListensPage {
            listens,
            newest_at: Some(newest_at),
            oldest_at: Some(oldest_at),
        }
    }

    #[test]
    fn a_page_of_the_history_lights_history_in_the_sidebar_and_browse_in_the_bar() {
        let b = Browser {
            history: vec!["history:before:1700".into()],
            ..Browser::default()
        };
        assert_eq!((section(&b), bar_section(&b)), (3, 1));
    }

    #[test]
    fn a_row_plays_with_the_page_s_other_rows_the_library_has() {
        let browser = Shared::default();
        let gone = missing_song(&wanted("Gone", "Nobody", None));
        browser.borrow_mut().songs =
            vec![song("a", "A", "X", None), gone, song("b", "B", "X", None)];
        let (songs, start) = super::super::songs(&browser, Page::History, 2);
        let ids: Vec<&str> = songs.iter().map(|song| song.id.as_str()).collect();
        assert_eq!((ids, start), (vec!["a", "b"], 1));
    }

    #[test]
    fn a_link_asks_for_the_listens_before_or_after_a_time_else_the_newest() {
        assert_eq!(around(""), lb::Around::Newest);
        assert_eq!(around("before:1700"), lb::Around::Before(1700));
        assert_eq!(around("after:1700"), lb::Around::After(1700));
        assert_eq!(around("after:soon"), lb::Around::Newest);
        assert_eq!(around("since:1700"), lb::Around::Newest);
    }

    #[test]
    fn plays_here_and_listens_from_elsewhere_are_merged_newest_first() {
        let plays = [
            played(10, "Valve", "The Steam Collective", None),
            played(30, "Salt", "Mara Lind", None),
        ];
        let listens = vec![
            listened(40, "Tell Him", "Röyksopp", None),
            listened(20, "Gone", "Nobody", None),
        ];
        let timeline = merge(&plays, page(listens, 40, 20), lb::Around::Newest, 10);
        assert_eq!(
            times(&timeline),
            [(40, false), (30, true), (20, false), (10, true)]
        );
        assert!(!timeline.has_newer && !timeline.has_older);
    }

    #[test]
    fn a_play_sent_to_listenbrainz_shows_once_as_the_play() {
        let recording = "00000000-0000-4000-8000-000000000005";
        let plays = [
            played(30, "Salt", "Mara Lind", Some(recording)),
            played(20, "Café del Mar", "Energy 52", None),
        ];
        // Known by its recording, or without one, by its title and artist
        // however cased and accented; at another time, or of another
        // track at that time, a listen is another.
        let listens = vec![
            listened(30, "Salt (Live)", "Mara Lind", Some(recording)),
            listened(25, "Salt", "Mara Lind", Some(recording)),
            listened(
                20,
                "cafe del mar",
                "ENERGY 52",
                Some("ffffffff-0000-4000-8000-000000000000"),
            ),
            listened(20, "Gone", "Nobody", None),
        ];
        let timeline = merge(&plays, page(listens, 30, 20), lb::Around::Newest, 10);
        assert_eq!(
            times(&timeline),
            [(30, true), (25, false), (20, true), (20, false)]
        );
        let Heard::Elsewhere(gone) = &timeline.heard[3] else {
            panic!("a listen")
        };
        assert_eq!(gone.track.title, "Gone");
    }

    #[test]
    fn a_page_before_a_time_is_of_the_newest_before_it() {
        let plays = [played(50, "A", "X", None), played(40, "B", "X", None)];
        let listens = vec![
            listened(44, "C", "X", None),
            listened(30, "D", "X", None),
            listened(20, "E", "X", None),
        ];
        let timeline = merge(&plays, page(listens, 60, 10), lb::Around::Before(45), 3);
        assert_eq!(times(&timeline), [(44, false), (40, true), (30, false)]);
        assert!(timeline.has_newer && timeline.has_older);
    }

    #[test]
    fn a_page_after_a_time_is_of_the_oldest_after_it() {
        let plays = [played(50, "A", "X", None), played(40, "B", "X", None)];
        let listens = vec![listened(30, "C", "X", None), listened(20, "D", "X", None)];
        let timeline = merge(&plays, page(listens, 30, 10), lb::Around::After(15), 3);
        assert_eq!(times(&timeline), [(40, true), (30, false), (20, false)]);
        assert!(timeline.has_newer && timeline.has_older);
    }

    #[test]
    fn more_on_either_side_is_said_by_the_plays_here_or_by_listenbrainz() {
        let plays = [played(30, "A", "X", None), played(20, "B", "X", None)];
        let only_here = merge(
            &plays,
            lb::ListensPage::default(),
            lb::Around::Before(30),
            10,
        );
        assert_eq!(times(&only_here), [(20, true)]);
        assert!(only_here.has_newer && !only_here.has_older);
        let earlier = vec![listened(10, "C", "X", None)];
        let with_older = merge(&plays, page(earlier, 30, 5), lb::Around::Before(30), 2);
        assert_eq!(times(&with_older), [(20, true), (10, false)]);
        assert!(with_older.has_newer && with_older.has_older);
        let to_the_end = merge(&plays, page(Vec::new(), 30, 30), lb::Around::After(20), 10);
        assert_eq!(times(&to_the_end), [(30, true)]);
        assert!(!to_the_end.has_newer && to_the_end.has_older);
        let one_before = vec![listened(25, "C", "X", None)];
        let elsewhere = merge(&[], page(one_before, 40, 25), lb::Around::Before(30), 10);
        assert_eq!(times(&elsewhere), [(25, false)]);
        assert!(elsewhere.has_newer && !elsewhere.has_older);
        let nothing = merge(&[], lb::ListensPage::default(), lb::Around::Newest, 10);
        assert_eq!(
            nothing,
            Timeline {
                heard: Vec::new(),
                has_newer: false,
                has_older: false
            }
        );
    }

    #[test]
    fn the_pages_beside_start_at_the_newest_and_oldest_on_this_one() {
        let timeline = |has_newer, has_older| Timeline {
            heard: vec![
                Heard::Here(played(30, "A", "X", None)),
                Heard::Elsewhere(listened(20, "B", "X", None)),
                Heard::Here(played(10, "C", "X", None)),
            ],
            has_newer,
            has_older,
        };
        assert_eq!(
            beside(&timeline(true, true)),
            ("history:after:30".into(), "history:before:10".into())
        );
        assert_eq!(
            beside(&timeline(false, false)),
            (String::new(), String::new())
        );
        assert_eq!(
            beside(&timeline(false, true)),
            (String::new(), "history:before:10".into())
        );
    }

    #[test]
    fn a_listen_says_how_long_ago_it_was_heard() {
        let now = 1_000_000_000;
        let said = |seconds_before: i64| ago(now - seconds_before, now);
        assert_eq!(said(0), "now");
        assert_eq!(said(59), "now");
        assert_eq!(said(-30), "now");
        assert_eq!(said(60), "1m");
        assert_eq!(said(60 * 60 - 1), "59m");
        assert_eq!(said(60 * 60), "1h");
        assert_eq!(said(DAY - 1), "23h");
        assert_eq!(said(DAY), "1d");
        assert_eq!(said(7 * DAY - 1), "6d");
        assert_eq!(said(7 * DAY), "1w");
        assert_eq!(said(30 * DAY - 1), "4w");
        assert_eq!(said(30 * DAY), "1mo");
        assert_eq!(said(365 * DAY - 1), "12mo");
        assert_eq!(said(365 * DAY), "1y");
        assert_eq!(said(3 * 365 * DAY), "3y");
    }

    #[test]
    fn a_play_here_always_plays_and_a_listen_the_library_lacks_is_greyed() {
        let here = Heard::Here(played(1000, "Familiar", "Nils Frahm", None));
        let [own] = first_songs(std::slice::from_ref(&here)).try_into().unwrap();
        let row = heard_row(&here, &own, 1000);
        assert!(!row.unavailable);
        assert_eq!((own.id.as_str(), row.duration.as_str()), ("s1000", "now"));
        let elsewhere = Heard::Elsewhere(listened(1000, "Gone", "Nobody", None));
        let [missing] = first_songs(std::slice::from_ref(&elsewhere))
            .try_into()
            .unwrap();
        let row = heard_row(&elsewhere, &missing, 1000 + DAY);
        assert!(row.unavailable);
        assert_eq!((row.title.as_str(), row.duration.as_str()), ("Gone", "1d"));
        let found = song("a", "Gone", "Nobody", None);
        assert!(!heard_row(&elsewhere, &found, 1000).unavailable);
    }
}
