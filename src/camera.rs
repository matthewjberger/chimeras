//! The [`Camera`] resource handle and frame-reading free functions.

use crate::backend::Backend;
use crate::error::Error;
use crate::types::{Frame, StreamConfig};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use std::time::Duration;

pub(crate) type NativeHandle = <crate::ActiveBackend as Backend>::SessionHandle;

/// Opaque handle kept inside [`Camera`] so the session tears down on drop.
///
/// Variant payloads are held purely so their own `Drop` impls fire when the `Camera`
/// is dropped; nothing reads them. The `expect(dead_code)` markers are assertions
/// rather than silences: if any variant ever becomes actually-read, the lint will
/// error and the marker can be removed.
pub(crate) enum Handle {
    /// Handle from an OS camera backend (AVFoundation, Media Foundation, V4L2).
    Native(#[expect(dead_code)] NativeHandle),
    /// Handle from the RTSP network backend.
    #[cfg(feature = "rtsp")]
    Rtsp(#[expect(dead_code)] crate::rtsp::SessionHandle),
}

/// An open, streaming camera.
///
/// Obtained from [`crate::open`] for USB cameras or [`crate::open_rtsp`] for network
/// cameras. Holds a worker that continuously pushes frames into a bounded channel; read
/// them with [`next_frame`] or [`try_next_frame`]. Dropping the `Camera` stops the
/// stream and releases the underlying session.
pub struct Camera {
    /// The configuration actually applied to the stream.
    ///
    /// May differ from what was passed to [`crate::open`] / [`crate::open_rtsp`] if the
    /// source rounded the resolution or framerate.
    pub config: StreamConfig,
    pub(crate) frame_rx: Receiver<Result<Frame, Error>>,
    #[expect(dead_code)]
    pub(crate) handle: Handle,
}

/// Block for the next frame up to `timeout`.
///
/// Returns [`Error::Timeout`] if no frame arrives in time (the camera is still open,
/// try again), or [`Error::StreamEnded`] if the worker has exited.
pub fn next_frame(camera: &Camera, timeout: Duration) -> Result<Frame, Error> {
    match camera.frame_rx.recv_timeout(timeout) {
        Ok(frame) => frame,
        Err(RecvTimeoutError::Timeout) => Err(Error::Timeout),
        Err(RecvTimeoutError::Disconnected) => Err(Error::StreamEnded),
    }
}

/// Return immediately with a frame if one is already buffered, or `None` otherwise.
pub fn try_next_frame(camera: &Camera) -> Option<Result<Frame, Error>> {
    camera.frame_rx.try_recv().ok()
}
