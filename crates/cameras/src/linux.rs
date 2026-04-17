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
use crate::types::{ControlCapabilities, Controls};
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
    fn control_capabilities(_id: &DeviceId) -> Result<ControlCapabilities, Error> {
        Err(Error::Unsupported {
            platform: "linux",
            reason: "controls not yet implemented",
        })
    }

    fn read_controls(_id: &DeviceId) -> Result<Controls, Error> {
        Err(Error::Unsupported {
            platform: "linux",
            reason: "controls not yet implemented",
        })
    }

    fn apply_controls(_id: &DeviceId, _controls: &Controls) -> Result<(), Error> {
        Err(Error::Unsupported {
            platform: "linux",
            reason: "controls not yet implemented",
        })
    }
}
