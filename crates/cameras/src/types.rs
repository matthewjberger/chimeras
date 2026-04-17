//! Plain data types describing devices, formats, and frames.

use bytes::Bytes;
use std::time::Duration;

/// Platform-assigned unique identifier for a camera device.
///
/// On macOS this is the device's `uniqueID` from AVFoundation. On Windows it's the
/// Media Foundation symbolic link. On Linux it's a `/dev/video*` path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeviceId(pub String);

/// Logical mounting position of a camera on the host device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Position {
    /// Position is not known.
    Unspecified,
    /// Front-facing camera (user-facing on phones/laptops).
    Front,
    /// Back-facing camera.
    Back,
    /// External USB or virtual camera.
    External,
}

/// Physical transport the camera uses to reach the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transport {
    /// Integrated into the host hardware (e.g. laptop FaceTime camera).
    BuiltIn,
    /// USB UVC / connected via USB.
    Usb,
    /// Software virtual camera (OBS, Continuity, etc.).
    Virtual,
    /// Network camera reached over a protocol such as RTSP.
    Network,
    /// Transport type is unknown or not represented above.
    Other,
}

/// Credentials for authenticating to a network camera.
///
/// Passed to [`crate::open_rtsp`] separately from the URL so the URL can be
/// logged or stored safely without leaking the password.
#[derive(Clone, Debug)]
pub struct Credentials {
    /// Username, plain text.
    pub username: String,
    /// Password, plain text. Be deliberate about where this is stored.
    pub password: String,
}

/// A camera the platform can see. Enumerated by [`crate::devices`].
///
/// All fields are public plain data; copy and mutate as you like.
#[derive(Clone, Debug)]
pub struct Device {
    /// Platform-unique identifier. Pass this back to open or probe the device.
    pub id: DeviceId,
    /// Human-readable device name as reported by the OS.
    pub name: String,
    /// Mounting position, if known.
    pub position: Position,
    /// Transport type, if known.
    pub transport: Transport,
}

/// A pixel resolution in whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resolution {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Concrete pixel layouts that [`Frame::plane_primary`] / `plane_secondary` may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 32-bit BGRA, one plane, alpha ignored for 24-bit conversions.
    Bgra8,
    /// 24-bit packed RGB, one plane.
    Rgb8,
    /// 32-bit RGBA, one plane.
    Rgba8,
    /// 16-bit packed YUY2 (YUYV), one plane, luma + subsampled chroma interleaved.
    Yuyv,
    /// 12-bit bi-planar NV12: Y plane in `plane_primary`, interleaved UV in `plane_secondary`.
    Nv12,
    /// Compressed Motion JPEG. Decodable via [`crate::to_rgb8`] (which uses `zune-jpeg`).
    Mjpeg,
}

/// A closed range of framerates a device format can produce, in frames per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FramerateRange {
    /// Lowest framerate the device will produce in this mode.
    pub min: f64,
    /// Highest framerate the device will produce in this mode.
    pub max: f64,
}

/// A single `(resolution, framerate_range, pixel_format)` combination that a device supports.
///
/// Returned inside [`Capabilities`] from [`crate::probe`].
#[derive(Clone, Debug)]
pub struct FormatDescriptor {
    /// Resolution this entry describes.
    pub resolution: Resolution,
    /// Framerate range this entry supports.
    pub framerate_range: FramerateRange,
    /// Pixel format this entry delivers.
    pub pixel_format: PixelFormat,
}

/// Full list of formats a device supports, as reported by [`crate::probe`].
#[derive(Clone, Debug)]
pub struct Capabilities {
    /// One entry per supported `(resolution, framerate_range, pixel_format)` tuple.
    pub formats: Vec<FormatDescriptor>,
}

/// Requested stream configuration for [`crate::open`].
///
/// Backends do their best to honor every field; the actually-applied configuration
/// is available on [`crate::Camera::config`] once the camera is open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamConfig {
    /// Requested frame size in pixels.
    pub resolution: Resolution,
    /// Requested framerate in frames per second.
    pub framerate: u32,
    /// Requested pixel format of delivered [`Frame`]s.
    pub pixel_format: PixelFormat,
}

/// Indicates whether a delivered frame is pixel-perfect or potentially corrupted.
///
/// Network sources (RTSP) can lose packets and produce frames that reference missing
/// past data, smearing macroblocks until the next keyframe. USB backends always return
/// [`FrameQuality::Intact`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameQuality {
    /// Pixel data is complete and trustworthy.
    Intact,
    /// The decoder recovered from packet loss; pixels may be visibly wrong for a few frames.
    Recovering,
}

/// A single captured video frame.
///
/// Frame data lives in [`Bytes`], which is reference-counted; cloning a `Frame` does not
/// copy the pixel buffer, so you can cheaply fan it out to multiple consumers or hand it
/// to a background thread for encoding.
///
/// For multi-plane formats like NV12, `plane_primary` holds the Y plane and
/// `plane_secondary` holds the interleaved UV plane. Single-plane formats leave
/// `plane_secondary` empty.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Bytes per row of the primary plane (may include alignment padding).
    pub stride: u32,
    /// Presentation timestamp from the capture source. Origin is platform-defined.
    pub timestamp: Duration,
    /// Pixel format that describes both planes.
    pub pixel_format: PixelFormat,
    /// Pixel data integrity. Always [`FrameQuality::Intact`] for USB backends.
    pub quality: FrameQuality,
    /// Primary plane bytes (or the whole frame for single-plane formats).
    pub plane_primary: Bytes,
    /// Secondary plane bytes (UV plane for NV12). Empty for single-plane formats.
    pub plane_secondary: Bytes,
}

/// Axis-aligned rectangle in pixel coordinates.
///
/// Used to describe regions of interest for analysis helpers like
/// [`crate::analysis::blur_variance_in`]. Origin is the top-left of the frame.
#[cfg(feature = "analysis")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left edge in pixels from the frame's left side.
    pub x: u32,
    /// Top edge in pixels from the frame's top.
    pub y: u32,
    /// Width of the rectangle in pixels.
    pub width: u32,
    /// Height of the rectangle in pixels.
    pub height: u32,
}

/// Events emitted by the hotplug [`crate::DeviceMonitor`].
#[derive(Clone, Debug)]
pub enum DeviceEvent {
    /// A new device was discovered (or was present when the monitor started).
    Added(Device),
    /// A previously-known device disappeared.
    Removed(DeviceId),
}
