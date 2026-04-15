use crate::camera::Camera;
use crate::error::Error;
use crate::macos::delegate::FrameDelegate;
use crate::macos::enumerate::find_device;
use crate::macos::permission::ensure_authorized;
use crate::types::{DeviceId, Frame, StreamConfig};
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceFormat, AVCaptureDeviceInput, AVCaptureSession,
    AVCaptureVideoDataOutput,
};
use objc2_core_media::{CMTime, CMVideoFormatDescriptionGetDimensions};
use objc2_core_video::kCVPixelFormatType_32BGRA;
use objc2_foundation::{NSDictionary, NSNumber, NSString};

pub struct SessionHandle {
    pub(crate) session: Retained<AVCaptureSession>,
    #[allow(dead_code)]
    pub(crate) delegate: Retained<FrameDelegate>,
}

unsafe impl Send for SessionHandle {}
unsafe impl Sync for SessionHandle {}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        unsafe { self.session.stopRunning() };
    }
}

pub fn open(id: &DeviceId, config: StreamConfig) -> Result<Camera, Error> {
    ensure_authorized()?;

    let device = find_device(id)?;
    let format = select_format(&device, &config)?;

    let input =
        unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }.map_err(|error| {
            Error::Backend {
                platform: "macos",
                message: error.to_string(),
            }
        })?;

    let session = unsafe { AVCaptureSession::new() };
    unsafe { session.beginConfiguration() };

    if !unsafe { session.canAddInput(&input) } {
        return Err(Error::DeviceInUse);
    }
    unsafe { session.addInput(&input) };

    configure_device(&device, &format, config.framerate)?;

    let output = unsafe { AVCaptureVideoDataOutput::new() };
    let video_settings = build_video_settings();
    unsafe { output.setVideoSettings(Some(&video_settings)) };

    let (frame_tx, frame_rx) = crossbeam_channel::bounded::<Result<Frame, Error>>(3);
    let delegate = FrameDelegate::new(frame_tx);
    let queue = DispatchQueue::new("chimeras.frame_queue", None);

    unsafe {
        output.setSampleBufferDelegate_queue(Some(delegate.as_protocol()), Some(&queue));
    }

    if !unsafe { session.canAddOutput(&output) } {
        return Err(Error::FormatNotSupported);
    }
    unsafe { session.addOutput(&output) };

    unsafe { session.commitConfiguration() };
    unsafe { session.startRunning() };

    Ok(Camera {
        config,
        frame_rx,
        handle: SessionHandle { session, delegate },
    })
}

fn configure_device(
    device: &AVCaptureDevice,
    format: &AVCaptureDeviceFormat,
    framerate: u32,
) -> Result<(), Error> {
    unsafe {
        device
            .lockForConfiguration()
            .map_err(|error| Error::Backend {
                platform: "macos",
                message: error.to_string(),
            })?;
    }

    unsafe { device.setActiveFormat(format) };

    if let Some(duration) = pick_frame_duration(format, framerate) {
        let result = unsafe {
            objc2::exception::catch(std::panic::AssertUnwindSafe(|| unsafe {
                device.setActiveVideoMinFrameDuration(duration);
            }))
        };
        if let Err(exception) = result {
            unsafe { device.unlockForConfiguration() };
            let message = exception
                .as_ref()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "ObjC exception (nil)".into());
            return Err(Error::Backend {
                platform: "macos",
                message: format!("setActiveVideoMinFrameDuration rejected: {message}"),
            });
        }
    }

    unsafe { device.unlockForConfiguration() };
    Ok(())
}

fn pick_frame_duration(format: &AVCaptureDeviceFormat, framerate: u32) -> Option<CMTime> {
    let ranges = unsafe { format.videoSupportedFrameRateRanges() };
    let target = framerate as f64;
    let mut best_inside: Option<Retained<objc2_av_foundation::AVFrameRateRange>> = None;
    let mut best_distance: Option<(f64, Retained<objc2_av_foundation::AVFrameRateRange>)> = None;

    for index in 0..ranges.count() {
        let range = ranges.objectAtIndex(index);
        let min = unsafe { range.minFrameRate() };
        let max = unsafe { range.maxFrameRate() };
        if target >= min && target <= max {
            best_inside = Some(range);
            break;
        }
        let distance = (target - max).abs().min((target - min).abs());
        match &best_distance {
            None => best_distance = Some((distance, range)),
            Some((current, _)) if distance < *current => {
                best_distance = Some((distance, range));
            }
            _ => {}
        }
    }

    let range = best_inside.or(best_distance.map(|(_, range)| range))?;
    Some(unsafe { range.minFrameDuration() })
}

fn select_format(
    device: &AVCaptureDevice,
    config: &StreamConfig,
) -> Result<Retained<AVCaptureDeviceFormat>, Error> {
    let formats = unsafe { device.formats() };
    let mut exact: Option<Retained<AVCaptureDeviceFormat>> = None;
    let mut closest: Option<(i64, Retained<AVCaptureDeviceFormat>)> = None;

    for index in 0..formats.count() {
        let format = formats.objectAtIndex(index);
        let description = unsafe { format.formatDescription() };
        let dimensions = unsafe { CMVideoFormatDescriptionGetDimensions(&description) };
        let width = dimensions.width as u32;
        let height = dimensions.height as u32;

        let width_delta = (width as i64 - config.resolution.width as i64).abs();
        let height_delta = (height as i64 - config.resolution.height as i64).abs();
        let total_delta = width_delta + height_delta;

        if width == config.resolution.width && height == config.resolution.height {
            if supports_framerate(&format, config.framerate) {
                exact = Some(format.clone());
                break;
            }
            if exact.is_none() {
                exact = Some(format.clone());
            }
        }

        match &closest {
            None => closest = Some((total_delta, format.clone())),
            Some((best_delta, _)) if total_delta < *best_delta => {
                closest = Some((total_delta, format.clone()));
            }
            _ => {}
        }
    }

    exact
        .or(closest.map(|(_, format)| format))
        .ok_or(Error::FormatNotSupported)
}

fn supports_framerate(format: &AVCaptureDeviceFormat, framerate: u32) -> bool {
    let ranges = unsafe { format.videoSupportedFrameRateRanges() };
    let target = framerate as f64;
    for index in 0..ranges.count() {
        let range = ranges.objectAtIndex(index);
        let min = unsafe { range.minFrameRate() };
        let max = unsafe { range.maxFrameRate() };
        if target >= min && target <= max {
            return true;
        }
    }
    false
}

fn build_video_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    let key = NSString::from_str("PixelFormatType");
    let value = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
    let value_any: &AnyObject = &value;
    NSDictionary::from_slices(&[&*key], &[value_any])
}
