//! Dioxus hook that drives a [`cameras::discover::Discovery`] and surfaces
//! its results as Dioxus signals.
//!
//! The scan runs on a dedicated worker thread; the UI thread is never
//! blocked. Events flow through an internal 50ms-tick poll task into the
//! signals exposed by [`UseDiscovery`].
//!
//! ```no_run
//! use dioxus::prelude::*;
//! use dioxus_cameras::cameras::discover::DiscoverConfig;
//! use dioxus_cameras::use_discovery;
//!
//! fn DiscoverPanel() -> Element {
//!     let discovery = use_discovery();
//!     let running = *discovery.running.read();
//!     let cameras = discovery.cameras.read().len();
//!     rsx! {
//!         // `start` auto-cancels any in-flight scan, so the button is
//!         // always pressable; the label changes to communicate intent.
//!         button {
//!             onclick: move |_| {
//!                 discovery.start.call(DiscoverConfig {
//!                     endpoints: vec!["127.0.0.1:554".parse().unwrap()],
//!                     ..Default::default()
//!                 });
//!             },
//!             if running { "Restart scan" } else { "Scan" }
//!         }
//!         button {
//!             disabled: !running,
//!             onclick: move |_| discovery.cancel.call(()),
//!             "Cancel"
//!         }
//!         button {
//!             disabled: running,
//!             onclick: move |_| discovery.clear.call(()),
//!             "Clear"
//!         }
//!         p { "{cameras} cameras found" }
//!     }
//! }
//! ```

use std::net::IpAddr;
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
    /// Hosts that answered RTSP but did not match a known vendor profile,
    /// each paired with the raw `Server:` header string. Useful for
    /// diagnosing "scan finished, nothing found" — a populated list is a
    /// strong signal that a vendor clause is missing.
    pub unmatched_hosts: Signal<Vec<(IpAddr, String)>>,
    /// Hosts that have completed probing.
    pub scanned: Signal<usize>,
    /// Total hosts being probed in the current scan.
    pub total: Signal<usize>,
    /// `true` while a scan is in flight.
    pub running: Signal<bool>,
    /// Last error encountered while trying to launch a scan. Covers
    /// synchronous [`cameras::discover::discover`] failures (bad subnet
    /// cap, tokio runtime build failure) and worker-thread spawn failures.
    /// Cleared to `None` on each successful `start`. Mid-scan errors do
    /// not exist: the library's scan task either runs to completion or is
    /// cancelled, so there is no other error path to surface here.
    pub error: Signal<Option<Error>>,
    /// Start a new scan.
    ///
    /// If a scan is already running, it is cancelled synchronously before
    /// the new one starts; there is no silent no-op. All result signals
    /// (`cameras`, `unmatched_hosts`, `scanned`, `total`, `error`) are
    /// reset at the start of each fresh scan.
    pub start: Callback<DiscoverConfig>,
    /// Cancel the in-flight scan, if any. No-op otherwise.
    pub cancel: Callback<()>,
    /// Reset all result signals and cancel any in-flight scan. Useful for
    /// "clear the screen" flows between runs.
    pub clear: Callback<()>,
}

/// Drive a [`cameras::discover::Discovery`] from a Dioxus component.
///
/// See the module-level example and the handle docs on [`UseDiscovery`] for
/// usage.
pub fn use_discovery() -> UseDiscovery {
    let mut cameras = use_signal(Vec::<DiscoveredCamera>::new);
    let mut unmatched_hosts = use_signal(Vec::<(IpAddr, String)>::new);
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
                        DiscoverEvent::HostUnmatched { host, server } => {
                            unmatched_hosts.write().push((host, server));
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
                        DiscoverEvent::HostFound { .. } => {}
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
        // Always bump the generation first: any running worker's id is now
        // stale, and will fail both the forward-event and loop-iteration
        // checks inside run_discovery. This makes start auto-cancel-and-
        // restart rather than silently no-op.
        let id = start_scan_id.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut guard = start_handle.lock().unwrap_or_else(PoisonError::into_inner);
            guard.take();
        }

        cameras.set(Vec::new());
        unmatched_hosts.set(Vec::new());
        scanned.set(0);
        total.set(0);
        error.set(None);

        let discovery = match discover::discover(config) {
            Ok(discovery) => discovery,
            Err(start_error) => {
                error.set(Some(start_error));
                running.set(false);
                return;
            }
        };

        {
            let mut guard = start_handle.lock().unwrap_or_else(PoisonError::into_inner);
            *guard = Some(discovery);
        }
        running.set(true);

        let tx = start_tx.clone();
        let handle = Arc::clone(&start_handle);
        let current = Arc::clone(&start_scan_id);
        let spawn_result = std::thread::Builder::new()
            .name("cameras-discover-hook".into())
            .spawn(move || {
                run_discovery(handle, tx, id, current);
            });
        if let Err(spawn_error) = spawn_result {
            // OS out of resources for threads, or similar. Unwind: pull the
            // Discovery we just installed so it doesn't sit there undriven,
            // surface the error, and flip running back to false.
            let mut guard = start_handle.lock().unwrap_or_else(PoisonError::into_inner);
            guard.take();
            error.set(Some(Error::Backend {
                platform: "discover",
                message: format!("worker thread spawn: {spawn_error}"),
            }));
            running.set(false);
        }
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

    let clear_handle = Arc::clone(&handle);
    let clear_scan_id = Arc::clone(&scan_id);
    let clear = use_callback(move |()| {
        clear_scan_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut guard = clear_handle.lock().unwrap_or_else(PoisonError::into_inner);
            guard.take();
        }
        cameras.set(Vec::new());
        unmatched_hosts.set(Vec::new());
        scanned.set(0);
        total.set(0);
        error.set(None);
        running.set(false);
    });

    UseDiscovery {
        cameras,
        unmatched_hosts,
        scanned,
        total,
        running,
        error,
        start,
        cancel,
        clear,
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
        // Gate each send on a fresh gen check. Keeping the check adjacent
        // to the send closes the TOCTOU window where UI-thread `start` /
        // `cancel` / `clear` could bump gen between "we observed this
        // event" and "we forwarded it to the poll task", which would let
        // a stale worker's event pollute the next scan's signals.
        match event {
            Ok(DiscoverEvent::Done) => {
                if current.load(Ordering::SeqCst) == id {
                    let _ = tx.send(DiscoverEvent::Done);
                    let mut guard = handle.lock().unwrap_or_else(PoisonError::into_inner);
                    guard.take();
                }
                return;
            }
            Ok(event) => {
                if current.load(Ordering::SeqCst) == id {
                    let _ = tx.send(event);
                }
            }
            Err(cameras::Error::Timeout) => continue,
            Err(_) => {
                if current.load(Ordering::SeqCst) == id {
                    let _ = tx.send(DiscoverEvent::Done);
                }
                return;
            }
        }
    }
}
