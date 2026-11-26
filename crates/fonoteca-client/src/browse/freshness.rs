//! What the home page says about the ListenBrainz data it shows: when it
//! came, and why asking for newer failed. The stats and the shelf of
//! playlists made for the user each keep one [`Freshness`]. A timer ticks
//! while the app runs: it words the ages anew, and asks again for what
//! stayed on the home page for an hour.

use std::rc::Rc;
use std::time::Duration;

use listenbrainz as lb;
use opensubsonic::FRESH_FOR;
use slint::{ComponentHandle, Timer, TimerMode};

use super::{Page, Shared, count, created_for, stats};
use crate::AppWindow;

/// How often the ages are worded anew, and what is due is asked for again.
const TICK_EVERY: Duration = Duration::from_secs(60);

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// How old the data on a page is, and how it fared when last asked for.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct Freshness {
    /// When the data shown came, by the answers' clock; none for none.
    at: Option<u64>,
    /// When ListenBrainz was last asked or the data last shown from this
    /// device, which an hour on makes it due.
    checked: u64,
    /// Why the last ask failed, while the data shown is older than it.
    failure: Option<String>,
}

impl Freshness {
    /// Data kept on this device since `at`, shown without asking.
    pub(super) fn kept(at: u64) -> Self {
        Freshness {
            at: Some(at),
            checked: at,
            failure: None,
        }
    }

    /// Data ListenBrainz gave at `now`.
    pub(super) fn updated(now: u64) -> Self {
        Self::kept(now)
    }

    /// An ask at `now` that failed for `problem`, while the data kept since
    /// `at`, if any, stays on show.
    pub(super) fn failed(problem: String, at: Option<u64>, now: u64) -> Self {
        Freshness {
            at,
            checked: now,
            failure: Some(problem),
        }
    }

    /// Whether it has been an hour since ListenBrainz was asked, or the data
    /// was shown; nothing shown is never due, for the page asks on opening.
    pub(super) fn is_due(&self, now: u64) -> bool {
        self.at.is_some() && now.saturating_sub(self.checked) >= FRESH_FOR
    }

    /// What the page says under its heading, none for no data.
    pub(super) fn note(&self, now: u64) -> String {
        let Some(at) = self.at else {
            return String::new();
        };
        let age = ago(now.saturating_sub(at));
        match &self.failure {
            Some(problem) => format!("Couldn't update, showing the data from {age}: {problem}"),
            None => format!("Updated {age}"),
        }
    }
}

/// "a moment ago", "12 minutes ago", "3 hours ago", "2 days ago".
fn ago(seconds: u64) -> String {
    let (n, one, many) = match seconds {
        0..MINUTE => return "a moment ago".into(),
        MINUTE..HOUR => (seconds / MINUTE, "minute", "minutes"),
        HOUR..DAY => (seconds / HOUR, "hour", "hours"),
        _ => (seconds / DAY, "day", "days"),
    };
    format!("{} ago", count(n as usize, one, many))
}

/// Why a call to ListenBrainz failed, in words for the page.
pub(super) fn why(error: &lb::Error) -> String {
    match error {
        lb::Error::Api {
            status: 401 | 403, ..
        } => "ListenBrainz rejected the token".into(),
        lb::Error::RateLimited { .. } => "ListenBrainz is limiting the calls, try later".into(),
        lb::Error::Http(_) => "ListenBrainz couldn't be reached".into(),
        other => other.to_string(),
    }
}

thread_local! {
    // Outside the Browser, which signing out replaces.
    static UPKEEP: Timer = Timer::default();
}

pub(super) fn wire(ui: &AppWindow, browser: &Shared) {
    // Weak, so that restarting the timer, as a second window does, doesn't
    // drop the last of a Browser while the timers are borrowed.
    let (weak, browser) = (ui.as_weak(), Rc::downgrade(browser));
    UPKEEP.with(|upkeep| {
        upkeep.start(TimerMode::Repeated, TICK_EVERY, move || {
            if let (Some(ui), Some(browser)) = (weak.upgrade(), browser.upgrade()) {
                tick(&ui, &browser);
            }
        });
    });
}

/// Words the ages anew, and asks again for what an open home page shows
/// that is due.
fn tick(ui: &AppWindow, browser: &Shared) {
    stats::show_note(ui, browser);
    created_for::show_note(ui, browser);
    if ui.get_page() != Page::Home || ui.get_listenbrainz_user().is_empty() {
        return;
    }
    if stats::is_due(browser) {
        stats::ask(ui, browser);
    }
    if created_for::is_due(browser) {
        created_for::shelf(ui, browser);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 10 * DAY;

    #[test]
    fn an_age_is_worded_in_the_biggest_unit_it_fills() {
        assert_eq!(ago(0), "a moment ago");
        assert_eq!(ago(59), "a moment ago");
        assert_eq!(ago(MINUTE), "1 minute ago");
        assert_eq!(ago(12 * MINUTE + 30), "12 minutes ago");
        assert_eq!(ago(HOUR), "1 hour ago");
        assert_eq!(ago(3 * HOUR + 59 * MINUTE), "3 hours ago");
        assert_eq!(ago(DAY), "1 day ago");
        assert_eq!(ago(2 * DAY + HOUR), "2 days ago");
    }

    #[test]
    fn the_note_says_when_the_data_came_or_why_it_is_older() {
        let kept = Freshness::kept(NOW - 12 * MINUTE);
        assert_eq!(kept.note(NOW), "Updated 12 minutes ago");
        let failed = Freshness::failed("offline".into(), Some(NOW - 2 * DAY), NOW);
        assert_eq!(
            failed.note(NOW),
            "Couldn't update, showing the data from 2 days ago: offline"
        );
        let nothing = Freshness::failed("offline".into(), None, NOW);
        assert_eq!(
            nothing.note(NOW),
            "",
            "the page says it where it has no data"
        );
        assert_eq!(Freshness::default().note(NOW), "");
    }

    #[test]
    fn data_is_due_an_hour_after_it_was_asked_for_even_when_the_ask_failed() {
        let kept = Freshness::kept(NOW - FRESH_FOR + 1);
        assert!(!kept.is_due(NOW));
        assert!(Freshness::kept(NOW - FRESH_FOR).is_due(NOW));
        let failed = Freshness::failed("rate limited".into(), Some(NOW - 5 * HOUR), NOW);
        assert!(
            !failed.is_due(NOW + FRESH_FOR - 1),
            "not asked again at once"
        );
        assert!(failed.is_due(NOW + FRESH_FOR));
        assert!(!Freshness::default().is_due(NOW), "a page asks when opened");
    }

    #[test]
    fn a_failure_is_worded_by_its_cause() {
        let refused = |status| lb::Error::Api {
            status,
            message: "no".into(),
        };
        assert_eq!(why(&refused(401)), "ListenBrainz rejected the token");
        assert_eq!(why(&refused(403)), "ListenBrainz rejected the token");
        assert!(why(&refused(500)).contains("500"));
        let limited = lb::Error::RateLimited { reset_in: Some(3) };
        assert_eq!(
            why(&limited),
            "ListenBrainz is limiting the calls, try later"
        );
    }
}
