//! Noticing when the arrangement changes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::{Layout, detect};

/// Calls back whenever the desktop's arrangement changes.
///
/// Polling rather than subscribing, because each source signals changes differently — a
/// D-Bus signal here, a Wayland event there, a RandR event elsewhere — and asking all of
/// them costs one round trip on a local socket. Against that, three separate event loops
/// would be three separate ways to hang. Polls are cheap: no source in this crate touches an
/// I2C bus or wakes a display.
///
/// Dropping the watcher stops the thread.
pub struct LayoutWatcher {
    running: Arc<AtomicBool>,
}

impl LayoutWatcher {
    /// Poll every `interval` and call `on_change` when the layout differs from the last one.
    ///
    /// `on_change` runs on the watcher's thread, so it should hand the layout off rather than
    /// block. It fires as soon as the first reading differs from `initial`, which is how a
    /// caller that starts with an empty layout gets one without blocking its own startup.
    pub fn spawn(
        interval: Duration,
        initial: &Layout,
        mut on_change: impl FnMut(Layout) + Send + 'static,
    ) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let mut last = initial.fingerprint();

        thread::Builder::new()
            .name("layout-watch".into())
            .spawn(move || {
                while flag.load(Ordering::Relaxed) {
                    // Read first, sleep afterwards. Asking a display server for the
                    // arrangement means a round trip that a wedged compositor can hold for
                    // seconds, so it has to happen here rather than on the caller's thread —
                    // and the caller wants the first answer as soon as there is one, not an
                    // interval later.
                    let layout = detect();
                    let fingerprint = layout.fingerprint();
                    if fingerprint != last {
                        last = fingerprint;
                        on_change(layout);
                    }

                    // Slept in slices so dropping the watcher does not wait out a whole
                    // interval before the thread notices it should stop.
                    let mut slept = Duration::ZERO;
                    while slept < interval && flag.load(Ordering::Relaxed) {
                        let slice = interval.min(Duration::from_millis(200));
                        thread::sleep(slice);
                        slept += slice;
                    }
                }
            })
            .expect("spawning the layout watcher thread");

        Self { running }
    }
}

impl Drop for LayoutWatcher {
    fn drop(&mut self) {
        // Flagged but not joined. The thread checks the flag every 200 ms and only ever reads,
        // so leaving it to finish on its own costs nothing — while joining here would make
        // closing a window wait out however long the display server takes to answer, which on
        // a wedged compositor is the whole timeout.
        self.running.store(false, Ordering::Relaxed);
    }
}
