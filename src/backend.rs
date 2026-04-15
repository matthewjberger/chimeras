use crate::camera::Camera;
use crate::error::Error;
use crate::monitor::DeviceMonitor;
use crate::types::{Capabilities, Device, DeviceId, StreamConfig};

pub trait Backend {
    type SessionHandle;

    fn devices() -> Result<Vec<Device>, Error>;
    fn probe(id: &DeviceId) -> Result<Capabilities, Error>;
    fn open(id: &DeviceId, config: StreamConfig) -> Result<Camera, Error>;
    fn monitor() -> Result<DeviceMonitor, Error>;
}
