//! egui / eframe integration for the [`cameras`] crate.
//!
//! This crate owns only the thin glue between a running
//! [`cameras::pump::Pump`] and an [`egui::TextureHandle`]. Every camera
//! primitive (pause / resume, single-frame capture, hotplug, source
//! abstraction) lives upstream in [`cameras`] itself and is re-exported here
//! for convenience.
//!
//! # Wiring an eframe app
//!
//! ```ignore
//! use egui_cameras::cameras::{self, PixelFormat, Resolution, StreamConfig};
//! use eframe::egui;
//!
//! struct App {
//!     stream: egui_cameras::Stream,
//! }
//!
//! impl eframe::App for App {
//!     fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
//!         let ctx = ui.ctx().clone();
//!         egui_cameras::update_texture(&mut self.stream, &ctx).ok();
//!         egui::CentralPanel::default().show_inside(ui, |ui| {
//!             egui_cameras::show(&self.stream, ui);
//!         });
//!         ctx.request_repaint();
//!     }
//! }
//! ```
//!
//! The doctest is `ignore`d because it uses `eframe`, which `egui-cameras`
//! does not depend on. The `apps/egui-demo` app in this repo is the
//! runnable version.

#![warn(missing_docs)]

pub use cameras;

#[cfg(all(feature = "discover", any(target_os = "macos", target_os = "windows")))]
mod discover;

#[cfg(all(feature = "discover", any(target_os = "macos", target_os = "windows")))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(feature = "discover", any(target_os = "macos", target_os = "windows"))))
)]
pub use discover::{DiscoverySession, poll_discovery, show_discovery, start_discovery};

use std::sync::{Arc, Mutex, PoisonError};

use cameras::{Camera, Frame, pump};
use egui::{ColorImage, Context, TextureHandle, TextureOptions, Ui};

pub use cameras::pump::{Pump, capture_frame, set_active, stop_and_join};

const DEFAULT_TEXTURE_NAME: &str = "cameras-frame";

/// A running camera pump plus an [`egui::TextureHandle`] that is refreshed
/// each time [`update_texture`] is called.
///
/// Obtained from [`spawn`]. All fields are public, data-oriented, no methods.
pub struct Stream {
    /// The underlying pump. Pass by reference to [`set_active`],
    /// [`capture_frame`], or [`stop_and_join`] to drive the pump.
    pub pump: Pump,
    /// Shared slot the pump writes each frame into. Cleared by
    /// [`update_texture`] once the frame is uploaded.
    pub sink: Sink,
    /// The egui texture the frame is uploaded to. `None` until the first
    /// frame arrives.
    pub texture: Option<TextureHandle>,
    /// Name the texture is registered under in egui's texture cache.
    pub name: String,
}

/// Shared slot that a [`Pump`] writes each frame into.
///
/// Cheap to clone: it holds a single `Arc`.
#[derive(Clone, Default)]
pub struct Sink {
    frame: Arc<Mutex<Option<Frame>>>,
}

/// Spawn a pump that feeds a fresh [`Stream`] backed by a default-named
/// texture. The returned [`Stream`] is in the active state.
pub fn spawn(camera: Camera) -> Stream {
    spawn_named(camera, DEFAULT_TEXTURE_NAME)
}

/// Like [`spawn`], but lets you name the egui texture. Useful when you have
/// more than one concurrent camera stream in the same app.
pub fn spawn_named(camera: Camera, name: impl Into<String>) -> Stream {
    let sink = Sink::default();
    let pump = spawn_pump(camera, sink.clone());
    Stream {
        pump,
        sink,
        texture: None,
        name: name.into(),
    }
}

/// Spawn a [`Pump`] that writes each incoming frame into `sink`. Use this
/// when you want to manage the [`Sink`] and [`TextureHandle`] yourself
/// instead of bundling them in a [`Stream`].
pub fn spawn_pump(camera: Camera, sink: Sink) -> Pump {
    pump::spawn(camera, move |frame| publish_frame(&sink, frame))
}

/// Publish `frame` to `sink`, replacing any previous frame.
pub fn publish_frame(sink: &Sink, frame: Frame) {
    let mut slot = sink.frame.lock().unwrap_or_else(PoisonError::into_inner);
    *slot = Some(frame);
}

/// Take the latest frame out of `sink`, returning `None` if no frame has
/// arrived since the last call.
pub fn take_frame(sink: &Sink) -> Option<Frame> {
    sink.frame
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

/// Convert a cameras [`Frame`] into an egui [`ColorImage`] (RGBA8).
pub fn frame_to_color_image(frame: &Frame) -> Result<ColorImage, cameras::Error> {
    let rgba = cameras::to_rgba8(frame)?;
    Ok(ColorImage::from_rgba_unmultiplied(
        [frame.width as usize, frame.height as usize],
        &rgba,
    ))
}

/// Upload the latest frame on `stream`'s [`Sink`] to its [`TextureHandle`].
///
/// Returns `Ok(true)` if a new frame was uploaded this call, `Ok(false)` if
/// no new frame was waiting, or an error if pixel conversion failed. Call
/// once per frame (typically at the top of your eframe `update` method).
pub fn update_texture(stream: &mut Stream, ctx: &Context) -> Result<bool, cameras::Error> {
    let Some(frame) = take_frame(&stream.sink) else {
        return Ok(false);
    };
    let image = frame_to_color_image(&frame)?;
    match &mut stream.texture {
        Some(texture) => texture.set(image, TextureOptions::LINEAR),
        None => {
            stream.texture = Some(ctx.load_texture(&stream.name, image, TextureOptions::LINEAR));
        }
    }
    Ok(true)
}

/// Draw the stream's texture into `ui` as a sized [`egui::Image`] that
/// fills the available width while preserving aspect ratio.
///
/// No-op if no frame has arrived yet (the texture is still `None`).
pub fn show(stream: &Stream, ui: &mut Ui) {
    let Some(texture) = &stream.texture else {
        return;
    };
    let aspect = texture.aspect_ratio();
    let available = ui.available_size();
    let width = available.x.min(available.y * aspect);
    let height = width / aspect;
    ui.image((texture.id(), egui::vec2(width, height)));
}
