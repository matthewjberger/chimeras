#![deny(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod camera;
pub mod convert;
pub mod error;
pub mod monitor;
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
pub use camera::{Camera, next_frame, try_next_frame};
pub use convert::{to_rgb8, to_rgba8};
pub use error::Error;
pub use monitor::{DeviceMonitor, next_event, try_next_event};
pub use types::{
    Capabilities, Device, DeviceEvent, DeviceId, FormatDescriptor, Frame, FramerateRange,
    PixelFormat, Position, Resolution, StreamConfig, Transport,
};

use std::time::Duration;

pub fn devices() -> Result<Vec<Device>, Error> {
    <ActiveBackend as Backend>::devices()
}

pub fn probe(device: &Device) -> Result<Capabilities, Error> {
    <ActiveBackend as Backend>::probe(&device.id)
}

pub fn open(device: &Device, config: StreamConfig) -> Result<Camera, Error> {
    <ActiveBackend as Backend>::open(&device.id, config)
}

pub fn monitor() -> Result<DeviceMonitor, Error> {
    <ActiveBackend as Backend>::monitor()
}

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

pub const DEFAULT_FRAME_TIMEOUT: Duration = Duration::from_millis(500);
