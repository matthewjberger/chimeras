use crate::error::Error;
use crate::macos::permission::ensure_authorized;
use crate::types::{
    Capabilities, Device, DeviceId, FormatDescriptor, FramerateRange, PixelFormat, Position,
    Resolution, Transport,
};
use objc2::rc::Retained;
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDevicePosition, AVCaptureDeviceType,
    AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureDeviceTypeExternal, AVMediaTypeVideo,
};
use objc2_core_media::CMVideoFormatDescriptionGetDimensions;
use objc2_foundation::{NSArray, NSString};

pub fn devices() -> Result<Vec<Device>, Error> {
    ensure_authorized()?;

    let session = discovery_session();
    let ns_devices = unsafe { session.devices() };

    let mut result = Vec::with_capacity(ns_devices.count());
    for index in 0..ns_devices.count() {
        let device = ns_devices.objectAtIndex(index);
        result.push(device_to_public(&device));
    }
    Ok(result)
}

pub fn probe(id: &DeviceId) -> Result<Capabilities, Error> {
    ensure_authorized()?;

    let device = find_device(id)?;
    let formats = unsafe { device.formats() };
    let mut descriptors = Vec::new();

    for index in 0..formats.count() {
        let format = formats.objectAtIndex(index);
        let description = unsafe { format.formatDescription() };
        let dimensions = unsafe { CMVideoFormatDescriptionGetDimensions(&description) };
        let subtype = unsafe { description.media_sub_type() };
        let pixel_format = fourcc_to_pixel_format(subtype);
        let resolution = Resolution {
            width: dimensions.width as u32,
            height: dimensions.height as u32,
        };
        let ranges = unsafe { format.videoSupportedFrameRateRanges() };
        for range_index in 0..ranges.count() {
            let range = ranges.objectAtIndex(range_index);
            descriptors.push(FormatDescriptor {
                resolution,
                framerate_range: FramerateRange {
                    min: unsafe { range.minFrameRate() },
                    max: unsafe { range.maxFrameRate() },
                },
                pixel_format,
            });
        }
    }

    Ok(Capabilities {
        formats: descriptors,
    })
}

fn fourcc_to_pixel_format(code: u32) -> PixelFormat {
    let bytes = code.to_be_bytes();
    match &bytes {
        b"yuvs" | b"yuv2" => PixelFormat::Yuyv,
        b"420f" | b"420v" => PixelFormat::Nv12,
        b"BGRA" => PixelFormat::Bgra8,
        b"RGBA" => PixelFormat::Rgba8,
        b"jpeg" | b"dmb1" => PixelFormat::Mjpeg,
        _ => PixelFormat::Bgra8,
    }
}

pub(super) fn find_device(id: &DeviceId) -> Result<Retained<AVCaptureDevice>, Error> {
    let ns_id = NSString::from_str(&id.0);
    unsafe { AVCaptureDevice::deviceWithUniqueID(&ns_id) }
        .ok_or_else(|| Error::DeviceNotFound(id.0.clone()))
}

pub(super) fn discovery_session() -> Retained<AVCaptureDeviceDiscoverySession> {
    let device_type_refs: [&AVCaptureDeviceType; 2] = unsafe {
        [
            AVCaptureDeviceTypeBuiltInWideAngleCamera,
            AVCaptureDeviceTypeExternal,
        ]
    };
    let device_types = NSArray::from_slice(&device_type_refs);
    let media_type = unsafe { AVMediaTypeVideo };
    unsafe {
        AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &device_types,
            media_type,
            AVCaptureDevicePosition::Unspecified,
        )
    }
}

fn device_to_public(device: &AVCaptureDevice) -> Device {
    let unique_id = unsafe { device.uniqueID() };
    let name = unsafe { device.localizedName() };
    let raw_position = unsafe { device.position() };
    let position = if raw_position == AVCaptureDevicePosition::Front {
        Position::Front
    } else if raw_position == AVCaptureDevicePosition::Back {
        Position::Back
    } else {
        Position::External
    };
    let transport = transport_from_code(unsafe { device.transportType() });
    Device {
        id: DeviceId(unique_id.to_string()),
        name: name.to_string(),
        position,
        transport,
    }
}

fn transport_from_code(code: i32) -> Transport {
    let bytes = code.to_be_bytes();
    match &bytes {
        b"usb " => Transport::Usb,
        b"bltn" => Transport::BuiltIn,
        b"virt" => Transport::Virtual,
        _ => Transport::Other,
    }
}
