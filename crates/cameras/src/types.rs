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
#[non_exhaustive]
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
#[non_exhaustive]
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
/// Passed to `crate::open_rtsp` separately from the URL so the URL can be
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub enum DeviceEvent {
    /// A new device was discovered (or was present when the monitor started).
    Added(Device),
    /// A previously-known device disappeared.
    Removed(DeviceId),
}

/// AC mains frequency choice for cameras that support power-line-frequency filtering.
///
/// Supported on Linux via `V4L2_CID_POWER_LINE_FREQUENCY` and on Windows via
/// `IAMVideoProcAmp`'s `VideoProcAmp_PowerLineFrequency` property (id `10`).
/// macOS reports [`None`] for this capability — AVFoundation does not expose it.
#[cfg(feature = "controls")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PowerLineFrequency {
    /// Flicker-suppression disabled.
    Disabled,
    /// 50 Hz mains.
    Hz50,
    /// 60 Hz mains.
    Hz60,
    /// Hardware auto-detects mains frequency.
    Auto,
}

/// Requested tweaks to a device's runtime controls.
///
/// Each field uses [`Option::None`] to mean "leave the current value alone"
/// and [`Option::Some`] to mean "apply this value." Values are in each
/// platform's native range; consult [`ControlCapabilities`] for the exact
/// endpoints before writing.
///
/// Platforms reject out-of-range or unsupported writes with
/// [`crate::Error::Unsupported`].
#[cfg(feature = "controls")]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Controls {
    /// Manual focus position. See [`ControlCapabilities::focus`] for range semantics.
    pub focus: Option<f32>,
    /// Enable (`true`) or disable (`false`) continuous auto-focus.
    pub auto_focus: Option<bool>,
    /// Manual exposure value in each platform's native unit (seconds on macOS, microseconds on Linux).
    pub exposure: Option<f32>,
    /// Enable (`true`) or disable (`false`) auto-exposure. Read-back collapses V4L2 priority modes (shutter/aperture priority) into `Some(true)`; write-back of `Some(true)` applies full AUTO (value 0).
    pub auto_exposure: Option<bool>,
    /// Manual white-balance temperature (Kelvin on Linux, synthesized via gains round-trip on macOS).
    pub white_balance_temperature: Option<f32>,
    /// Enable (`true`) or disable (`false`) auto white balance.
    pub auto_white_balance: Option<bool>,
    /// Image brightness in native units.
    pub brightness: Option<f32>,
    /// Image contrast in native units.
    pub contrast: Option<f32>,
    /// Image saturation in native units.
    pub saturation: Option<f32>,
    /// Image sharpness in native units.
    pub sharpness: Option<f32>,
    /// Sensor gain in native units (ISO on macOS).
    pub gain: Option<f32>,
    /// Backlight compensation in native units.
    pub backlight_compensation: Option<f32>,
    /// AC power-line frequency for flicker suppression.
    pub power_line_frequency: Option<PowerLineFrequency>,
    /// Pan axis in native units. PTZ-capable devices only.
    pub pan: Option<f32>,
    /// Tilt axis in native units. PTZ-capable devices only.
    pub tilt: Option<f32>,
    /// Zoom factor in native units. PTZ-capable devices only.
    pub zoom: Option<f32>,
}

/// Reported range for one numeric camera control.
///
/// All fields are in the platform's native unit for the control — do not
/// assume a normalized 0..1 scale. Read endpoints from this struct before
/// constructing [`Controls`] values.
#[cfg(feature = "controls")]
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ControlRange {
    /// Minimum accepted value, inclusive.
    pub min: f32,
    /// Maximum accepted value, inclusive.
    pub max: f32,
    /// Smallest step between accepted values. `0.0` means continuous.
    pub step: f32,
    /// Factory default value.
    pub default: f32,
}

/// Power-line-frequency capability detail on devices that expose it.
#[cfg(feature = "controls")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct PowerLineFrequencyCapability {
    /// `true` if 50 Hz filtering is selectable on this device.
    pub hz50: bool,
    /// `true` if 60 Hz filtering is selectable on this device.
    pub hz60: bool,
    /// `true` if the "off" mode is selectable on this device.
    pub disabled: bool,
    /// `true` if hardware auto-detect mode is selectable on this device.
    pub auto: bool,
    /// Factory default mode.
    pub default: PowerLineFrequency,
}

/// Identifier for every control field on [`Controls`] and [`ControlCapabilities`].
///
/// Useful for UI iteration, config serialization, and fetching platform-scoped
/// caveats via [`ControlKind::caveat`]. Iterate [`ControlKind::ALL`] to visit
/// every control in a stable order.
#[cfg(feature = "controls")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ControlKind {
    /// Manual focus position.
    Focus,
    /// Auto-focus toggle.
    AutoFocus,
    /// Manual exposure value.
    Exposure,
    /// Auto-exposure toggle.
    AutoExposure,
    /// Manual white-balance temperature.
    WhiteBalanceTemperature,
    /// Auto-white-balance toggle.
    AutoWhiteBalance,
    /// Image brightness.
    Brightness,
    /// Image contrast.
    Contrast,
    /// Image saturation.
    Saturation,
    /// Image sharpness.
    Sharpness,
    /// Sensor gain (ISO on macOS).
    Gain,
    /// Backlight compensation.
    BacklightCompensation,
    /// AC mains frequency filtering.
    PowerLineFrequency,
    /// Pan axis (PTZ-capable devices only).
    Pan,
    /// Tilt axis (PTZ-capable devices only).
    Tilt,
    /// Zoom factor (PTZ-capable devices only).
    Zoom,
}

#[cfg(feature = "controls")]
impl ControlKind {
    /// Every [`ControlKind`] variant in declaration order.
    pub const ALL: [ControlKind; 16] = [
        ControlKind::Focus,
        ControlKind::AutoFocus,
        ControlKind::Exposure,
        ControlKind::AutoExposure,
        ControlKind::WhiteBalanceTemperature,
        ControlKind::AutoWhiteBalance,
        ControlKind::Brightness,
        ControlKind::Contrast,
        ControlKind::Saturation,
        ControlKind::Sharpness,
        ControlKind::Gain,
        ControlKind::BacklightCompensation,
        ControlKind::PowerLineFrequency,
        ControlKind::Pan,
        ControlKind::Tilt,
        ControlKind::Zoom,
    ];

    /// Snake_case name matching the corresponding field on [`Controls`].
    pub fn label(&self) -> &'static str {
        match self {
            ControlKind::Focus => "focus",
            ControlKind::AutoFocus => "auto_focus",
            ControlKind::Exposure => "exposure",
            ControlKind::AutoExposure => "auto_exposure",
            ControlKind::WhiteBalanceTemperature => "white_balance_temperature",
            ControlKind::AutoWhiteBalance => "auto_white_balance",
            ControlKind::Brightness => "brightness",
            ControlKind::Contrast => "contrast",
            ControlKind::Saturation => "saturation",
            ControlKind::Sharpness => "sharpness",
            ControlKind::Gain => "gain",
            ControlKind::BacklightCompensation => "backlight_compensation",
            ControlKind::PowerLineFrequency => "power_line_frequency",
            ControlKind::Pan => "pan",
            ControlKind::Tilt => "tilt",
            ControlKind::Zoom => "zoom",
        }
    }

    /// Platform-specific caveat for this control on the current target, if any.
    ///
    /// Returns `Some` only when the current target cannot expose the control
    /// regardless of device — useful as UI tooltip text explaining why a
    /// capability row is marked unsupported. Currently populated for macOS
    /// controls that AVFoundation does not surface.
    pub fn caveat(&self) -> Option<&'static str> {
        #[cfg(target_os = "macos")]
        {
            match self {
                ControlKind::Brightness
                | ControlKind::Contrast
                | ControlKind::Saturation
                | ControlKind::Sharpness
                | ControlKind::BacklightCompensation => Some(
                    "macOS: AVFoundation doesn't expose per-channel image-processing controls. \
                     Apply CPU/GPU post-processing (shaders, color matrices) over the Frame \
                     bytes in your app. The library is capture-only.",
                ),
                ControlKind::PowerLineFrequency => {
                    Some("macOS: AVFoundation doesn't expose AC mains frequency filtering.")
                }
                ControlKind::Pan | ControlKind::Tilt => Some(
                    "macOS: AVFoundation doesn't expose pan/tilt controls for built-in or UVC cameras.",
                ),
                ControlKind::Focus
                | ControlKind::AutoFocus
                | ControlKind::Exposure
                | ControlKind::AutoExposure
                | ControlKind::WhiteBalanceTemperature
                | ControlKind::AutoWhiteBalance
                | ControlKind::Gain
                | ControlKind::Zoom => None,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }
}

/// What a device reports it can do, per control.
///
/// Each field is [`Some`] when the platform exposes the control on this
/// device and [`None`] when it does not. For numeric controls, `Some` carries
/// the native [`ControlRange`]. For auto toggles, `Some(true)` means the
/// device supports auto, `Some(false)` means manual-only, `None` means no
/// auto control.
#[cfg(feature = "controls")]
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct ControlCapabilities {
    /// Focus-position capability.
    pub focus: Option<ControlRange>,
    /// Auto-focus toggle capability.
    pub auto_focus: Option<bool>,
    /// Exposure-value capability.
    pub exposure: Option<ControlRange>,
    /// Auto-exposure toggle capability.
    pub auto_exposure: Option<bool>,
    /// White-balance-temperature capability.
    pub white_balance_temperature: Option<ControlRange>,
    /// Auto-white-balance toggle capability.
    pub auto_white_balance: Option<bool>,
    /// Brightness capability.
    pub brightness: Option<ControlRange>,
    /// Contrast capability.
    pub contrast: Option<ControlRange>,
    /// Saturation capability.
    pub saturation: Option<ControlRange>,
    /// Sharpness capability.
    pub sharpness: Option<ControlRange>,
    /// Gain capability.
    pub gain: Option<ControlRange>,
    /// Backlight-compensation capability.
    pub backlight_compensation: Option<ControlRange>,
    /// Power-line-frequency capability.
    pub power_line_frequency: Option<PowerLineFrequencyCapability>,
    /// Pan capability.
    pub pan: Option<ControlRange>,
    /// Tilt capability.
    pub tilt: Option<ControlRange>,
    /// Zoom capability.
    pub zoom: Option<ControlRange>,
}
