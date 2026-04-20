//! egui-side glue for [`cameras::discover`].
//!
//! [`start_discovery`] returns a [`DiscoverySession`]. Call
//! [`poll_discovery`] on it to drain freshly-arrived events (typically once
//! per frame; the underlying channel is unbounded so infrequent polling
//! will not stall the scan). Render results with [`show_discovery`], or
//! call [`show_discovery_status`] and [`show_discovery_results`] separately
//! when you want the status line and the result list in different places
//! in your UI. A click on a result row returns the chosen
//! [`DiscoveredCamera`] so the caller can pass it to
//! [`cameras::open_source`]. Dropping the session (or calling
//! [`cancel_discovery`]) cancels the scan.
//!
//! ```no_run
//! use cameras::discover::DiscoverConfig;
//! use egui_cameras::{poll_discovery, show_discovery, start_discovery};
//!
//! fn ui(ui: &mut egui::Ui, session: &mut Option<egui_cameras::DiscoverySession>) {
//!     if ui.button("Scan").clicked() {
//!         let net: ipnet::IpNet = "192.168.1.0/24".parse().unwrap();
//!         *session = start_discovery(DiscoverConfig {
//!             subnets: vec![net],
//!             ..Default::default()
//!         })
//!         .ok();
//!     }
//!     if let Some(s) = session.as_mut() {
//!         poll_discovery(s);
//!         if let Some(cam) = show_discovery(s, ui) {
//!             let _ = cam;
//!             // hand `cam.source` to cameras::open_source
//!         }
//!     }
//! }
//! ```

use std::net::IpAddr;

use cameras::CameraSource;
use cameras::discover::{
    self, DiscoverConfig, DiscoverEvent, DiscoveredCamera, Discovery, try_next_event,
};
use egui::Ui;

/// Live state of a running [`Discovery`] plus the accumulated results.
///
/// All observable fields are public and plain data: inspect them directly
/// or pass the session to the `show_*` helpers. Not an [`egui::Widget`] —
/// pass to [`show_discovery`] instead. The underlying [`Discovery`] is
/// intentionally private: calling
/// [`cameras::discover::next_event`](::cameras::discover::next_event) on it
/// directly would steal events from [`poll_discovery`]'s drain path and
/// leave the session's fields out of sync with reality.
pub struct DiscoverySession {
    pub(crate) inner: Discovery,
    /// Cameras confirmed so far, in arrival order.
    pub cameras: Vec<DiscoveredCamera>,
    /// Hosts that answered RTSP but did not match a known vendor profile,
    /// each paired with the raw `Server:` header. Useful for diagnosing
    /// "scan finished, nothing found" — a populated list is a strong
    /// signal that a vendor clause is missing.
    pub unmatched_hosts: Vec<(IpAddr, String)>,
    /// Hosts fully probed so far.
    pub scanned: usize,
    /// Hosts the scan intends to visit in total.
    pub total: usize,
    /// `true` once the scan has emitted [`DiscoverEvent::Done`].
    pub done: bool,
}

/// Kick off a discovery scan.
pub fn start_discovery(config: DiscoverConfig) -> Result<DiscoverySession, cameras::Error> {
    let inner = discover::discover(config)?;
    Ok(DiscoverySession {
        inner,
        cameras: Vec::new(),
        unmatched_hosts: Vec::new(),
        scanned: 0,
        total: 0,
        done: false,
    })
}

/// Cancel the scan and release its runtime. Equivalent to dropping the
/// session; exposed for symmetry with [`cameras::discover::cancel`] and for
/// readability at call sites that want to be explicit about intent.
pub fn cancel_discovery(session: DiscoverySession) {
    drop(session);
}

/// Drain every buffered event into the session. Non-blocking. Typically
/// called once per egui frame. Safe to call infrequently or not at all —
/// the underlying [`Discovery`] channel is unbounded, so the scan does not
/// stall on backpressure. Events accumulate until drained or the session
/// is dropped.
pub fn poll_discovery(session: &mut DiscoverySession) {
    while let Some(event) = try_next_event(&session.inner) {
        match event {
            DiscoverEvent::CameraFound(camera) => session.cameras.push(camera),
            DiscoverEvent::HostUnmatched { host, server } => {
                session.unmatched_hosts.push((host, server));
            }
            DiscoverEvent::Progress { scanned, total } => {
                session.scanned = scanned;
                session.total = total;
            }
            DiscoverEvent::Done => {
                session.done = true;
                return;
            }
            DiscoverEvent::HostFound { .. } => {}
            _ => {}
        }
    }
}

/// Render the session's status line and clickable result list into `ui`.
/// Convenience wrapper over [`show_discovery_status`] +
/// [`show_discovery_results`] that renders both parts in sequence. Returns
/// `Some(camera)` when the user clicks a row.
pub fn show_discovery(session: &DiscoverySession, ui: &mut Ui) -> Option<DiscoveredCamera> {
    show_discovery_status(session, ui);
    show_discovery_results(session, ui)
}

/// Render just the progress / status line ("scanned N/M", "scan finished",
/// "scanning..."). Call in a different part of your UI than
/// [`show_discovery_results`] when you want the status separated from the
/// list.
pub fn show_discovery_status(session: &DiscoverySession, ui: &mut Ui) {
    if session.total > 0 {
        ui.label(format!(
            "scanned {}/{}",
            session.scanned.min(session.total),
            session.total
        ));
    } else if session.done {
        ui.label("scan finished");
    } else {
        ui.label("scanning...");
    }
}

/// Render the clickable scrollable result list. Returns `Some(camera)`
/// when the user clicks a row. Suitable for placing in a side panel or a
/// collapsing header separate from the status line.
pub fn show_discovery_results(session: &DiscoverySession, ui: &mut Ui) -> Option<DiscoveredCamera> {
    let mut clicked: Option<DiscoveredCamera> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .max_height(240.0)
        .show(ui, |ui| {
            if session.cameras.is_empty() {
                ui.weak("no cameras yet");
                return;
            }
            for camera in &session.cameras {
                let label = format_camera_row(camera);
                if ui.button(label).clicked() {
                    clicked = Some(camera.clone());
                }
            }
        });
    clicked
}

fn format_camera_row(camera: &DiscoveredCamera) -> String {
    let vendor = camera.vendor.as_deref().unwrap_or("?");
    let channel = match camera.channel {
        Some(channel) => format!("ch{channel}"),
        None => "ch?".to_string(),
    };
    let url = match &camera.source {
        CameraSource::Rtsp { url, .. } => url.as_str(),
        _ => "(non-rtsp)",
    };
    format!("{} [{}] {}  ·  {}", camera.host, vendor, channel, url)
}
