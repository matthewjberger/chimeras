mod delegate;
mod enumerate;
mod monitor;
mod permission;
mod session;

use crate::backend::Backend;
#[cfg(feature = "controls")]
use crate::backend::BackendControls;
use crate::camera::Camera;
use crate::error::Error;
use crate::monitor::DeviceMonitor;
use crate::types::{Capabilities, Device, DeviceId, StreamConfig};
#[cfg(feature = "controls")]
use crate::types::{ControlCapabilities, Controls};

pub use session::SessionHandle;

pub struct Driver;

impl Backend for Driver {
    type SessionHandle = SessionHandle;

    fn devices() -> Result<Vec<Device>, Error> {
        enumerate::devices()
    }

    fn probe(id: &DeviceId) -> Result<Capabilities, Error> {
        enumerate::probe(id)
    }

    fn open(id: &DeviceId, config: StreamConfig) -> Result<Camera, Error> {
        session::open(id, config)
    }

    fn monitor() -> Result<DeviceMonitor, Error> {
        monitor::monitor()
    }
}

#[cfg(feature = "controls")]
impl BackendControls for Driver {
    fn control_capabilities(_id: &DeviceId) -> Result<ControlCapabilities, Error> {
        Err(Error::Unsupported {
            platform: "macos",
            reason: "controls not yet implemented",
        })
    }

    fn read_controls(_id: &DeviceId) -> Result<Controls, Error> {
        Err(Error::Unsupported {
            platform: "macos",
            reason: "controls not yet implemented",
        })
    }

    fn apply_controls(_id: &DeviceId, _controls: &Controls) -> Result<(), Error> {
        Err(Error::Unsupported {
            platform: "macos",
            reason: "controls not yet implemented",
        })
    }
}
