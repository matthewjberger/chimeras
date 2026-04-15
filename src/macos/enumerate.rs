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
    let localized = unsafe { device.localizedName() }.to_string();
    let manufacturer = unsafe { device.manufacturer() }.to_string();
    let model = unsafe { device.modelID() }.to_string();
    let name = compose_device_name(&manufacturer, &model, &localized);
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
        name,
        position,
        transport,
    }
}

fn compose_device_name(manufacturer: &str, model: &str, localized: &str) -> String {
    let manufacturer = normalize_vendor_fragment(manufacturer);
    let model = normalize_model_fragment(model);
    let localized_lower = localized.to_lowercase();

    let mut parts: Vec<&str> = Vec::new();
    if !manufacturer.is_empty()
        && !fragment_already_present(&manufacturer, &localized_lower)
        && !fragment_already_present(&manufacturer, &model.to_lowercase())
    {
        parts.push(manufacturer.as_str());
    }
    if !model.is_empty() && !fragment_already_present(&model, &localized_lower) {
        parts.push(model.as_str());
    }
    parts.push(localized);

    let assembled = parts.join(" ");
    if assembled.trim().is_empty() {
        localized.to_string()
    } else {
        assembled
    }
}

fn normalize_vendor_fragment(value: &str) -> String {
    value.trim().trim_end_matches(['.', ',']).trim().to_string()
}

fn normalize_model_fragment(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.contains("VendorID") || trimmed.contains("ProductID") {
        return String::new();
    }
    trimmed.to_string()
}

fn fragment_already_present(fragment: &str, haystack_lower: &str) -> bool {
    let first_token = fragment.to_lowercase();
    let first_token = first_token.split_whitespace().next().unwrap_or("");
    !first_token.is_empty() && haystack_lower.contains(first_token)
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
