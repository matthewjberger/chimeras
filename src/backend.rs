//! Compile-time contract every platform backend must implement.
//!
//! Chimeras never holds a `Box<dyn Backend>`; instead the active backend is selected at
//! compile time via a `cfg`-gated type alias. The trait exists only so the compiler can
//! verify that every platform module implements the same surface: adding a method here
//! forces every backend to provide it.

use crate::camera::Camera;
use crate::error::Error;
use crate::monitor::DeviceMonitor;
use crate::types::{Capabilities, Device, DeviceId, StreamConfig};

/// The contract every platform backend implements.
///
/// Users should not consume this trait directly; call the free functions at the crate
/// root ([`crate::devices`], [`crate::probe`], [`crate::open`], [`crate::monitor`])
/// which dispatch through the active backend.
pub trait Backend {
    /// Opaque platform-specific handle stored inside [`Camera`] to keep the OS session
    /// alive while the camera is open. Its `Drop` impl shuts the session down.
    type SessionHandle;

    /// List every video capture device currently visible to the platform.
    fn devices() -> Result<Vec<Device>, Error>;
    /// Return every format the given device supports.
    fn probe(id: &DeviceId) -> Result<Capabilities, Error>;
    /// Open a streaming session on the given device with the given configuration.
    fn open(id: &DeviceId, config: StreamConfig) -> Result<Camera, Error>;
    /// Start a hotplug monitor.
    fn monitor() -> Result<DeviceMonitor, Error>;
}
