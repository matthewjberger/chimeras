//! A cross-platform camera library for Rust, built with data-oriented design.
//!
//! `cameras` exposes plain data ([`Device`], [`Capabilities`], [`FormatDescriptor`],
//! [`StreamConfig`], [`Frame`]) and free functions that operate on that data. Every
//! public type has public fields. Format negotiation is explicit: you probe, you pick,
//! you open. Errors are typed. Platform dispatch happens at compile time via `cfg` and
//! the associated-type [`Backend`] trait; there are zero trait objects anywhere in the
//! library.
//!
//! # Platform support
//!
//! | Platform | USB / Built-in | RTSP (`rtsp` feature) |
//! |----------|---------------|-----------------------|
//! | macOS    | AVFoundation via `objc2` | retina + VideoToolbox (H.264 / H.265 / MJPEG) |
//! | Windows  | Media Foundation via `windows` | retina + Media Foundation (H.264 / H.265 / MJPEG) |
//! | Linux    | V4L2 via `v4l` | not supported |
//!
//! # Quick Start
//!
//! ```no_run
//! use std::time::Duration;
//!
//! fn main() -> Result<(), cameras::Error> {
//!     let devices = cameras::devices()?;
//!     let device = devices.first().expect("no cameras");
//!
//!     let capabilities = cameras::probe(device)?;
//!     println!("{} formats available", capabilities.formats.len());
//!
//!     let config = cameras::StreamConfig {
//!         resolution: cameras::Resolution { width: 1280, height: 720 },
//!         framerate: 30,
//!         pixel_format: cameras::PixelFormat::Bgra8,
//!     };
//!
//!     let camera = cameras::open(device, config)?;
//!
//!     for _ in 0..30 {
//!         let frame = cameras::next_frame(&camera, Duration::from_secs(2))?;
//!         let rgb = cameras::to_rgb8(&frame)?;
//!         println!("{}x{} -> {} bytes rgb", frame.width, frame.height, rgb.len());
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! Dropping the [`Camera`] stops the stream. Dropping the [`DeviceMonitor`] joins its
//! polling worker.
//!
//! # Higher-level primitives
//!
//! Two modules layer on top of the [`Camera`] / [`next_frame`] core. They are optional;
//! callers who want full control can stick with the core API.
//!
//! - [`source`]: a [`CameraSource`] enum that unifies USB and RTSP, plus
//!   [`open_source`] which dispatches to [`open`] or `open_rtsp` automatically.
//!   Useful for UIs and config files that want a single "where do frames come from"
//!   value type.
//! - [`pump`]: a long-running background worker that pulls frames and hands them to a
//!   caller-provided sink closure. Supports [`pump::set_active`] (pause / resume without
//!   closing the camera), [`pump::capture_frame`] (single fresh frame on demand, works
//!   while paused), and [`pump::stop_and_join`] (deterministic teardown). This is the
//!   primitive higher-level integrations (for example, the `dioxus-cameras` hook) are
//!   built on.
//! - `analysis` (feature-gated, see [`analysis`]): blur-variance sharpness metrics
//!   and a small [`analysis::Ring`] for "take the sharpest frame of the last N"
//!   capture flows. Scores are relative; calibrate thresholds per camera.
//!
//! # Design
//!
//! - **Data-oriented**: Types hold data. Functions operate on data. No `impl` blocks with
//!   hidden accessors, no trait objects, no inheritance.
//! - **Explicit format negotiation**: [`probe`] returns every format a device supports.
//!   You pick one and pass it to [`open`] via [`StreamConfig`]. If you want a fallback,
//!   [`best_format`] picks the closest match.
//! - **Push-based delivery**: Each [`Camera`] owns a worker thread and a bounded crossbeam
//!   channel. The consumer pulls frames with a timeout via [`next_frame`]. If the consumer
//!   falls behind, old frames are dropped, not buffered.
//! - **Typed errors**: See [`Error`].
//! - **Pluggable pixel conversion**: [`to_rgb8`] / [`to_rgba8`] decode from BGRA, RGBA,
//!   YUYV, NV12, and MJPEG (via `zune-jpeg`), honoring stride.
//! - **Hotplug**: [`monitor()`] returns a [`DeviceMonitor`] that emits
//!   [`DeviceEvent::Added`] / [`DeviceEvent::Removed`] as cameras appear and disappear.
//! - **Unified opening**: [`open_source`] + [`CameraSource`] let you treat USB and RTSP
//!   cameras uniformly in higher-level code.
//! - **Background pump with pause + capture**: [`pump::spawn`] runs the frame loop off
//!   the calling thread, with [`pump::set_active`] for pause / resume and
//!   [`pump::capture_frame`] for single-shot snapshots while paused.
//! - **Compile-time backend contract**: Platform backends are selected with `cfg`. Each is
//!   a `Driver` struct that implements [`Backend`]. No `Box<dyn Backend>`; the compiler
//!   verifies every platform implements the same surface.
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

#[cfg(feature = "analysis")]
pub mod analysis;
pub mod backend;
pub mod camera;
pub mod convert;
pub mod error;
pub mod monitor;
pub mod pump;
pub mod source;
pub mod types;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod unknown;

#[cfg(target_os = "macos")]
pub(crate) type ActiveBackend = crate::macos::Driver;

#[cfg(target_os = "windows")]
pub(crate) type ActiveBackend = crate::windows::Driver;

#[cfg(target_os = "linux")]
pub(crate) type ActiveBackend = crate::linux::Driver;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub(crate) type ActiveBackend = crate::unknown::Driver;

pub use backend::Backend;
#[cfg(feature = "controls")]
pub use backend::BackendControls;
pub use camera::{Camera, next_frame, try_next_frame};
pub use convert::{to_rgb8, to_rgba8};
pub use error::Error;
pub use monitor::{DeviceMonitor, next_event, try_next_event};
pub use source::{CameraSource, open_source, source_label};
#[cfg(feature = "analysis")]
pub use types::Rect;
pub use types::{
    Capabilities, Credentials, Device, DeviceEvent, DeviceId, FormatDescriptor, Frame,
    FrameQuality, FramerateRange, PixelFormat, Position, Resolution, StreamConfig, Transport,
};
#[cfg(feature = "controls")]
pub use types::{
    ControlCapabilities, ControlKind, ControlRange, Controls, PowerLineFrequency,
    PowerLineFrequencyCapability,
};

#[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
pub mod rtsp;
#[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
pub use rtsp::open_rtsp;

#[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
mod decode;

use std::time::Duration;

/// Enumerate every video capture device the platform currently sees.
///
/// On macOS this triggers the system camera permission prompt on first call
/// if it hasn't been granted. On Linux this reads `/dev/video*`. On Windows
/// this queries Media Foundation via `MFEnumDeviceSources`.
pub fn devices() -> Result<Vec<Device>, Error> {
    <ActiveBackend as Backend>::devices()
}

/// Inspect a device's full set of supported formats without opening a stream.
///
/// Returns every native `(resolution, framerate_range, pixel_format)` tuple the
/// device reports. On macOS and Linux this is cheap metadata; on Windows it
/// instantiates a source reader briefly.
pub fn probe(device: &Device) -> Result<Capabilities, Error> {
    <ActiveBackend as Backend>::probe(&device.id)
}

/// Open a camera with the given configuration and start streaming.
///
/// The returned [`Camera`] owns a worker thread that pushes frames into a
/// bounded crossbeam channel. Read them with [`next_frame`] or [`try_next_frame`].
/// Dropping the [`Camera`] stops the stream.
pub fn open(device: &Device, config: StreamConfig) -> Result<Camera, Error> {
    <ActiveBackend as Backend>::open(&device.id, config)
}

/// Start a hotplug monitor.
///
/// Returns a [`DeviceMonitor`] that emits [`DeviceEvent::Added`] / [`DeviceEvent::Removed`]
/// as cameras appear and disappear. Initial events are emitted for every device already
/// present when the monitor starts. Dropping the monitor joins its polling worker.
pub fn monitor() -> Result<DeviceMonitor, Error> {
    <ActiveBackend as Backend>::monitor()
}

/// Pick the closest supported format to a requested `StreamConfig`.
///
/// Tries, in order:
/// 1. An exact match on `(pixel_format, resolution, framerate)`.
/// 2. Any format at the requested resolution.
/// 3. The format whose resolution has the smallest total width + height delta
///    from the request.
///
/// Returns `None` only if `capabilities.formats` is empty.
pub fn best_format(capabilities: &Capabilities, config: &StreamConfig) -> Option<FormatDescriptor> {
    let mut exact = capabilities
        .formats
        .iter()
        .filter(|format| format.pixel_format == config.pixel_format)
        .filter(|format| format.resolution == config.resolution)
        .filter(|format| {
            let fps = config.framerate as f64;
            fps >= format.framerate_range.min && fps <= format.framerate_range.max
        });
    if let Some(format) = exact.next() {
        return Some(format.clone());
    }

    let mut same_resolution = capabilities
        .formats
        .iter()
        .filter(|format| format.resolution == config.resolution);
    if let Some(format) = same_resolution.next() {
        return Some(format.clone());
    }

    capabilities
        .formats
        .iter()
        .min_by_key(|format| {
            let width_delta =
                (format.resolution.width as i64 - config.resolution.width as i64).abs();
            let height_delta =
                (format.resolution.height as i64 - config.resolution.height as i64).abs();
            width_delta + height_delta
        })
        .cloned()
}

/// A reasonable default timeout for [`next_frame`] when you don't want to hand-pick one.
pub const DEFAULT_FRAME_TIMEOUT: Duration = Duration::from_millis(500);

/// Report which runtime controls the given device exposes and their native ranges.
///
/// Fields on the returned [`ControlCapabilities`] are `None` for controls the
/// platform / device does not expose. Ranges are in each platform's native
/// unit — do not assume a normalized scale.
#[cfg(feature = "controls")]
pub fn control_capabilities(device: &Device) -> Result<ControlCapabilities, Error> {
    <ActiveBackend as BackendControls>::control_capabilities(&device.id)
}

/// Read the current value of every exposed control on `device`.
///
/// Fields are `None` for controls the device does not expose. Read-back of
/// `auto_exposure` collapses V4L2 priority modes into `Some(true)`.
#[cfg(feature = "controls")]
pub fn read_controls(device: &Device) -> Result<Controls, Error> {
    <ActiveBackend as BackendControls>::read_controls(&device.id)
}

/// Apply every [`Some`]-valued field in `controls` to `device`.
///
/// `None` fields are left at their current value. Returns the first platform
/// failure encountered; does not preflight against [`control_capabilities`].
#[cfg(feature = "controls")]
pub fn apply_controls(device: &Device, controls: &Controls) -> Result<(), Error> {
    <ActiveBackend as BackendControls>::apply_controls(&device.id, controls)
}
