use crate::backend::Backend;
use crate::error::Error;
use crate::types::{Frame, StreamConfig};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use std::time::Duration;

pub(crate) type PlatformHandle = <crate::ActiveBackend as Backend>::SessionHandle;

pub struct Camera {
    pub config: StreamConfig,
    pub(crate) frame_rx: Receiver<Result<Frame, Error>>,
    #[allow(dead_code)]
    pub(crate) handle: PlatformHandle,
}

pub fn next_frame(camera: &Camera, timeout: Duration) -> Result<Frame, Error> {
    match camera.frame_rx.recv_timeout(timeout) {
        Ok(frame) => frame,
        Err(RecvTimeoutError::Timeout) => Err(Error::Timeout),
        Err(RecvTimeoutError::Disconnected) => Err(Error::StreamEnded),
    }
}

pub fn try_next_frame(camera: &Camera) -> Option<Result<Frame, Error>> {
    camera.frame_rx.try_recv().ok()
}
