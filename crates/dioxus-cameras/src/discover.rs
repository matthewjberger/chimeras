use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cameras::Error;
use cameras::discover::{
    self, DiscoverConfig, DiscoverEvent, DiscoveredCamera, Discovery, next_event,
};
use dioxus::prelude::*;

use crate::channel::Channel;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const WORKER_TICK: Duration = Duration::from_millis(100);

/// Handle returned by [`use_discovery`].
///
/// Signals update incrementally as the scan runs. `running` flips to `true`
/// the instant `start` is called and back to `false` when the scan finishes
/// or is cancelled.
#[derive(Copy, Clone, PartialEq)]
pub struct UseDiscovery {
    /// Cameras discovered so far, appended in arrival order.
    pub cameras: Signal<Vec<DiscoveredCamera>>,
    /// Hosts that have completed probing.
    pub scanned: Signal<usize>,
    /// Total hosts being probed in the current scan.
    pub total: Signal<usize>,
    /// `true` while a scan is in flight.
    pub running: Signal<bool>,
    /// Last error from [`cameras::discover::discover`] if `start` failed to
    /// launch the scan (bad subnet cap, tokio runtime build failure). Cleared
    /// to `None` on each successful `start`.
    pub error: Signal<Option<Error>>,
    /// Start a new scan.
    ///
    /// Ignored while a scan is already running. Resets `cameras`, `scanned`,
    /// `total`, and `error` at the start of each fresh scan. If
    /// [`cameras::discover::discover`] returns an error synchronously, the
    /// error is stored in `error` and `running` stays `false`.
    pub start: Callback<DiscoverConfig>,
    /// Cancel the in-flight scan, if any. No-op otherwise.
    pub cancel: Callback<()>,
}

/// Hook that drives a [`Discovery`](cameras::discover::Discovery) and
/// surfaces results as Dioxus signals.
///
/// The scan runs on a dedicated worker thread; the UI thread is never
/// blocked. Clicking `start` on an already-running scan is a no-op, call
/// `cancel` first to stop it.
pub fn use_discovery() -> UseDiscovery {
    let mut cameras = use_signal(Vec::<DiscoveredCamera>::new);
    let mut scanned = use_signal(|| 0usize);
    let mut total = use_signal(|| 0usize);
    let mut running = use_signal(|| false);
    let mut error = use_signal(|| None::<Error>);

    let channel = use_hook(Channel::<DiscoverEvent>::new);
    let handle = use_hook(|| Arc::new(Mutex::new(None::<Discovery>)));
    let scan_id = use_hook(|| Arc::new(AtomicU64::new(0)));

    let poll_channel = channel.clone();
    use_hook(move || {
        spawn(async move {
            loop {
                futures_timer::Delay::new(POLL_INTERVAL).await;
                for event in poll_channel.drain() {
                    match event {
                        DiscoverEvent::CameraFound(camera) => {
                            cameras.write().push(camera);
                        }
                        DiscoverEvent::Progress {
                            scanned: done,
                            total: totals,
                        } => {
                            scanned.set(done);
                            total.set(totals);
                        }
                        DiscoverEvent::Done => {
                            running.set(false);
                        }
                        DiscoverEvent::HostFound { .. } | DiscoverEvent::HostUnmatched { .. } => {}
                        _ => {}
                    }
                }
            }
        })
    });

    let start_tx = channel.sender.clone();
    let start_handle = Arc::clone(&handle);
    let start_scan_id = Arc::clone(&scan_id);
    let start = use_callback(move |config: DiscoverConfig| {
        if *running.peek() {
            return;
        }
        cameras.set(Vec::new());
        scanned.set(0);
        total.set(0);
        error.set(None);
        let discovery = match discover::discover(config) {
            Ok(discovery) => discovery,
            Err(start_error) => {
                error.set(Some(start_error));
                return;
            }
        };
        let id = start_scan_id.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut guard = start_handle.lock().unwrap_or_else(PoisonError::into_inner);
            *guard = Some(discovery);
        }
        running.set(true);
        let tx = start_tx.clone();
        let handle = Arc::clone(&start_handle);
        let current = Arc::clone(&start_scan_id);
        let _ = std::thread::Builder::new()
            .name("cameras-discover-hook".into())
            .spawn(move || {
                run_discovery(handle, tx, id, current);
            });
    });

    let cancel_handle = Arc::clone(&handle);
    let cancel_scan_id = Arc::clone(&scan_id);
    let cancel = use_callback(move |()| {
        cancel_scan_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut guard = cancel_handle.lock().unwrap_or_else(PoisonError::into_inner);
            guard.take();
        }
        running.set(false);
    });

    UseDiscovery {
        cameras,
        scanned,
        total,
        running,
        error,
        start,
        cancel,
    }
}

fn run_discovery(
    handle: Arc<Mutex<Option<Discovery>>>,
    tx: std::sync::mpsc::Sender<DiscoverEvent>,
    id: u64,
    current: Arc<AtomicU64>,
) {
    loop {
        if current.load(Ordering::SeqCst) != id {
            return;
        }
        let event = {
            let guard = handle.lock().unwrap_or_else(PoisonError::into_inner);
            if current.load(Ordering::SeqCst) != id {
                return;
            }
            let Some(disc) = guard.as_ref() else {
                return;
            };
            next_event(disc, WORKER_TICK)
        };
        if current.load(Ordering::SeqCst) != id {
            return;
        }
        match event {
            Ok(DiscoverEvent::Done) => {
                let _ = tx.send(DiscoverEvent::Done);
                let mut guard = handle.lock().unwrap_or_else(PoisonError::into_inner);
                if current.load(Ordering::SeqCst) == id {
                    guard.take();
                }
                return;
            }
            Ok(event) => {
                let _ = tx.send(event);
            }
            Err(cameras::Error::Timeout) => continue,
            Err(_) => {
                let _ = tx.send(DiscoverEvent::Done);
                return;
            }
        }
    }
}
