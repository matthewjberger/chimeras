//! Hotplug monitor that emits [`DeviceEvent`] as cameras appear and disappear.

use crate::error::Error;
use crate::types::DeviceEvent;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;
use std::time::Duration;

/// A running device monitor.
///
/// Obtained from [`crate::monitor`]. Owns a polling worker thread that runs until the
/// monitor is dropped. When dropped, signals the worker to stop and joins it.
pub struct DeviceMonitor {
    pub(crate) event_rx: Receiver<DeviceEvent>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) worker: Option<JoinHandle<()>>,
}

/// Block for the next device event up to `timeout`.
///
/// Returns [`Error::Timeout`] if nothing happened in time (the monitor is still active,
/// try again), or [`Error::StreamEnded`] if the worker has exited.
pub fn next_event(monitor: &DeviceMonitor, timeout: Duration) -> Result<DeviceEvent, Error> {
    match monitor.event_rx.recv_timeout(timeout) {
        Ok(event) => Ok(event),
        Err(RecvTimeoutError::Timeout) => Err(Error::Timeout),
        Err(RecvTimeoutError::Disconnected) => Err(Error::StreamEnded),
    }
}

/// Return the next device event immediately if one is buffered, or `None` otherwise.
pub fn try_next_event(monitor: &DeviceMonitor) -> Option<DeviceEvent> {
    monitor.event_rx.try_recv().ok()
}

impl Drop for DeviceMonitor {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}
