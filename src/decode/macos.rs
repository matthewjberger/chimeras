//! macOS VideoToolbox hardware H.264 / H.265 decoder.
//!
//! Builds a `CMFormatDescription` from the RTSP SDP extradata (AVCC for H.264,
//! HVCC for H.265), creates a `VTDecompressionSession`, and feeds NAL units
//! as `CMSampleBuffer`s. Decoded `CVPixelBuffer`s are pulled out of the async
//! output callback via an internal mutex-guarded queue and returned as NV12
//! [`Frame`]s (Y in `plane_primary`, UV in `plane_secondary`) matching the
//! Windows backend so the demo's GPU conversion path is identical on both.

use super::{VideoCodec, VideoDecoder};
use crate::error::Error;
use crate::types::{Frame, FrameQuality, PixelFormat};
use bytes::Bytes;
use objc2::rc::Retained;
use objc2_core_foundation::{CFDictionary, CFNumber, CFNumberType, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMTimeInvalid,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPlaneCount, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferPixelFormatTypeKey,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession,
};
use std::os::raw::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const NAL_LENGTH_SIZE: i32 = 4;

/// VideoToolbox decoder instance.
pub(crate) struct VideoToolboxDecoder {
    session: Retained<VTDecompressionSession>,
    format_desc: Retained<CMFormatDescription>,
    output: Arc<Mutex<OutputQueue>>,
}

unsafe impl Send for VideoToolboxDecoder {}

#[derive(Default)]
struct OutputQueue {
    frames: Vec<Frame>,
    error: Option<Error>,
}

fn build_destination_attributes() -> Option<CFRetained<CFDictionary>> {
    // NV12 biplanar video range: 'y420' -> kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
    let pixel_format: i32 = 0x34323076;
    let number = unsafe {
        CFNumber::new(
            None,
            CFNumberType::SInt32Type,
            &pixel_format as *const i32 as *const c_void,
        )
    }?;
    let number_ref: &CFType = (&*number).as_ref();
    let key_ref: &CFString = unsafe { kCVPixelBufferPixelFormatTypeKey };
    let typed: CFRetained<CFDictionary<CFString, CFType>> =
        CFDictionary::from_slices(&[key_ref], &[number_ref]);
    Some(unsafe { CFRetained::cast_unchecked(typed) })
}

fn catch_ns<R>(
    label: &'static str,
    closure: impl FnOnce() -> R + std::panic::UnwindSafe,
) -> Result<R, Error> {
    match objc2::exception::catch(closure) {
        Ok(value) => Ok(value),
        Err(None) => Err(Error::Backend {
            platform: "macos",
            message: format!("{label}: unknown NSException"),
        }),
        Err(Some(exception)) => Err(Error::Backend {
            platform: "macos",
            message: format!("{label}: {exception:?}"),
        }),
    }
}

impl VideoDecoder for VideoToolboxDecoder {
    fn new(codec: VideoCodec, extradata: &[u8]) -> Result<Self, Error> {
        let format_desc = build_format_description(codec, extradata)?;
        let output = Arc::new(Mutex::new(OutputQueue::default()));

        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: Arc::into_raw(Arc::clone(&output)) as *mut c_void,
        };

        let destination_attrs = build_destination_attributes();
        let destination_attrs_ref = destination_attrs.as_deref();

        let mut raw_session: *mut VTDecompressionSession = std::ptr::null_mut();
        let status = catch_ns(
            "VTDecompressionSession::create",
            std::panic::AssertUnwindSafe(|| unsafe {
                VTDecompressionSession::create(
                    None,
                    &format_desc,
                    None,
                    destination_attrs_ref,
                    &callback,
                    NonNull::from(&mut raw_session),
                )
            }),
        );
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                unsafe {
                    drop(Arc::from_raw(
                        callback.decompressionOutputRefCon as *const Mutex<OutputQueue>,
                    ));
                }
                return Err(error);
            }
        };
        if status != 0 || raw_session.is_null() {
            unsafe {
                drop(Arc::from_raw(
                    callback.decompressionOutputRefCon as *const Mutex<OutputQueue>,
                ));
            }
            return Err(Error::Backend {
                platform: "macos",
                message: format!("VTDecompressionSessionCreate failed: {status}"),
            });
        }

        let session = unsafe { Retained::from_raw(raw_session) }.ok_or(Error::Backend {
            platform: "macos",
            message: "VTDecompressionSession returned null".into(),
        })?;

        Ok(Self {
            session,
            format_desc,
            output,
        })
    }

    fn decode(&mut self, nal: &[u8], timestamp: Duration) -> Result<Vec<Frame>, Error> {
        let block = create_block_buffer(nal)?;
        let sample = create_sample_buffer(&block, &self.format_desc, timestamp)?;
        let mut info_flags = VTDecodeInfoFlags::empty();
        let status = catch_ns(
            "VTDecompressionSession::decode_frame",
            std::panic::AssertUnwindSafe(|| unsafe {
                self.session.decode_frame(
                    &sample,
                    VTDecodeFrameFlags::empty(),
                    std::ptr::null_mut(),
                    &mut info_flags,
                )
            }),
        )?;
        if status != 0 {
            return Err(Error::Backend {
                platform: "macos",
                message: format!("VTDecompressionSessionDecodeFrame failed: {status}"),
            });
        }

        let _ = catch_ns(
            "VTDecompressionSession::wait_for_asynchronous_frames",
            std::panic::AssertUnwindSafe(|| unsafe { self.session.wait_for_asynchronous_frames() }),
        );

        let mut guard = self.output.lock().map_err(|_| Error::Backend {
            platform: "macos",
            message: "decoder output mutex poisoned".into(),
        })?;
        if let Some(error) = guard.error.take() {
            return Err(error);
        }
        Ok(std::mem::take(&mut guard.frames))
    }
}

impl Drop for VideoToolboxDecoder {
    fn drop(&mut self) {
        unsafe { self.session.invalidate() };
    }
}

unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    _source_frame_refcon: *mut c_void,
    status: i32,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    _presentation_timestamp: CMTime,
    _presentation_duration: CMTime,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if refcon.is_null() {
            return;
        }
        let output = unsafe { &*(refcon as *const Mutex<OutputQueue>) };
        let Ok(mut guard) = output.lock() else { return };
        if status != 0 || image_buffer.is_null() {
            if guard.error.is_none() {
                guard.error = Some(Error::Backend {
                    platform: "macos",
                    message: format!("VideoToolbox decode callback status: {status}"),
                });
            }
            return;
        }
        let pixel_buffer = unsafe { &*(image_buffer as *const CVPixelBuffer) };
        if let Some(frame) = pixel_buffer_to_frame(pixel_buffer) {
            guard.frames.push(frame);
        }
    }));
    let _ = result;
}

fn pixel_buffer_to_frame(pb: &CVPixelBuffer) -> Option<Frame> {
    unsafe {
        CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
    }

    let width = CVPixelBufferGetWidth(pb) as u32;
    let height = CVPixelBufferGetHeight(pb) as u32;
    let plane_count = CVPixelBufferGetPlaneCount(pb);
    let result = if plane_count >= 2 {
        copy_biplanar_nv12(pb, width, height)
    } else {
        copy_single_plane_bgra(pb, width, height)
    };

    unsafe {
        CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
    }
    result
}

fn copy_biplanar_nv12(pb: &CVPixelBuffer, width: u32, height: u32) -> Option<Frame> {
    let y_stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
    let y_rows = CVPixelBufferGetHeightOfPlane(pb, 0);
    let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
    let uv_rows = CVPixelBufferGetHeightOfPlane(pb, 1);
    let y_base = CVPixelBufferGetBaseAddressOfPlane(pb, 0);
    let uv_base = CVPixelBufferGetBaseAddressOfPlane(pb, 1);
    if y_base.is_null() || uv_base.is_null() || y_stride == 0 || y_stride != uv_stride {
        return None;
    }
    let y_len = y_stride.saturating_mul(y_rows);
    let uv_len = uv_stride.saturating_mul(uv_rows);
    if y_len == 0 || uv_len == 0 {
        return None;
    }
    let y_data = unsafe { std::slice::from_raw_parts(y_base as *const u8, y_len) }.to_vec();
    let uv_data = unsafe { std::slice::from_raw_parts(uv_base as *const u8, uv_len) }.to_vec();
    Some(Frame {
        width,
        height,
        stride: y_stride as u32,
        timestamp: Duration::ZERO,
        pixel_format: PixelFormat::Nv12,
        quality: FrameQuality::Intact,
        plane_primary: Bytes::from(y_data),
        plane_secondary: Bytes::from(uv_data),
    })
}

fn copy_single_plane_bgra(pb: &CVPixelBuffer, width: u32, height: u32) -> Option<Frame> {
    let stride = CVPixelBufferGetBytesPerRow(pb);
    let base = CVPixelBufferGetBaseAddress(pb);
    if base.is_null() || stride == 0 || height == 0 {
        return None;
    }
    let byte_count = stride.saturating_mul(height as usize);
    if byte_count == 0 {
        return None;
    }
    let data = unsafe { std::slice::from_raw_parts(base as *const u8, byte_count) }.to_vec();
    Some(Frame {
        width,
        height,
        stride: stride as u32,
        timestamp: Duration::ZERO,
        pixel_format: PixelFormat::Bgra8,
        quality: FrameQuality::Intact,
        plane_primary: Bytes::from(data),
        plane_secondary: Bytes::new(),
    })
}

fn build_format_description(
    codec: VideoCodec,
    extradata: &[u8],
) -> Result<Retained<CMFormatDescription>, Error> {
    let (parameter_sets, sizes) = match codec {
        VideoCodec::H264 => parse_avcc(extradata)?,
        VideoCodec::H265 => parse_hvcc(extradata)?,
    };
    let pointers: Vec<NonNull<u8>> = parameter_sets
        .iter()
        .filter_map(|set| NonNull::new(set.as_ptr() as *mut u8))
        .collect();
    if pointers.len() != parameter_sets.len() {
        return Err(Error::Backend {
            platform: "macos",
            message: "parameter set had null pointer".into(),
        });
    }
    let pointers_nn =
        NonNull::new(pointers.as_ptr() as *mut NonNull<u8>).ok_or(Error::Backend {
            platform: "macos",
            message: "empty parameter set list".into(),
        })?;
    let sizes_nn = NonNull::new(sizes.as_ptr() as *mut usize).ok_or(Error::Backend {
        platform: "macos",
        message: "empty sizes list".into(),
    })?;

    let mut format_desc: *const CMFormatDescription = std::ptr::null();
    let status = catch_ns(
        "CMVideoFormatDescriptionCreate",
        std::panic::AssertUnwindSafe(|| unsafe {
            match codec {
                VideoCodec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    pointers.len(),
                    pointers_nn,
                    sizes_nn,
                    NAL_LENGTH_SIZE,
                    NonNull::from(&mut format_desc),
                ),
                VideoCodec::H265 => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    None,
                    pointers.len(),
                    pointers_nn,
                    sizes_nn,
                    NAL_LENGTH_SIZE,
                    None,
                    NonNull::from(&mut format_desc),
                ),
            }
        }),
    )?;
    if status != 0 || format_desc.is_null() {
        return Err(Error::Backend {
            platform: "macos",
            message: format!("CMVideoFormatDescriptionCreate failed: {status}"),
        });
    }
    unsafe { Retained::from_raw(format_desc as *mut CMFormatDescription) }.ok_or(Error::Backend {
        platform: "macos",
        message: "format description returned null".into(),
    })
}

fn parse_avcc(data: &[u8]) -> Result<(Vec<Vec<u8>>, Vec<usize>), Error> {
    if data.len() < 7 {
        return Err(Error::Backend {
            platform: "macos",
            message: "AVCC record too short".into(),
        });
    }
    let mut parameter_sets = Vec::new();
    let mut sizes = Vec::new();
    let num_sps = (data[5] & 0x1F) as usize;
    let mut offset = 6;
    for _ in 0..num_sps {
        if offset + 2 > data.len() {
            break;
        }
        let len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + len > data.len() {
            break;
        }
        parameter_sets.push(data[offset..offset + len].to_vec());
        sizes.push(len);
        offset += len;
    }
    if offset >= data.len() {
        return Err(Error::Backend {
            platform: "macos",
            message: "AVCC record missing PPS count".into(),
        });
    }
    let num_pps = data[offset] as usize;
    offset += 1;
    for _ in 0..num_pps {
        if offset + 2 > data.len() {
            break;
        }
        let len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + len > data.len() {
            break;
        }
        parameter_sets.push(data[offset..offset + len].to_vec());
        sizes.push(len);
        offset += len;
    }
    if parameter_sets.is_empty() {
        return Err(Error::Backend {
            platform: "macos",
            message: "AVCC record produced no parameter sets".into(),
        });
    }
    Ok((parameter_sets, sizes))
}

fn parse_hvcc(data: &[u8]) -> Result<(Vec<Vec<u8>>, Vec<usize>), Error> {
    if data.len() < 23 {
        return Err(Error::Backend {
            platform: "macos",
            message: "HVCC record too short".into(),
        });
    }
    let mut parameter_sets = Vec::new();
    let mut sizes = Vec::new();
    let num_arrays = data[22] as usize;
    let mut offset = 23;
    for _ in 0..num_arrays {
        if offset + 3 > data.len() {
            break;
        }
        let num_nalus = u16::from_be_bytes([data[offset + 1], data[offset + 2]]) as usize;
        offset += 3;
        for _ in 0..num_nalus {
            if offset + 2 > data.len() {
                break;
            }
            let len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
            offset += 2;
            if offset + len > data.len() {
                break;
            }
            parameter_sets.push(data[offset..offset + len].to_vec());
            sizes.push(len);
            offset += len;
        }
    }
    if parameter_sets.is_empty() {
        return Err(Error::Backend {
            platform: "macos",
            message: "HVCC record produced no parameter sets".into(),
        });
    }
    Ok((parameter_sets, sizes))
}

fn create_block_buffer(nal: &[u8]) -> Result<Retained<CMBlockBuffer>, Error> {
    let mut raw: *mut CMBlockBuffer = std::ptr::null_mut();
    let status = catch_ns(
        "CMBlockBuffer::create_with_memory_block",
        std::panic::AssertUnwindSafe(|| unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                std::ptr::null_mut(),
                nal.len(),
                None,
                std::ptr::null(),
                0,
                nal.len(),
                1,
                NonNull::from(&mut raw),
            )
        }),
    )?;
    if status != 0 || raw.is_null() {
        return Err(Error::Backend {
            platform: "macos",
            message: format!("CMBlockBuffer::create_with_memory_block failed: {status}"),
        });
    }
    let buffer = unsafe { Retained::from_raw(raw) }.ok_or(Error::Backend {
        platform: "macos",
        message: "block buffer null".into(),
    })?;
    let source = NonNull::new(nal.as_ptr() as *mut c_void).ok_or(Error::Backend {
        platform: "macos",
        message: "empty NAL buffer".into(),
    })?;
    let status = catch_ns(
        "CMBlockBuffer::replace_data_bytes",
        std::panic::AssertUnwindSafe(|| unsafe {
            CMBlockBuffer::replace_data_bytes(source, &buffer, 0, nal.len())
        }),
    )?;
    if status != 0 {
        return Err(Error::Backend {
            platform: "macos",
            message: format!("CMBlockBuffer::replace_data_bytes failed: {status}"),
        });
    }
    Ok(buffer)
}

fn create_sample_buffer(
    block: &CMBlockBuffer,
    format: &CMFormatDescription,
    timestamp: Duration,
) -> Result<Retained<CMSampleBuffer>, Error> {
    let mut raw: *mut CMSampleBuffer = std::ptr::null_mut();
    let invalid = unsafe { kCMTimeInvalid };
    let timing = CMSampleTimingInfo {
        duration: invalid,
        presentationTimeStamp: duration_to_cmtime(timestamp),
        decodeTimeStamp: invalid,
    };
    let status = catch_ns(
        "CMSampleBuffer::create",
        std::panic::AssertUnwindSafe(|| unsafe {
            CMSampleBuffer::create(
                None,
                Some(block),
                true,
                None,
                std::ptr::null_mut(),
                Some(format),
                1,
                1,
                &timing,
                0,
                std::ptr::null(),
                NonNull::from(&mut raw),
            )
        }),
    )?;
    if status != 0 || raw.is_null() {
        return Err(Error::Backend {
            platform: "macos",
            message: format!("CMSampleBufferCreate failed: {status}"),
        });
    }
    unsafe { Retained::from_raw(raw) }.ok_or(Error::Backend {
        platform: "macos",
        message: "sample buffer null".into(),
    })
}

fn duration_to_cmtime(duration: Duration) -> CMTime {
    CMTime {
        value: duration.as_micros() as i64,
        timescale: 1_000_000,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}
