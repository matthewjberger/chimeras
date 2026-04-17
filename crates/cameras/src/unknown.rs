use crate::backend::Backend;
#[cfg(feature = "controls")]
use crate::backend::BackendControls;
use crate::camera::Camera;
use crate::error::Error;
use crate::monitor::DeviceMonitor;
use crate::types::{Capabilities, Device, DeviceId, StreamConfig};
#[cfg(feature = "controls")]
use crate::types::{ControlCapabilities, Controls};

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

#[cfg(feature = "controls")]
impl BackendControls for Driver {
    fn control_capabilities(_id: &DeviceId) -> Result<ControlCapabilities, Error> {
        Err(Error::Unsupported {
            platform: "unknown",
            reason: "controls unavailable on this target",
        })
    }

    fn read_controls(_id: &DeviceId) -> Result<Controls, Error> {
        Err(Error::Unsupported {
            platform: "unknown",
            reason: "controls unavailable on this target",
        })
    }

    fn apply_controls(_id: &DeviceId, _controls: &Controls) -> Result<(), Error> {
        Err(Error::Unsupported {
            platform: "unknown",
            reason: "controls unavailable on this target",
        })
    }
}
