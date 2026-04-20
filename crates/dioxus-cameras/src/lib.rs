//! Dioxus integration for the [`cameras`] crate.
//!
//! This crate owns only the Dioxus-specific glue, the HTTP preview server,
//! the [`Registry`] backing it, the `<canvas>`-side WebGL2 renderer, and the
//! hooks consumers plug into their components. Every other primitive
//! (single-frame capture, pause/resume pump, source abstraction) lives
//! upstream in [`cameras`] itself so non-Dioxus callers can use it too.
//!
//! # What's here
//!
//! - [`PreviewServer`] + [`start_preview_server`] + [`register_with`], a
//!   loopback HTTP server that publishes the latest [`Frame`](cameras::Frame)
//!   for each stream id over `/preview/{id}.bin`. The listener thread is torn
//!   down when the last [`PreviewServer`] clone drops.
//! - [`Registry`] + [`LatestFrame`], shared map of stream id → latest frame.
//!   The server reads from it; pumps publish to it. Cleaned up automatically
//!   when the owning component unmounts.
//! - [`use_camera_stream`], high-level hook returning [`UseCameraStream`]:
//!   a status signal, an active/paused toggle, and a single-frame `capture_frame`
//!   callback. Wraps [`cameras::pump`] under the hood.
//! - [`use_devices`] / [`use_streams`], hooks for the camera list and
//!   multi-stream id management.
//! - [`PreviewScript`] + [`StreamPreview`], components that render live
//!   frames into a `<canvas>` via WebGL2 (NV12, BGRA, or RGBA shaders).
//!
//! # Wiring a Dioxus app
//!
//! ```no_run
//! use dioxus_cameras::cameras::{self, CameraSource, PixelFormat, Resolution, StreamConfig};
//! use dioxus::prelude::*;
//! use dioxus_cameras::{PreviewScript, StreamPreview, register_with, start_preview_server, use_camera_stream};
//!
//! fn main() {
//!     let server = start_preview_server().expect("preview server");
//!     register_with(&server, dioxus::LaunchBuilder::desktop()).launch(app);
//! }
//!
//! fn app() -> Element {
//!     let source = use_signal::<Option<CameraSource>>(|| None);
//!     let config = StreamConfig {
//!         resolution: Resolution { width: 1280, height: 720 },
//!         framerate: 30,
//!         pixel_format: PixelFormat::Bgra8,
//!     };
//!     let stream = use_camera_stream(0, source, config);
//!     rsx! {
//!         StreamPreview { id: 0 }
//!         p { "{stream.status}" }
//!         button {
//!             onclick: move |_| stream.active.clone().set(!*stream.active.read()),
//!             "Toggle preview"
//!         }
//!         button {
//!             onclick: move |_| { let _ = stream.capture_frame.call(()); },
//!             "Take picture"
//!         }
//!         PreviewScript {}
//!     }
//! }
//! ```

#![warn(missing_docs)]

pub use cameras;

mod camera_stream;
mod channel;
mod component;
mod devices;
mod poison;
mod registry;
mod server;
mod streams;

#[cfg(all(feature = "discover", any(target_os = "macos", target_os = "windows")))]
mod discover;

pub use camera_stream::{StreamStatus, UseCameraStream, use_camera_stream};
pub use component::{PreviewScript, StreamPreview};
pub use devices::{UseDevices, use_devices};
pub use registry::{LatestFrame, Registry, get_or_create_sink, publish_frame, remove_sink};
pub use server::{PreviewServer, register_with, start_preview_server};
pub use streams::{UseStreams, use_streams};

#[cfg(all(feature = "discover", any(target_os = "macos", target_os = "windows")))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(feature = "discover", any(target_os = "macos", target_os = "windows"))))
)]
pub use discover::{UseDiscovery, use_discovery};

/// The JavaScript blob that drives the WebGL2 preview renderer.
///
/// Usually you want the [`PreviewScript`] component instead, it injects this
/// blob into a `<script>` tag with the right attributes. This raw constant is
/// exposed for users rendering outside a Dioxus component (for example, in a
/// custom server-rendered template).
///
/// The script scans the DOM for `canvas[data-stream-id]` elements and binds
/// each one to the URL in its `data-preview-url` attribute. The
/// [`StreamPreview`] component emits canvases that match this contract.
pub const PREVIEW_JS: &str = include_str!("../assets/preview.js");
