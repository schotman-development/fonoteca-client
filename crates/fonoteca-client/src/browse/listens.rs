//! The listening history: what was played on this device, from its own log
//! of [`plays`], and with a ListenBrainz token the listens it has from
//! elsewhere too, newest first, a hundred to a page. "history" is the newest
//! page, "history:before:<time>" the page before a time and
//! "history:after:<time>" the page after one, so Back and Forward go through
//! the pages seen. A play on this device shows at once and plays. A listen
//! from elsewhere is greyed until the whole page is looked up in the library,
//! and those it has light up together; [`matching`] says which songs are the
//! library's.
//!
//! The listens are kept on this device as one unbroken run, which serves
//! the pages it holds at once. An hour on, or once a listen was sent, the
//! listens since its newest are put on top, as long as ListenBrainz's count
//! of them agrees; else, as when listens came late with an older time, the
//! newest page takes its place. Older pages are added below as they are
//! seen.

use std::cmp::Reverse;

use listenbrainz as lb;
use serde::{Deserialize, Serialize};

use super::created_for::{self, is_missing, missing_row, missing_song};
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
/// Where the listens are kept, before the ListenBrainz user's name.
pub(super) const KEPT_UNDER: &str = "listens:";

/// A user's listens as kept on this device: one unbroken run of them, newest
/// first, from the newest as of the last ask down to the oldest page seen;
/// when ListenBrainz said the newest and oldest of all were heard; and how
/// many it counted at the last ask.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Run {
    listens: Vec<lb::Listened>,
    newest_at: Option<i64>,
    oldest_at: Option<i64>,
    count: u64,
}

impl Run {
    fn new(page: lb::ListensPage, count: u64) -> Self {
        Run {
            listens: page.listens,
            newest_at: page.newest_at,
            oldest_at: page.oldest_at,
            count,
        }
    }

    /// When its newest and its oldest listen were heard.
    fn newest(&self) -> Option<i64> {
        self.listens.first().map(|listen| listen.listened_at)
    }

    fn oldest(&self) -> Option<i64> {
        self.listens.last().map(|listen| listen.listened_at)
    }

    /// Whether it goes back to the user's first listen.
    fn reaches_start(&self) -> bool {
        match (self.oldest(), self.oldest_at) {
            (Some(oldest), Some(first)) => oldest <= first,
            _ => true,
        }
    }

    /// The listens of the page `around` asks for, when the run holds them
    /// all: the newest, the nearest before a time, or the nearest after one.
    fn page(&self, around: lb::Around) -> Option<lb::ListensPage> {
        let listens = match around {
            lb::Around::Newest => self.listens.iter().take(PAGE).cloned().collect(),
            lb::Around::Before(time) => {
                let before: Vec<lb::Listened> = (self.listens.iter())
                    .filter(|listen| listen.listened_at < time)
                    .take(PAGE)
                    .cloned()
                    .collect();
                if before.len() < PAGE && !self.reaches_start() {
                    return None;
                }
                before
            }
            lb::Around::After(time) => {
                let reaches_back = self.oldest().is_none_or(|oldest| oldest <= time);
                if !reaches_back && !self.reaches_start() {
                    return None;
                }
                let after = self
                    .listens
                    .iter()
                    .filter(|listen| listen.listened_at > time);
                let mut nearest: Vec<lb::Listened> = after.rev().take(PAGE).cloned().collect();
                nearest.reverse();
                nearest
            }
        };
        Some(lb::ListensPage {
            listens,
            newest_at: self.newest_at,
            oldest_at: self.oldest_at,
        })
    }

    /// The run with `newer`, the listens heard since its newest, on top;
    /// none when ListenBrainz now counts others, as when listens came late
    /// with an older time or were deleted, or when as many came as a page
    /// holds, which may leave some out.
    fn with_newer(mut self, newer: lb::ListensPage, count: u64) -> Option<Run> {
        let added = newer.listens.len();
        let is_whole = added < lb::MAX_LISTENS_PER_PAGE as usize;
        if !is_whole || self.count + added as u64 != count {
            return None;
        }
        self.listens.splice(0..0, newer.listens);
        self.count = count;
        self.newest_at = newer.newest_at.or(self.newest_at);
        self.oldest_at = newer.oldest_at.or(self.oldest_at);
        Some(self)
    }

    /// When its oldest listen was heard, where the page `around` asks for
    /// goes on below it, so that the page before that one is the run's next.
    fn goes_on_below(&self, around: lb::Around) -> Option<i64> {
        let lb::Around::Before(time) = around else {
            return None;
        };
        self.oldest().filter(|&oldest| oldest <= time)
    }

    /// The run with `older`, the page before its oldest, at its end.
    fn with_older(mut self, older: lb::ListensPage) -> Run {
        let oldest = self.oldest();
        let before = (older.listens.into_iter())
            .filter(|listen| oldest.is_none_or(|oldest| listen.listened_at < oldest));
        self.listens.extend(before);
        self.oldest_at = older.oldest_at.or(self.oldest_at);
        self
    }
}

/// Where the listens of `user` are kept.
fn kept_under(user: &str) -> String {
    format!("{KEPT_UNDER}{user}")
}

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
/// with ListenBrainz's listens kept merged in, then as ListenBrainz has them
/// now where they are an hour old, which it looks up.
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
        let lb_account = created_for::account().await;
        let answers = listenbrainz_answers(&browser).await;
        let Some(ui) = still_on(&weak, &browser, visit) else {
            return;
        };
        let (Some((client, user)), Some(answers)) = (lb_account, answers) else {
            return ui.set_loading(false);
        };
        let kept = answers::find::<Run>(&answers, &kept_under(&user));
        let kept_page = kept.as_ref().and_then(|(run, _)| run.page(around));
        let shown = kept_page
            .clone()
            .map(|listens| merge(&plays, listens, around, PAGE));
        if let Some(timeline) = &shown {
            show_timeline(&ui, &browser, timeline);
        }
        let fetched = listens(&answers, &client, &user, around, kept).await;
        let Some(ui) = still_on(&weak, &browser, visit) else {
            return;
        };
        ui.set_loading(false);
        let listens = match (fetched, kept_page) {
            (Ok(listens), _) => listens,
            (Err(e), Some(kept)) => {
                eprintln!("{e}; the listens kept stay");
                kept
            }
            (Err(e), None) => return ui.set_page_problem(e.into()),
        };
        let timeline = merge(&plays, listens, around, PAGE);
        if shown.as_ref() != Some(&timeline) {
            show_timeline(&ui, &browser, &timeline);
        }
        let has_elsewhere =
            (timeline.heard.iter()).any(|heard| matches!(heard, Heard::Elsewhere(_)));
        if has_elsewhere {
            ui.set_header_facts(FINDING.into());
            find_songs(&weak, &browser, &timeline.heard, visit).await;
        }
    })
    .expect("the event loop is running");
}

/// The listens of the page `around` asks for: from the run kept, brought up
/// to date first when it is an hour old or there is none, and else from
/// ListenBrainz. A page just before the run is added to it.
async fn listens(
    answers: &Answers,
    client: &lb::Client,
    user: &str,
    around: lb::Around,
    kept: Option<(Run, bool)>,
) -> Result<lb::ListensPage, String> {
    let problem = |e: lb::Error| format!("Your listens on ListenBrainz couldn't be fetched: {e}");
    let run = match kept {
        Some((run, false)) => run,
        stale => {
            let writes = answers.writes();
            let run =
                (refreshed(client, user, stale.map(|(run, _)| run)).await).map_err(problem)?;
            answers::keep(answers, kept_under(user), &run, writes);
            run
        }
    };
    if let Some(page) = run.page(around) {
        return Ok(page);
    }
    if let Some(oldest) = run.goes_on_below(around) {
        let older = (client
            .listens(user, lb::Around::Before(oldest), PAGE as u32)
            .await)
            .map_err(problem)?;
        let run = run.with_older(older);
        answers::amend(answers, &kept_under(user), &run);
        if let Some(page) = run.page(around) {
            return Ok(page);
        }
    }
    // Pages apart from the run, as through Back after it was replaced, are
    // shown but not kept, which would break it.
    (client.listens(user, around, PAGE as u32).await).map_err(problem)
}

/// `run` with the listens heard since its newest on top, unless ListenBrainz
/// counts others than it would then hold: then, or without a run, the newest
/// page.
async fn refreshed(client: &lb::Client, user: &str, run: Option<Run>) -> lb::Result<Run> {
    let Some((run, newest)) = run.and_then(|run| run.newest().map(|newest| (run, newest))) else {
        return newest_run(client, user, None).await;
    };
    let newer = (client.listens(user, lb::Around::After(newest), lb::MAX_LISTENS_PER_PAGE)).await?;
    let count = client.listen_count(user).await?;
    match run.with_newer(newer, count) {
        Some(run) => Ok(run),
        None => newest_run(client, user, Some(count)).await,
    }
}

/// The newest page of listens as a run, with `count`, or the count
/// ListenBrainz gives.
async fn newest_run(client: &lb::Client, user: &str, count: Option<u64>) -> lb::Result<Run> {
    let page = client
        .listens(user, lb::Around::Newest, PAGE as u32)
        .await?;
    let count = match count {
        Some(count) => count,
        None => client.listen_count(user).await?,
    };
    Ok(Run::new(page, count))
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

    /// A run of listens at each of `times`, newest first, of `count`
    /// listens heard between `newest_at` and `oldest_at`.
    fn run(times: &[i64], newest_at: i64, oldest_at: i64, count: u64) -> Run {
        let listens = times.iter().map(|&at| listened(at, "Halo", "Ann", None));
        Run::new(page(listens.collect(), newest_at, oldest_at), count)
    }

    fn at(page: &lb::ListensPage) -> Vec<i64> {
        page.listens
            .iter()
            .map(|listen| listen.listened_at)
            .collect()
    }

    #[test]
    fn the_run_kept_serves_the_pages_it_holds_whole() {
        let times: Vec<i64> = (0..150).map(|n| 1000 - n).collect();
        let kept = run(&times, 1000, 0, 400);
        assert_eq!(at(&kept.page(lb::Around::Newest).unwrap()), times[..100]);
        assert_eq!(
            at(&kept.page(lb::Around::Before(1000 - 20)).unwrap()),
            times[21..121]
        );
        assert!(
            kept.page(lb::Around::Before(1000 - 99)).is_none(),
            "fewer than a page below it, and more before the run"
        );
        assert_eq!(
            at(&kept.page(lb::Around::After(1000 - 120)).unwrap()),
            times[20..120]
        );
        assert!(
            kept.page(lb::Around::After(10)).is_none(),
            "from before the run"
        );
        let page = kept.page(lb::Around::Newest).unwrap();
        assert_eq!((page.newest_at, page.oldest_at), (Some(1000), Some(0)));
    }

    #[test]
    fn a_run_back_to_the_first_listen_serves_the_last_pages_short() {
        let kept = run(&[30, 20, 10], 30, 10, 3);
        assert_eq!(at(&kept.page(lb::Around::Before(25)).unwrap()), [20, 10]);
        assert_eq!(at(&kept.page(lb::Around::After(5)).unwrap()), [30, 20, 10]);
        let nothing_yet = Run::default();
        assert!(
            nothing_yet
                .page(lb::Around::Newest)
                .unwrap()
                .listens
                .is_empty()
        );
    }

    #[test]
    fn listens_since_go_on_top_while_the_count_agrees() {
        let kept = run(&[30, 20], 30, 0, 10);
        let newer = page(vec![listened(50, "Halo", "Ann", None)], 50, 0);
        let up_to_date = kept.clone().with_newer(newer.clone(), 11).unwrap();
        assert_eq!(
            at(&up_to_date.page(lb::Around::Newest).unwrap()),
            [50, 30, 20]
        );
        assert_eq!((up_to_date.newest_at, up_to_date.count), (Some(50), 11));
        assert!(
            kept.clone().with_newer(newer, 12).is_none(),
            "one came late with an older time"
        );
        let none_since = page(Vec::new(), 30, 0);
        assert_eq!(
            kept.clone().with_newer(none_since.clone(), 10),
            Some(kept.clone())
        );
        assert!(kept.with_newer(none_since, 9).is_none(), "one was deleted");
    }

    #[test]
    fn as_many_listens_since_as_a_page_holds_replace_the_run() {
        let kept = run(&[30], 30, 0, 1);
        let full = lb::MAX_LISTENS_PER_PAGE as i64;
        let listens = (0..full).map(|n| listened(100 + n, "Halo", "Ann", None));
        let newer = page(listens.rev().collect(), 100 + full, 0);
        assert!(kept.with_newer(newer, 1 + full as u64).is_none());
    }

    #[test]
    fn a_page_from_the_oldest_kept_down_goes_on_below_the_run() {
        let kept = run(&[30, 20], 30, 0, 4);
        assert_eq!(kept.goes_on_below(lb::Around::Before(20)), Some(20));
        assert_eq!(kept.goes_on_below(lb::Around::Before(25)), Some(20));
        assert_eq!(
            kept.goes_on_below(lb::Around::Before(15)),
            None,
            "apart from it"
        );
        assert_eq!(kept.goes_on_below(lb::Around::After(20)), None);
        assert_eq!(Run::default().goes_on_below(lb::Around::Before(20)), None);
    }

    #[test]
    fn the_page_before_the_run_goes_at_its_end() {
        let kept = run(&[30, 20], 30, 0, 4);
        let older = page(
            vec![
                listened(20, "Halo", "Ann", None),
                listened(10, "Halo", "Ann", None),
                listened(5, "Halo", "Ann", None),
            ],
            30,
            5,
        );
        let longer = kept.with_older(older);
        assert_eq!(
            at(&longer.page(lb::Around::Newest).unwrap()),
            [30, 20, 10, 5]
        );
        assert_eq!(longer.oldest_at, Some(5));
        assert!(longer.reaches_start());
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
