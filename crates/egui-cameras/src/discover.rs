//! egui-side glue for `cameras::discover`.
//!
//! [`start_discovery`] opens a [`DiscoverySession`]; call [`poll_discovery`]
//! each frame to drain any newly-arrived events into the session, then pass
//! it to [`show_discovery`] to render the result list. `show_discovery`
//! returns `Some(camera)` the frame a row is clicked so the caller can open
//! it in their existing RTSP viewer.
use cameras::CameraSource;
use cameras::discover::{
    self, DiscoverConfig, DiscoverEvent, DiscoveredCamera, Discovery, try_next_event,
};
use egui::Ui;

/// Live state of a running [`Discovery`] plus the accumulated results,
/// laid out so a caller can inspect each field without going through a
/// method. Not an [`egui::Widget`]; pass to [`show_discovery`] instead.
pub struct DiscoverySession {
    /// The underlying scan. Drops (and cancels) with the session.
    pub inner: Discovery,
    /// Cameras confirmed so far, in arrival order.
    pub cameras: Vec<DiscoveredCamera>,
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
        scanned: 0,
        total: 0,
        done: false,
    })
}

/// Drain every buffered event into the session. Non-blocking. Typically
/// called once per egui frame. Safe to call infrequently or not at all,
/// the underlying [`cameras::discover::Discovery`] channel is unbounded so
/// the scan does not stall on backpressure. Events accumulate until drained
/// or the session is dropped.
pub fn poll_discovery(session: &mut DiscoverySession) {
    while let Some(event) = try_next_event(&session.inner) {
        match event {
            DiscoverEvent::CameraFound(camera) => session.cameras.push(camera),
            DiscoverEvent::Progress { scanned, total } => {
                session.scanned = scanned;
                session.total = total;
            }
            DiscoverEvent::Done => {
                session.done = true;
                return;
            }
            DiscoverEvent::HostFound { .. } | DiscoverEvent::HostUnmatched { .. } => {}
            _ => {}
        }
    }
}

/// Render the session into `ui`. Returns `Some(camera)` when the user
/// clicks a result row, so the caller can hand it to
/// [`cameras::open_source`].
pub fn show_discovery(session: &DiscoverySession, ui: &mut Ui) -> Option<DiscoveredCamera> {
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
