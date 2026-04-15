mod delegate;
mod enumerate;
mod monitor;
mod permission;
mod session;

use crate::backend::Backend;
use crate::camera::Camera;
use crate::error::Error;
use crate::monitor::DeviceMonitor;
use crate::types::{Capabilities, Device, DeviceId, StreamConfig};

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
