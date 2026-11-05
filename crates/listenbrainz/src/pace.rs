//! The one gate every call to ListenBrainz passes through. Its docs say each
//! client application must never make more than one call a second, whichever
//! of the app's features makes it, so the calls queue here, each starting a
//! second after the one before.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// How far apart ListenBrainz's own server wants calls to start.
pub const CALL_SPACING: Duration = Duration::from_secs(1);

/// How far apart calls to a stand-in for ListenBrainz start: far enough to
/// tell the gate is there, and short enough for tests.
pub const STAND_IN_SPACING: Duration = Duration::from_millis(50);

/// When the next call may start, as the time since the clock began, or none
/// before the first call.
static NEXT_START: Mutex<Option<Duration>> = Mutex::new(None);

/// Waits for the call's turn, `spacing` after the start of the one before.
pub(crate) async fn wait_turn(spacing: Duration) {
    let wait = {
        let mut next_start = NEXT_START.lock().unwrap_or_else(PoisonError::into_inner);
        take_turn(&mut next_start, clock::now(), spacing)
    };
    if !wait.is_zero() {
        clock::sleep(wait).await;
    }
}

/// How long a call arriving at `now` waits, which also books the turn after it.
fn take_turn(next_start: &mut Option<Duration>, now: Duration, spacing: Duration) -> Duration {
    let start = next_start.map_or(now, |next| next.max(now));
    *next_start = Some(start + spacing);
    start - now
}

/// The clock and the timer the gate runs on. std's clock doesn't exist on
/// wasm, and no timer does without a runtime, so the browser's are the app's
/// to hand over.
#[cfg(not(target_family = "wasm"))]
mod clock {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    static BEGAN: OnceLock<Instant> = OnceLock::new();

    pub(super) fn now() -> Duration {
        BEGAN.get_or_init(Instant::now).elapsed()
    }

    pub(super) async fn sleep(duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

#[cfg(target_family = "wasm")]
mod clock {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::OnceLock;
    use std::time::Duration;

    /// A sleep in the browser's event loop.
    pub type Sleep = fn(Duration) -> Pin<Box<dyn Future<Output = ()>>>;

    pub(super) struct Browser {
        pub(super) now_ms: fn() -> f64,
        pub(super) sleep: Sleep,
    }

    pub(super) static BROWSER: OnceLock<Browser> = OnceLock::new();

    /// Without the browser's clock calls go out as they come.
    pub(super) fn now() -> Duration {
        BROWSER.get().map_or(Duration::ZERO, |b| {
            Duration::from_secs_f64((b.now_ms)() / 1000.0)
        })
    }

    pub(super) async fn sleep(duration: Duration) {
        if let Some(browser) = BROWSER.get() {
            (browser.sleep)(duration).await;
        }
    }
}

/// Hands the gate the browser's clock, in milliseconds, and a sleep that
/// runs on its event loop.
#[cfg(target_family = "wasm")]
pub fn use_browser_clock(now_ms: fn() -> f64, sleep: clock::Sleep) {
    let _ = clock::BROWSER.set(clock::Browser { now_ms, sleep });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn the_first_call_goes_at_once_and_the_next_waits_a_spacing_after_its_start() {
        let mut next_start = None;
        assert_eq!(
            take_turn(&mut next_start, Duration::ZERO, SECOND),
            Duration::ZERO
        );
        let soon = Duration::from_millis(300);
        assert_eq!(
            take_turn(&mut next_start, soon, SECOND),
            Duration::from_millis(700)
        );
    }

    #[test]
    fn calls_that_queue_up_start_a_spacing_apart() {
        let mut next_start = None;
        let waits: Vec<Duration> = (0..3)
            .map(|_| take_turn(&mut next_start, Duration::ZERO, SECOND))
            .collect();
        assert_eq!(waits, [Duration::ZERO, SECOND, 2 * SECOND]);
    }

    #[test]
    fn a_call_after_a_long_pause_goes_at_once() {
        let mut next_start = None;
        take_turn(&mut next_start, Duration::ZERO, SECOND);
        let later = Duration::from_secs(60);
        assert_eq!(take_turn(&mut next_start, later, SECOND), Duration::ZERO);
        assert_eq!(take_turn(&mut next_start, later, SECOND), SECOND);
    }
}
