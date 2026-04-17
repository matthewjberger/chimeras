use crate::backend::Backend;
#[cfg(feature = "controls")]
use crate::backend::BackendControls;
use crate::camera::Camera;
use crate::error::Error;
use crate::monitor::DeviceMonitor;
use crate::types::{
    Capabilities, Device, DeviceId, FormatDescriptor, Frame, FramerateRange, PixelFormat, Position,
    Resolution, StreamConfig, Transport,
};
#[cfg(feature = "controls")]
use crate::types::{
    ControlCapabilities, ControlRange, Controls, PowerLineFrequency, PowerLineFrequencyCapability,
};
use bytes::Bytes;
use crossbeam_channel::Sender;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use v4l::Device as V4lDevice;
use v4l::buffer::Type as BufferType;
use v4l::context;
use v4l::format::Format as V4lFormat;
use v4l::format::FourCC;
use v4l::frameinterval::FrameIntervalEnum;
use v4l::io::mmap::stream::Stream as MmapStream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;

pub struct SessionHandle {
    shutdown: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

pub struct Driver;

impl Backend for Driver {
    type SessionHandle = SessionHandle;

    fn devices() -> Result<Vec<Device>, Error> {
        let entries = context::enum_devices();
        let mut result = Vec::with_capacity(entries.len());
        for entry in entries {
            let name = entry
                .name()
                .unwrap_or_else(|| format!("/dev/video{}", entry.index()));
            let path = entry.path().to_str().unwrap_or("").to_string();
            if path.is_empty() {
                continue;
            }
            result.push(Device {
                id: DeviceId(path),
                name,
                position: Position::External,
                transport: Transport::Usb,
            });
        }
        Ok(result)
    }

    fn probe(id: &DeviceId) -> Result<Capabilities, Error> {
        let device = V4lDevice::with_path(&id.0).map_err(map_io_error)?;
        let format_descriptions = device.enum_formats().map_err(map_io_error)?;

        let mut descriptors = Vec::new();
        for description in format_descriptions {
            let pixel_format = fourcc_to_pixel_format(&description.fourcc);
            let sizes = device
                .enum_framesizes(description.fourcc)
                .map_err(map_io_error)?;
            for framesize in sizes {
                for discrete in framesize.size.to_discrete() {
                    let intervals = device
                        .enum_frameintervals(framesize.fourcc, discrete.width, discrete.height)
                        .map_err(map_io_error)?;
                    for interval in intervals {
                        let (min_fps, max_fps) = interval_to_fps_range(&interval.interval);
                        descriptors.push(FormatDescriptor {
                            resolution: Resolution {
                                width: discrete.width,
                                height: discrete.height,
                            },
                            framerate_range: FramerateRange {
                                min: min_fps,
                                max: max_fps,
                            },
                            pixel_format,
                        });
                    }
                }
            }
        }

        Ok(Capabilities {
            formats: descriptors,
        })
    }

    fn open(id: &DeviceId, config: StreamConfig) -> Result<Camera, Error> {
        let device = V4lDevice::with_path(&id.0).map_err(map_io_error)?;
        let requested_fourcc = pixel_format_to_fourcc(config.pixel_format);
        let target_format = V4lFormat::new(
            config.resolution.width,
            config.resolution.height,
            requested_fourcc,
        );
        let applied = device.set_format(&target_format).map_err(map_io_error)?;
        let applied_pixel_format = fourcc_to_pixel_format(&applied.fourcc);
        let width = applied.width;
        let height = applied.height;
        let stride = applied.stride;

        let (frame_tx, frame_rx) = crossbeam_channel::bounded::<Result<Frame, Error>>(3);
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_worker = Arc::clone(&shutdown);

        let worker = std::thread::Builder::new()
            .name("cameras-v4l".into())
            .spawn(move || {
                stream_loop(
                    device,
                    frame_tx,
                    shutdown_for_worker,
                    applied_pixel_format,
                    width,
                    height,
                    stride,
                );
            })
            .map_err(|error| Error::Backend {
                platform: "linux",
                message: error.to_string(),
            })?;

        Ok(Camera {
            config: StreamConfig {
                resolution: Resolution { width, height },
                framerate: config.framerate,
                pixel_format: applied_pixel_format,
            },
            frame_rx,
            handle: crate::camera::Handle::Native(SessionHandle {
                shutdown,
                worker: Some(worker),
            }),
        })
    }

    fn monitor() -> Result<DeviceMonitor, Error> {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_thread = Arc::clone(&shutdown);

        let initial = Self::devices()?;
        for device in &initial {
            let _ = event_tx.send(crate::types::DeviceEvent::Added(device.clone()));
        }

        let worker = std::thread::Builder::new()
            .name("cameras-monitor".into())
            .spawn(move || {
                let mut known: HashMap<DeviceId, Device> = initial
                    .into_iter()
                    .map(|device| (device.id.clone(), device))
                    .collect();
                let interval = Duration::from_millis(1000);
                while !shutdown_for_thread.load(Ordering::Relaxed) {
                    std::thread::sleep(interval);
                    if shutdown_for_thread.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(current) = Self::devices() else {
                        continue;
                    };
                    let current_map: HashMap<DeviceId, Device> = current
                        .into_iter()
                        .map(|device| (device.id.clone(), device))
                        .collect();
                    for (id, device) in &current_map {
                        if !known.contains_key(id) {
                            let _ = event_tx.send(crate::types::DeviceEvent::Added(device.clone()));
                        }
                    }
                    let removed: Vec<DeviceId> = known
                        .keys()
                        .filter(|id| !current_map.contains_key(id))
                        .cloned()
                        .collect();
                    for id in removed {
                        let _ = event_tx.send(crate::types::DeviceEvent::Removed(id.clone()));
                        known.remove(&id);
                    }
                    for (id, device) in current_map {
                        known.insert(id, device);
                    }
                }
            })
            .map_err(|error| Error::Backend {
                platform: "linux",
                message: error.to_string(),
            })?;

        Ok(DeviceMonitor {
            event_rx,
            shutdown,
            worker: Some(worker),
        })
    }
}

fn stream_loop(
    device: V4lDevice,
    sender: Sender<Result<Frame, Error>>,
    shutdown: Arc<AtomicBool>,
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
    stride: u32,
) {
    let mut stream = match MmapStream::with_buffers(&device, BufferType::VideoCapture, 4) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = sender.try_send(Err(Error::Backend {
                platform: "linux",
                message: error.to_string(),
            }));
            return;
        }
    };

    while !shutdown.load(Ordering::Relaxed) {
        match stream.next() {
            Ok((buffer, _meta)) => {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                let (plane_primary, plane_secondary) =
                    split_planes(buffer, pixel_format, width, height, stride);
                let frame = Frame {
                    width,
                    height,
                    stride,
                    timestamp,
                    pixel_format,
                    quality: crate::types::FrameQuality::Intact,
                    plane_primary,
                    plane_secondary,
                };
                let _ = sender.try_send(Ok(frame));
            }
            Err(error) => {
                let _ = sender.try_send(Err(Error::Backend {
                    platform: "linux",
                    message: error.to_string(),
                }));
                break;
            }
        }
    }
}

fn split_planes(
    buffer: &[u8],
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
    stride: u32,
) -> (Bytes, Bytes) {
    if pixel_format == PixelFormat::Nv12 {
        let y_stride = if stride == 0 { width } else { stride } as usize;
        let y_size = y_stride * height as usize;
        if buffer.len() >= y_size {
            let primary = Bytes::copy_from_slice(&buffer[..y_size]);
            let secondary = Bytes::copy_from_slice(&buffer[y_size..]);
            return (primary, secondary);
        }
    }
    (Bytes::copy_from_slice(buffer), Bytes::new())
}

fn fourcc_to_pixel_format(fourcc: &FourCC) -> PixelFormat {
    match &fourcc.repr {
        b"MJPG" | b"JPEG" => PixelFormat::Mjpeg,
        b"YUYV" | b"YUY2" => PixelFormat::Yuyv,
        b"NV12" => PixelFormat::Nv12,
        b"RGB3" | b"RGB\0" => PixelFormat::Rgb8,
        b"BGR3" => PixelFormat::Bgra8,
        b"AB24" | b"RGBA" => PixelFormat::Rgba8,
        b"BA24" | b"BGRA" => PixelFormat::Bgra8,
        _ => PixelFormat::Mjpeg,
    }
}

fn pixel_format_to_fourcc(format: PixelFormat) -> FourCC {
    match format {
        PixelFormat::Mjpeg => FourCC::new(b"MJPG"),
        PixelFormat::Yuyv => FourCC::new(b"YUYV"),
        PixelFormat::Nv12 => FourCC::new(b"NV12"),
        PixelFormat::Rgb8 => FourCC::new(b"RGB3"),
        PixelFormat::Rgba8 => FourCC::new(b"AB24"),
        PixelFormat::Bgra8 => FourCC::new(b"BA24"),
    }
}

fn interval_to_fps_range(interval: &FrameIntervalEnum) -> (f64, f64) {
    match interval {
        FrameIntervalEnum::Discrete(value) => {
            let fps = value.denominator as f64 / value.numerator.max(1) as f64;
            (fps, fps)
        }
        FrameIntervalEnum::Stepwise(range) => {
            let max_fps = range.min.denominator as f64 / range.min.numerator.max(1) as f64;
            let min_fps = range.max.denominator as f64 / range.max.numerator.max(1) as f64;
            (min_fps, max_fps)
        }
    }
}

fn map_io_error(error: io::Error) -> Error {
    match error.kind() {
        io::ErrorKind::PermissionDenied => Error::PermissionDenied,
        io::ErrorKind::NotFound => Error::DeviceNotFound(error.to_string()),
        _ => Error::Backend {
            platform: "linux",
            message: error.to_string(),
        },
    }
}

#[cfg(feature = "controls")]
impl BackendControls for Driver {
    fn control_capabilities(id: &DeviceId) -> Result<ControlCapabilities, Error> {
        let device = V4lDevice::with_path(&id.0).map_err(map_io_error)?;
        let descriptions = device.query_controls().map_err(map_io_error)?;
        let mut caps = ControlCapabilities::default();
        for description in descriptions.iter() {
            if description_is_unavailable(description) {
                continue;
            }
            populate_capability(&mut caps, description);
        }
        Ok(caps)
    }

    fn read_controls(id: &DeviceId) -> Result<Controls, Error> {
        let device = V4lDevice::with_path(&id.0).map_err(map_io_error)?;
        let descriptions = device.query_controls().map_err(map_io_error)?;
        let mut result = Controls::default();
        for description in descriptions.iter() {
            if description_is_unavailable(description) {
                continue;
            }
            let Ok(control) = device.control(description.id) else {
                continue;
            };
            populate_control_value(&mut result, description.id, &control.value);
        }
        Ok(result)
    }

    fn apply_controls(id: &DeviceId, controls: &Controls) -> Result<(), Error> {
        let device = V4lDevice::with_path(&id.0).map_err(map_io_error)?;
        let descriptions = device.query_controls().map_err(map_io_error)?;

        let auto_batch = collect_auto_mode_writes(controls, &descriptions)?;
        if !auto_batch.is_empty() {
            device.set_controls(auto_batch).map_err(map_io_error)?;
        }

        let value_batch = collect_value_writes(controls, &descriptions)?;
        if !value_batch.is_empty() {
            device.set_controls(value_batch).map_err(map_io_error)?;
        }

        Ok(())
    }
}

#[cfg(feature = "controls")]
use v4l::control::{Control as V4lControl, Description, Flags as V4lFlags, Value as V4lValue};
#[cfg(feature = "controls")]
use v4l::v4l_sys::{
    V4L2_CID_AUTO_WHITE_BALANCE, V4L2_CID_BACKLIGHT_COMPENSATION, V4L2_CID_BRIGHTNESS,
    V4L2_CID_CONTRAST, V4L2_CID_EXPOSURE_ABSOLUTE, V4L2_CID_EXPOSURE_AUTO, V4L2_CID_FOCUS_ABSOLUTE,
    V4L2_CID_FOCUS_AUTO, V4L2_CID_GAIN, V4L2_CID_PAN_ABSOLUTE, V4L2_CID_POWER_LINE_FREQUENCY,
    V4L2_CID_SATURATION, V4L2_CID_SHARPNESS, V4L2_CID_TILT_ABSOLUTE,
    V4L2_CID_WHITE_BALANCE_TEMPERATURE, V4L2_CID_ZOOM_ABSOLUTE,
};

#[cfg(feature = "controls")]
const V4L2_EXPOSURE_AUTO_FULL: i64 = 0;
#[cfg(feature = "controls")]
const V4L2_EXPOSURE_AUTO_MANUAL: i64 = 1;
#[cfg(feature = "controls")]
const V4L2_POWER_LINE_FREQUENCY_DISABLED: i64 = 0;
#[cfg(feature = "controls")]
const V4L2_POWER_LINE_FREQUENCY_HZ50: i64 = 1;
#[cfg(feature = "controls")]
const V4L2_POWER_LINE_FREQUENCY_HZ60: i64 = 2;
#[cfg(feature = "controls")]
const V4L2_POWER_LINE_FREQUENCY_AUTO: i64 = 3;

#[cfg(feature = "controls")]
fn description_is_unavailable(description: &Description) -> bool {
    description.flags.contains(V4lFlags::DISABLED) || description.flags.contains(V4lFlags::INACTIVE)
}

#[cfg(feature = "controls")]
fn description_range(description: &Description) -> ControlRange {
    ControlRange {
        min: description.minimum as f32,
        max: description.maximum as f32,
        step: description.step as f32,
        default: description.default as f32,
    }
}

#[cfg(feature = "controls")]
fn populate_capability(caps: &mut ControlCapabilities, description: &Description) {
    let range = description_range(description);
    match description.id {
        id if id == V4L2_CID_FOCUS_ABSOLUTE => caps.focus = Some(range),
        id if id == V4L2_CID_FOCUS_AUTO => {
            caps.auto_focus = Some(true);
        }
        id if id == V4L2_CID_EXPOSURE_ABSOLUTE => caps.exposure = Some(range),
        id if id == V4L2_CID_EXPOSURE_AUTO => {
            caps.auto_exposure = Some(true);
        }
        id if id == V4L2_CID_WHITE_BALANCE_TEMPERATURE => {
            caps.white_balance_temperature = Some(range);
        }
        id if id == V4L2_CID_AUTO_WHITE_BALANCE => {
            caps.auto_white_balance = Some(true);
        }
        id if id == V4L2_CID_BRIGHTNESS => caps.brightness = Some(range),
        id if id == V4L2_CID_CONTRAST => caps.contrast = Some(range),
        id if id == V4L2_CID_SATURATION => caps.saturation = Some(range),
        id if id == V4L2_CID_SHARPNESS => caps.sharpness = Some(range),
        id if id == V4L2_CID_GAIN => caps.gain = Some(range),
        id if id == V4L2_CID_BACKLIGHT_COMPENSATION => caps.backlight_compensation = Some(range),
        id if id == V4L2_CID_POWER_LINE_FREQUENCY => {
            caps.power_line_frequency = Some(power_line_capability(description));
        }
        id if id == V4L2_CID_PAN_ABSOLUTE => caps.pan = Some(range),
        id if id == V4L2_CID_TILT_ABSOLUTE => caps.tilt = Some(range),
        id if id == V4L2_CID_ZOOM_ABSOLUTE => caps.zoom = Some(range),
        _ => {}
    }
}

#[cfg(feature = "controls")]
fn power_line_capability(description: &Description) -> PowerLineFrequencyCapability {
    let items = description.items.as_ref();
    let has_value = |target: i64| -> bool {
        items
            .map(|entries| entries.iter().any(|(value, _)| *value as i64 == target))
            .unwrap_or(false)
    };
    PowerLineFrequencyCapability {
        disabled: has_value(V4L2_POWER_LINE_FREQUENCY_DISABLED),
        hz50: has_value(V4L2_POWER_LINE_FREQUENCY_HZ50),
        hz60: has_value(V4L2_POWER_LINE_FREQUENCY_HZ60),
        auto: has_value(V4L2_POWER_LINE_FREQUENCY_AUTO),
        default: power_line_from_value(description.default).unwrap_or(PowerLineFrequency::Disabled),
    }
}

#[cfg(feature = "controls")]
fn power_line_from_value(value: i64) -> Option<PowerLineFrequency> {
    match value {
        V4L2_POWER_LINE_FREQUENCY_DISABLED => Some(PowerLineFrequency::Disabled),
        V4L2_POWER_LINE_FREQUENCY_HZ50 => Some(PowerLineFrequency::Hz50),
        V4L2_POWER_LINE_FREQUENCY_HZ60 => Some(PowerLineFrequency::Hz60),
        V4L2_POWER_LINE_FREQUENCY_AUTO => Some(PowerLineFrequency::Auto),
        _ => None,
    }
}

#[cfg(feature = "controls")]
fn power_line_to_value(frequency: PowerLineFrequency) -> i64 {
    match frequency {
        PowerLineFrequency::Disabled => V4L2_POWER_LINE_FREQUENCY_DISABLED,
        PowerLineFrequency::Hz50 => V4L2_POWER_LINE_FREQUENCY_HZ50,
        PowerLineFrequency::Hz60 => V4L2_POWER_LINE_FREQUENCY_HZ60,
        PowerLineFrequency::Auto => V4L2_POWER_LINE_FREQUENCY_AUTO,
    }
}

#[cfg(feature = "controls")]
fn populate_control_value(target: &mut Controls, id: u32, value: &V4lValue) {
    let as_integer = match value {
        V4lValue::Integer(number) => Some(*number),
        V4lValue::Boolean(flag) => Some(*flag as i64),
        _ => None,
    };
    let Some(number) = as_integer else { return };
    match id {
        V4L2_CID_FOCUS_ABSOLUTE => target.focus = Some(number as f32),
        V4L2_CID_FOCUS_AUTO => target.auto_focus = Some(number != 0),
        V4L2_CID_EXPOSURE_ABSOLUTE => target.exposure = Some(number as f32),
        V4L2_CID_EXPOSURE_AUTO => {
            target.auto_exposure = Some(number != V4L2_EXPOSURE_AUTO_MANUAL);
        }
        V4L2_CID_WHITE_BALANCE_TEMPERATURE => {
            target.white_balance_temperature = Some(number as f32);
        }
        V4L2_CID_AUTO_WHITE_BALANCE => target.auto_white_balance = Some(number != 0),
        V4L2_CID_BRIGHTNESS => target.brightness = Some(number as f32),
        V4L2_CID_CONTRAST => target.contrast = Some(number as f32),
        V4L2_CID_SATURATION => target.saturation = Some(number as f32),
        V4L2_CID_SHARPNESS => target.sharpness = Some(number as f32),
        V4L2_CID_GAIN => target.gain = Some(number as f32),
        V4L2_CID_BACKLIGHT_COMPENSATION => target.backlight_compensation = Some(number as f32),
        V4L2_CID_POWER_LINE_FREQUENCY => {
            target.power_line_frequency = power_line_from_value(number)
        }
        V4L2_CID_PAN_ABSOLUTE => target.pan = Some(number as f32),
        V4L2_CID_TILT_ABSOLUTE => target.tilt = Some(number as f32),
        V4L2_CID_ZOOM_ABSOLUTE => target.zoom = Some(number as f32),
        _ => {}
    }
}

#[cfg(feature = "controls")]
fn find_description<'a>(
    descriptions: &'a [Description],
    id: u32,
    reason: &'static str,
) -> Result<&'a Description, Error> {
    descriptions
        .iter()
        .find(|description| description.id == id)
        .filter(|description| !description_is_unavailable(description))
        .ok_or(Error::Unsupported {
            platform: "linux",
            reason,
        })
}

#[cfg(feature = "controls")]
fn clamp_and_snap_to_description(value: f32, description: &Description) -> i64 {
    let clamped = (value as f64)
        .clamp(description.minimum as f64, description.maximum as f64)
        .round() as i64;
    if description.step <= 1 {
        return clamped;
    }
    let offset = clamped - description.minimum;
    let step = description.step as i64;
    let snapped = (offset / step) * step;
    description.minimum + snapped
}

#[cfg(feature = "controls")]
fn ensure_writable(description: &Description, reason: &'static str) -> Result<(), Error> {
    if description.flags.contains(V4lFlags::READ_ONLY) {
        return Err(Error::Unsupported {
            platform: "linux",
            reason,
        });
    }
    Ok(())
}

#[cfg(feature = "controls")]
fn collect_auto_mode_writes(
    controls: &Controls,
    descriptions: &[Description],
) -> Result<Vec<V4lControl>, Error> {
    let mut batch = Vec::new();
    if let Some(enabled) = controls.auto_focus {
        let description = find_description(descriptions, V4L2_CID_FOCUS_AUTO, "auto_focus")?;
        ensure_writable(description, "auto_focus")?;
        batch.push(V4lControl {
            id: V4L2_CID_FOCUS_AUTO,
            value: V4lValue::Integer(if enabled { 1 } else { 0 }),
        });
    }
    if let Some(enabled) = controls.auto_exposure {
        let description = find_description(descriptions, V4L2_CID_EXPOSURE_AUTO, "auto_exposure")?;
        ensure_writable(description, "auto_exposure")?;
        let value = if enabled {
            V4L2_EXPOSURE_AUTO_FULL
        } else {
            V4L2_EXPOSURE_AUTO_MANUAL
        };
        batch.push(V4lControl {
            id: V4L2_CID_EXPOSURE_AUTO,
            value: V4lValue::Integer(value),
        });
    }
    if let Some(enabled) = controls.auto_white_balance {
        let description = find_description(
            descriptions,
            V4L2_CID_AUTO_WHITE_BALANCE,
            "auto_white_balance",
        )?;
        ensure_writable(description, "auto_white_balance")?;
        batch.push(V4lControl {
            id: V4L2_CID_AUTO_WHITE_BALANCE,
            value: V4lValue::Integer(if enabled { 1 } else { 0 }),
        });
    }
    Ok(batch)
}

#[cfg(feature = "controls")]
fn collect_value_writes(
    controls: &Controls,
    descriptions: &[Description],
) -> Result<Vec<V4lControl>, Error> {
    let mut batch = Vec::new();
    let numeric_fields: [(Option<f32>, u32, &'static str); 12] = [
        (controls.focus, V4L2_CID_FOCUS_ABSOLUTE, "focus"),
        (controls.exposure, V4L2_CID_EXPOSURE_ABSOLUTE, "exposure"),
        (
            controls.white_balance_temperature,
            V4L2_CID_WHITE_BALANCE_TEMPERATURE,
            "white_balance_temperature",
        ),
        (controls.brightness, V4L2_CID_BRIGHTNESS, "brightness"),
        (controls.contrast, V4L2_CID_CONTRAST, "contrast"),
        (controls.saturation, V4L2_CID_SATURATION, "saturation"),
        (controls.sharpness, V4L2_CID_SHARPNESS, "sharpness"),
        (controls.gain, V4L2_CID_GAIN, "gain"),
        (
            controls.backlight_compensation,
            V4L2_CID_BACKLIGHT_COMPENSATION,
            "backlight_compensation",
        ),
        (controls.pan, V4L2_CID_PAN_ABSOLUTE, "pan"),
        (controls.tilt, V4L2_CID_TILT_ABSOLUTE, "tilt"),
        (controls.zoom, V4L2_CID_ZOOM_ABSOLUTE, "zoom"),
    ];
    for (maybe_value, cid, reason) in numeric_fields {
        if let Some(value) = maybe_value {
            let description = find_description(descriptions, cid, reason)?;
            ensure_writable(description, reason)?;
            batch.push(V4lControl {
                id: cid,
                value: V4lValue::Integer(clamp_and_snap_to_description(value, description)),
            });
        }
    }
    if let Some(frequency) = controls.power_line_frequency {
        let description = find_description(
            descriptions,
            V4L2_CID_POWER_LINE_FREQUENCY,
            "power_line_frequency",
        )?;
        ensure_writable(description, "power_line_frequency")?;
        batch.push(V4lControl {
            id: V4L2_CID_POWER_LINE_FREQUENCY,
            value: V4lValue::Integer(power_line_to_value(frequency)),
        });
    }
    Ok(batch)
}
