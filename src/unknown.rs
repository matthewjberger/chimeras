use crate::backend::Backend;
use crate::camera::Camera;
use crate::error::Error;
use crate::monitor::DeviceMonitor;
use crate::types::{Capabilities, Device, DeviceId, StreamConfig};

pub struct SessionHandle;

impl Drop for SessionHandle {
    fn drop(&mut self) {}
}

pub struct Driver;

impl Backend for Driver {
    type SessionHandle = SessionHandle;

    fn devices() -> Result<Vec<Device>, Error> {
        Err(Error::BackendNotImplemented {
            platform: "unknown",
        })
    }

    fn probe(_id: &DeviceId) -> Result<Capabilities, Error> {
        Err(Error::BackendNotImplemented {
            platform: "unknown",
        })
    }

    fn open(_id: &DeviceId, _config: StreamConfig) -> Result<Camera, Error> {
        Err(Error::BackendNotImplemented {
            platform: "unknown",
        })
    }

    fn monitor() -> Result<DeviceMonitor, Error> {
        Err(Error::BackendNotImplemented {
            platform: "unknown",
        })
    }
}
