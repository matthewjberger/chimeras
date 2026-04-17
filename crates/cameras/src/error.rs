//! Typed error enum for every failure path in the library.

use thiserror::Error;

/// Every error returned by `cameras`.
///
/// Matches on shape, not string text. Each variant carries the context needed to
/// diagnose the failure without having to parse a message.
#[derive(Clone, Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The user or OS denied access to camera hardware.
    #[error("camera permission denied")]
    PermissionDenied,

    /// The supplied [`crate::DeviceId`] did not match any currently-connected device.
    #[error("device not found: {0}")]
    DeviceNotFound(String),

    /// Another application holds exclusive access to the camera.
    #[error("device is already in use")]
    DeviceInUse,

    /// The requested [`crate::StreamConfig`] is not supported by the device.
    #[error("requested format is not supported")]
    FormatNotSupported,

    /// [`crate::next_frame`] did not receive a frame before the timeout expired.
    #[error("timed out waiting for frame")]
    Timeout,

    /// The camera stream ended (for example, the device was unplugged).
    #[error("camera stream ended")]
    StreamEnded,

    /// MJPEG decoding via `zune-jpeg` failed.
    #[error("mjpeg decode failed: {0}")]
    MjpegDecode(String),

    /// The current platform does not yet have a backend implementation.
    #[error("{platform} backend not implemented yet")]
    BackendNotImplemented {
        /// Name of the platform that is missing a backend.
        platform: &'static str,
    },

    /// Catch-all for platform-specific failures (ObjC exceptions, HRESULTs, ioctl errors).
    #[error("{platform}: {message}")]
    Backend {
        /// Name of the platform that raised the error.
        platform: &'static str,
        /// Human-readable message from the underlying platform API.
        message: String,
    },

    /// The requested control or capability is not supported on this platform/device.
    #[error("{platform}: unsupported ({reason})")]
    Unsupported {
        /// Name of the platform reporting the unsupported operation.
        platform: &'static str,
        /// Short identifier for what is unsupported (control field name, missing interface, or scope phrase).
        reason: &'static str,
    },
}
