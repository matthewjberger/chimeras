use bytes::Bytes;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeviceId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Position {
    Unspecified,
    Front,
    Back,
    External,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transport {
    BuiltIn,
    Usb,
    Virtual,
    Other,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub position: Position,
    pub transport: Transport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    Bgra8,
    Rgb8,
    Rgba8,
    Yuyv,
    Nv12,
    Mjpeg,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FramerateRange {
    pub min: f64,
    pub max: f64,
}

#[derive(Clone, Debug)]
pub struct FormatDescriptor {
    pub resolution: Resolution,
    pub framerate_range: FramerateRange,
    pub pixel_format: PixelFormat,
}

#[derive(Clone, Debug)]
pub struct Capabilities {
    pub formats: Vec<FormatDescriptor>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamConfig {
    pub resolution: Resolution,
    pub framerate: u32,
    pub pixel_format: PixelFormat,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub timestamp: Duration,
    pub pixel_format: PixelFormat,
    pub plane_primary: Bytes,
    pub plane_secondary: Bytes,
}

#[derive(Clone, Debug)]
pub enum DeviceEvent {
    Added(Device),
    Removed(DeviceId),
}
