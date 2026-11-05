//! The browser's clock and timer for the gate every ListenBrainz call waits
//! at: std's clock doesn't exist on wasm and there is no runtime to sleep on,
//! only the event loop.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// Hands the gate the browser's clock and timer.
pub(crate) fn install() {
    listenbrainz::use_browser_clock(js_sys::Date::now, sleep);
}

fn sleep(duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(Sleep::start(duration))
}

#[derive(Default)]
struct Wake {
    is_over: bool,
    waker: Option<Waker>,
}

struct Sleep(Rc<RefCell<Wake>>);

impl Sleep {
    fn start(duration: Duration) -> Self {
        let wake = Rc::new(RefCell::new(Wake::default()));
        let timer_wake = wake.clone();
        slint::Timer::single_shot(duration, move || {
            let mut wake = timer_wake.borrow_mut();
            wake.is_over = true;
            if let Some(waker) = wake.waker.take() {
                waker.wake();
            }
        });
        Self(wake)
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let mut wake = self.0.borrow_mut();
        if wake.is_over {
            return Poll::Ready(());
        }
        wake.waker = Some(context.waker().clone());
        Poll::Pending
    }
}
