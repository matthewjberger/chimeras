//! The [`Camera`] resource handle and frame-reading free functions.

use crate::backend::Backend;
use crate::error::Error;
use crate::types::{Frame, StreamConfig};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use std::time::Duration;

pub(crate) type PlatformHandle = <crate::ActiveBackend as Backend>::SessionHandle;

/// An open, streaming camera.
///
/// Obtained from [`crate::open`]. Holds a worker thread that continuously pushes frames
/// into a bounded channel; read them with [`next_frame`] or [`try_next_frame`]. Dropping
/// the `Camera` stops the stream and releases the underlying OS session.
pub struct Camera {
    /// The configuration that is actually applied to the hardware.
    ///
    /// May differ from what was passed to [`crate::open`] if the platform had to round
    /// the resolution or framerate to the nearest supported value. This is the single
    /// source of truth for frame dimensions and pixel format.
    pub config: StreamConfig,
    pub(crate) frame_rx: Receiver<Result<Frame, Error>>,
    #[allow(dead_code)]
    pub(crate) handle: PlatformHandle,
}

/// Block for the next frame up to `timeout`.
///
/// Returns [`Error::Timeout`] if no frame arrives in time (the camera is still open,
/// try again), or [`Error::StreamEnded`] if the worker thread has exited.
pub fn next_frame(camera: &Camera, timeout: Duration) -> Result<Frame, Error> {
    match camera.frame_rx.recv_timeout(timeout) {
        Ok(frame) => frame,
        Err(RecvTimeoutError::Timeout) => Err(Error::Timeout),
        Err(RecvTimeoutError::Disconnected) => Err(Error::StreamEnded),
    }
}

/// Return immediately with a frame if one is already buffered, or `None` otherwise.
///
/// Useful when you want to poll the camera from a render loop without ever blocking.
pub fn try_next_frame(camera: &Camera) -> Option<Result<Frame, Error>> {
    camera.frame_rx.try_recv().ok()
}
