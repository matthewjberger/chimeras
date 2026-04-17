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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use windows::Win32::Foundation::{S_FALSE, S_OK};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::core::GUID;

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
        let _com = ComGuard::init()?;
        let _mf = MfGuard::init()?;
        enumerate_devices()
    }

    fn probe(id: &DeviceId) -> Result<Capabilities, Error> {
        let _com = ComGuard::init()?;
        let _mf = MfGuard::init()?;
        let source = activate_source(id)?;
        let reader = create_source_reader(&source)?;
        let formats = enumerate_formats(&reader)?;
        Ok(Capabilities { formats })
    }

    fn open(id: &DeviceId, config: StreamConfig) -> Result<Camera, Error> {
        let id_clone = id.clone();
        let (frame_tx, frame_rx) = crossbeam_channel::bounded::<Result<Frame, Error>>(3);
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_worker = Arc::clone(&shutdown);

        let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<StreamConfig, Error>>(1);

        let worker = std::thread::Builder::new()
            .name("cameras-mediafoundation".into())
            .spawn(move || {
                let ready_for_panic = ready_tx.clone();
                let frame_for_panic = frame_tx.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_capture(id_clone, config, frame_tx, shutdown_for_worker, ready_tx);
                }));
                if result.is_err() {
                    let error = Error::Backend {
                        platform: "windows",
                        message: "media foundation worker panicked".into(),
                    };
                    let _ = ready_for_panic.try_send(Err(error.clone()));
                    let _ = frame_for_panic.try_send(Err(error));
                }
            })
            .map_err(|error| Error::Backend {
                platform: "windows",
                message: error.to_string(),
            })?;

        let applied = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| Error::Backend {
                platform: "windows",
                message: "camera initialization timed out".into(),
            })??;

        Ok(Camera {
            config: applied,
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
                platform: "windows",
                message: error.to_string(),
            })?;

        Ok(DeviceMonitor {
            event_rx,
            shutdown,
            worker: Some(worker),
        })
    }
}

fn run_capture(
    device_id: DeviceId,
    config: StreamConfig,
    frame_tx: Sender<Result<Frame, Error>>,
    shutdown: Arc<AtomicBool>,
    ready_tx: Sender<Result<StreamConfig, Error>>,
) {
    let _com = match ComGuard::init() {
        Ok(guard) => guard,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let _mf = match MfGuard::init() {
        Ok(guard) => guard,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    let source = match activate_source(&device_id) {
        Ok(source) => source,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    let reader = match create_source_reader(&source) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    let (stream, applied) = match configure_reader(&reader, &config) {
        Ok(value) => value,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    if ready_tx.send(Ok(applied)).is_err() {
        return;
    }

    while !shutdown.load(Ordering::Relaxed) {
        match read_next_sample(&reader, stream, &applied) {
            Ok(Some(frame)) => {
                let _ = frame_tx.try_send(Ok(frame));
            }
            Ok(None) => continue,
            Err(error) => {
                let _ = frame_tx.try_send(Err(error));
                break;
            }
        }
    }
}

fn enumerate_devices() -> Result<Vec<Device>, Error> {
    let activations = enumerate_activations()?;
    let mut result = Vec::with_capacity(activations.len());
    for activate in &activations {
        let name = read_string(activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
            .unwrap_or_else(|_| "Camera".into());
        let symbolic = read_string(
            activate,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
        )
        .map_err(|_| Error::Backend {
            platform: "windows",
            message: "missing symbolic link".into(),
        })?;
        result.push(Device {
            id: DeviceId(symbolic),
            name,
            position: Position::External,
            transport: Transport::Usb,
        });
    }
    Ok(result)
}

fn enumerate_activations() -> Result<Vec<IMFActivate>, Error> {
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1).map_err(map_com_error)?;
        let attributes = attributes.ok_or_else(|| Error::Backend {
            platform: "windows",
            message: "failed to create MF attributes".into(),
        })?;
        attributes
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .map_err(map_com_error)?;

        let mut raw_devices: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        MFEnumDeviceSources(&attributes, &mut raw_devices, &mut count).map_err(map_com_error)?;

        let mut activations = Vec::with_capacity(count as usize);
        for index in 0..count as isize {
            let slot = raw_devices.offset(index);
            let activation = std::ptr::read(slot);
            if let Some(activation) = activation {
                activations.push(activation);
            }
        }

        windows::Win32::System::Com::CoTaskMemFree(Some(raw_devices as *const _));
        Ok(activations)
    }
}

fn activate_source(id: &DeviceId) -> Result<IMFMediaSource, Error> {
    let activations = enumerate_activations()?;
    for activate in &activations {
        let symbolic = read_string(
            activate,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
        )
        .unwrap_or_default();
        if symbolic == id.0 {
            let source: IMFMediaSource =
                unsafe { activate.ActivateObject() }.map_err(map_com_error)?;
            return Ok(source);
        }
    }
    Err(Error::DeviceNotFound(id.0.clone()))
}

fn create_source_reader(source: &IMFMediaSource) -> Result<IMFSourceReader, Error> {
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1).map_err(map_com_error)?;
        let attributes = attributes.ok_or_else(|| Error::Backend {
            platform: "windows",
            message: "failed to create source reader attributes".into(),
        })?;
        attributes
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(map_com_error)?;
        MFCreateSourceReaderFromMediaSource(source, Some(&attributes)).map_err(map_com_error)
    }
}

fn enumerate_formats(reader: &IMFSourceReader) -> Result<Vec<FormatDescriptor>, Error> {
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let mut descriptors = Vec::new();
    let mut type_index: u32 = 0;
    loop {
        let media_type = unsafe { reader.GetNativeMediaType(stream, type_index) };
        match media_type {
            Ok(media_type) => {
                if let Ok(descriptor) = descriptor_from_media_type(&media_type) {
                    descriptors.push(descriptor);
                }
                type_index += 1;
            }
            Err(_) => break,
        }
    }
    Ok(descriptors)
}

fn descriptor_from_media_type(media_type: &IMFMediaType) -> Result<FormatDescriptor, Error> {
    unsafe {
        let (width, height) = read_packed_u64(media_type, &MF_MT_FRAME_SIZE)?;
        let (fps_num, fps_den) = read_packed_u64(media_type, &MF_MT_FRAME_RATE)?;
        let subtype = media_type.GetGUID(&MF_MT_SUBTYPE).map_err(map_com_error)?;
        let pixel_format = guid_to_pixel_format(&subtype);
        let fps = if fps_den == 0 {
            0.0
        } else {
            fps_num as f64 / fps_den as f64
        };
        Ok(FormatDescriptor {
            resolution: Resolution { width, height },
            framerate_range: FramerateRange { min: fps, max: fps },
            pixel_format,
        })
    }
}

fn configure_reader(
    reader: &IMFSourceReader,
    config: &StreamConfig,
) -> Result<(u32, StreamConfig), Error> {
    let target_subtype = pixel_format_to_guid(config.pixel_format);

    let (stream, native) = find_video_stream(reader, &config.resolution)?;
    let framerate = native.framerate.max(1);

    let output_type = unsafe { MFCreateMediaType() }.map_err(map_com_error)?;
    unsafe {
        output_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(map_com_error)?;
        output_type
            .SetGUID(&MF_MT_SUBTYPE, &target_subtype)
            .map_err(map_com_error)?;
        output_type
            .SetUINT64(
                &MF_MT_FRAME_SIZE,
                pack_u32_pair(native.resolution.width, native.resolution.height),
            )
            .map_err(map_com_error)?;
        output_type
            .SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(framerate, 1))
            .map_err(map_com_error)?;
        output_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, 2)
            .map_err(map_com_error)?;
        output_type
            .SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
            .map_err(map_com_error)?;
    }

    unsafe {
        reader
            .SetCurrentMediaType(stream, None, &output_type)
            .map_err(|error| Error::Backend {
                platform: "windows",
                message: format!(
                    "SetCurrentMediaType failed for {:?} at {}x{}: {}",
                    config.pixel_format,
                    native.resolution.width,
                    native.resolution.height,
                    error.message(),
                ),
            })?;
    }

    unsafe {
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
            .map_err(map_com_error)?;
        reader
            .SetStreamSelection(stream, true)
            .map_err(map_com_error)?;
    }

    let current = unsafe { reader.GetCurrentMediaType(stream) }.map_err(map_com_error)?;
    let (applied_width, applied_height) = read_packed_u64(&current, &MF_MT_FRAME_SIZE)
        .unwrap_or((native.resolution.width, native.resolution.height));
    let (applied_fps_num, applied_fps_den) =
        read_packed_u64(&current, &MF_MT_FRAME_RATE).unwrap_or((framerate, 1));
    let applied_framerate = if applied_fps_den == 0 {
        framerate
    } else {
        (applied_fps_num as f64 / applied_fps_den as f64).round() as u32
    };

    Ok((
        stream,
        StreamConfig {
            resolution: Resolution {
                width: applied_width,
                height: applied_height,
            },
            framerate: applied_framerate.max(1),
            pixel_format: config.pixel_format,
        },
    ))
}

fn find_video_stream(
    reader: &IMFSourceReader,
    target: &Resolution,
) -> Result<(u32, NativeMatch), Error> {
    let first_video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    if let Ok(value) = pick_native_resolution(reader, first_video, target) {
        return Ok((first_video, value));
    }

    let mut last_error: Option<Error> = None;
    for stream_index in 0u32..16 {
        let media_type = match unsafe { reader.GetNativeMediaType(stream_index, 0) } {
            Ok(media_type) => media_type,
            Err(_) => continue,
        };
        let major = match unsafe { media_type.GetGUID(&MF_MT_MAJOR_TYPE) } {
            Ok(value) => value,
            Err(_) => continue,
        };
        if major != MFMediaType_Video {
            continue;
        }
        match pick_native_resolution(reader, stream_index, target) {
            Ok(value) => return Ok((stream_index, value)),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or(Error::FormatNotSupported))
}

struct NativeMatch {
    resolution: Resolution,
    framerate: u32,
}

fn pick_native_resolution(
    reader: &IMFSourceReader,
    stream: u32,
    target: &Resolution,
) -> Result<NativeMatch, Error> {
    let mut exact: Option<NativeMatch> = None;
    let mut closest: Option<(i64, NativeMatch)> = None;
    let mut enumerated = 0u32;
    let mut first_error: Option<windows::core::Error> = None;

    let mut type_index: u32 = 0;
    loop {
        let media_type = match unsafe { reader.GetNativeMediaType(stream, type_index) } {
            Ok(media_type) => media_type,
            Err(error) => {
                if enumerated == 0 {
                    first_error = Some(error);
                }
                break;
            }
        };
        enumerated += 1;
        type_index += 1;

        let Ok((width, height)) = read_packed_u64(&media_type, &MF_MT_FRAME_SIZE) else {
            continue;
        };
        let (fps_num, fps_den) = read_packed_u64(&media_type, &MF_MT_FRAME_RATE).unwrap_or((30, 1));
        let framerate = if fps_den == 0 {
            30
        } else {
            (fps_num as f64 / fps_den as f64).round() as u32
        };
        let candidate = NativeMatch {
            resolution: Resolution { width, height },
            framerate,
        };

        if width == target.width && height == target.height {
            exact = Some(candidate);
            break;
        }

        let delta = (width as i64 - target.width as i64).abs()
            + (height as i64 - target.height as i64).abs();
        match &closest {
            None => closest = Some((delta, candidate)),
            Some((best_delta, _)) if delta < *best_delta => {
                closest = Some((delta, candidate));
            }
            _ => {}
        }
    }

    if let Some(value) = exact.or(closest.map(|(_, value)| value)) {
        return Ok(value);
    }
    if let Some(error) = first_error {
        return Err(Error::Backend {
            platform: "windows",
            message: format!(
                "device has no enumerable video media types (GetNativeMediaType: {})",
                error.message()
            ),
        });
    }
    Err(Error::FormatNotSupported)
}

fn pack_u32_pair(high: u32, low: u32) -> u64 {
    (high as u64) << 32 | low as u64
}

fn stride_from_current_type(reader: &IMFSourceReader, stream: u32) -> Option<u32> {
    let current = unsafe { reader.GetCurrentMediaType(stream) }.ok()?;
    let stride = unsafe { current.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.ok()?;
    Some((stride as i32).unsigned_abs())
}

fn expected_stride_bytes(pixel_format: PixelFormat, width: u32) -> u32 {
    match pixel_format {
        PixelFormat::Bgra8 | PixelFormat::Rgba8 => width * 4,
        PixelFormat::Rgb8 => width * 3,
        PixelFormat::Yuyv => width * 2,
        PixelFormat::Nv12 => width,
        PixelFormat::Mjpeg => 0,
    }
}

fn read_next_sample(
    reader: &IMFSourceReader,
    stream: u32,
    applied: &StreamConfig,
) -> Result<Option<Frame>, Error> {
    let declared_stride = stride_from_current_type(reader, stream)
        .unwrap_or_else(|| expected_stride_bytes(applied.pixel_format, applied.resolution.width));
    let mut stream_index: u32 = 0;
    let mut stream_flags: u32 = 0;
    let mut timestamp: i64 = 0;
    let mut sample: Option<IMFSample> = None;
    unsafe {
        reader
            .ReadSample(
                stream,
                0,
                Some(&mut stream_index),
                Some(&mut stream_flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
            .map_err(map_com_error)?;
    }
    let Some(sample) = sample else {
        return Ok(None);
    };

    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(map_com_error)?;
    let mut base_ptr: *mut u8 = std::ptr::null_mut();
    let mut max_length: u32 = 0;
    let mut current_length: u32 = 0;
    unsafe {
        buffer
            .Lock(
                &mut base_ptr,
                Some(&mut max_length),
                Some(&mut current_length),
            )
            .map_err(map_com_error)?;
    }

    let width = applied.resolution.width as usize;
    let height = applied.resolution.height as usize;
    let expected_stride = expected_stride_bytes(applied.pixel_format, applied.resolution.width);
    let expected_size = expected_stride as usize * height;
    let length = current_length as usize;
    let safe_length = length
        .min(max_length as usize)
        .min(expected_size.max(length));

    let data = if base_ptr.is_null() || safe_length == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(base_ptr, safe_length) }.to_vec()
    };

    unsafe {
        let _ = buffer.Unlock();
    }

    let _ = width;

    let frame_timestamp = if timestamp > 0 {
        Duration::from_nanos((timestamp as u64).saturating_mul(100))
    } else {
        Duration::ZERO
    };

    let stride = declared_stride;

    Ok(Some(Frame {
        width: applied.resolution.width,
        height: applied.resolution.height,
        stride,
        timestamp: frame_timestamp,
        pixel_format: applied.pixel_format,
        quality: crate::types::FrameQuality::Intact,
        plane_primary: Bytes::from(data),
        plane_secondary: Bytes::new(),
    }))
}

fn read_string(activate: &IMFActivate, key: &GUID) -> Result<String, Error> {
    unsafe {
        let length = activate.GetStringLength(key).map_err(map_com_error)?;
        let mut buffer = vec![0u16; (length + 1) as usize];
        let mut written: u32 = 0;
        activate
            .GetString(key, &mut buffer, Some(&mut written))
            .map_err(map_com_error)?;
        let end = written as usize;
        Ok(String::from_utf16_lossy(&buffer[..end]))
    }
}

fn read_packed_u64(media_type: &IMFMediaType, key: &GUID) -> Result<(u32, u32), Error> {
    unsafe {
        let packed = media_type.GetUINT64(key).map_err(map_com_error)?;
        let high = (packed >> 32) as u32;
        let low = (packed & 0xFFFF_FFFF) as u32;
        Ok((high, low))
    }
}

fn guid_to_pixel_format(guid: &GUID) -> PixelFormat {
    if *guid == MFVideoFormat_RGB32 {
        PixelFormat::Bgra8
    } else if *guid == MFVideoFormat_ARGB32 {
        PixelFormat::Rgba8
    } else if *guid == MFVideoFormat_NV12 {
        PixelFormat::Nv12
    } else if *guid == MFVideoFormat_YUY2 {
        PixelFormat::Yuyv
    } else {
        PixelFormat::Mjpeg
    }
}

fn pixel_format_to_guid(format: PixelFormat) -> GUID {
    match format {
        PixelFormat::Bgra8 => MFVideoFormat_RGB32,
        PixelFormat::Rgba8 => MFVideoFormat_ARGB32,
        PixelFormat::Nv12 => PixelFormat::native_nv12(),
        PixelFormat::Yuyv => MFVideoFormat_YUY2,
        PixelFormat::Mjpeg => MFVideoFormat_MJPG,
        PixelFormat::Rgb8 => MFVideoFormat_RGB24,
    }
}

impl PixelFormat {
    fn native_nv12() -> GUID {
        MFVideoFormat_NV12
    }
}

fn map_com_error(error: windows::core::Error) -> Error {
    Error::Backend {
        platform: "windows",
        message: error.message().to_string(),
    }
}

struct ComGuard {
    initialized: bool,
}

impl ComGuard {
    fn init() -> Result<Self, Error> {
        let hresult =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if hresult == S_OK || hresult == S_FALSE {
            Ok(Self { initialized: true })
        } else if hresult.is_err() {
            Err(Error::Backend {
                platform: "windows",
                message: format!("CoInitializeEx failed: 0x{:08X}", hresult.0),
            })
        } else {
            Ok(Self { initialized: false })
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

struct MfGuard {
    initialized: bool,
}

impl MfGuard {
    fn init() -> Result<Self, Error> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(map_com_error)?;
        }
        Ok(Self { initialized: true })
    }
}

impl Drop for MfGuard {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                let _ = MFShutdown();
            }
        }
    }
}

#[cfg(feature = "controls")]
impl BackendControls for Driver {
    fn control_capabilities(_id: &DeviceId) -> Result<ControlCapabilities, Error> {
        Err(Error::Unsupported {
            platform: "windows",
            reason: "controls not yet implemented",
        })
    }

    fn read_controls(_id: &DeviceId) -> Result<Controls, Error> {
        Err(Error::Unsupported {
            platform: "windows",
            reason: "controls not yet implemented",
        })
    }

    fn apply_controls(_id: &DeviceId, _controls: &Controls) -> Result<(), Error> {
        Err(Error::Unsupported {
            platform: "windows",
            reason: "controls not yet implemented",
        })
    }
}
