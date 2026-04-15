use thiserror::Error;

#[derive(Clone, Debug, Error)]
pub enum Error {
    #[error("camera permission denied")]
    PermissionDenied,

    #[error("device not found: {0}")]
    DeviceNotFound(String),

    #[error("device is already in use")]
    DeviceInUse,

    #[error("requested format is not supported")]
    FormatNotSupported,

    #[error("timed out waiting for frame")]
    Timeout,

    #[error("camera stream ended")]
    StreamEnded,

    #[error("mjpeg decode failed: {0}")]
    MjpegDecode(String),

    #[error("{platform} backend not implemented yet")]
    BackendNotImplemented { platform: &'static str },

    #[error("{platform}: {message}")]
    Backend {
        platform: &'static str,
        message: String,
    },
}
