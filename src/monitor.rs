use crate::error::Error;
use crate::types::DeviceEvent;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;
use std::time::Duration;

pub struct DeviceMonitor {
    pub(crate) event_rx: Receiver<DeviceEvent>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) worker: Option<JoinHandle<()>>,
}

pub fn next_event(monitor: &DeviceMonitor, timeout: Duration) -> Result<DeviceEvent, Error> {
    match monitor.event_rx.recv_timeout(timeout) {
        Ok(event) => Ok(event),
        Err(RecvTimeoutError::Timeout) => Err(Error::Timeout),
        Err(RecvTimeoutError::Disconnected) => Err(Error::StreamEnded),
    }
}

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
