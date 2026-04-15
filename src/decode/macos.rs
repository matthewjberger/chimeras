//! macOS VideoToolbox hardware H.264 / H.265 decoder.
//!
//! Builds a `CMFormatDescription` from the RTSP SDP extradata (AVCC for H.264,
//! HVCC for H.265), creates a `VTDecompressionSession` configured to output BGRA,
//! and feeds NAL units as `CMSampleBuffer`s. Decoded `CVPixelBuffer`s are pulled
//! out of the async output callback via an internal mutex-guarded queue and
//! returned as [`Frame`]s on the next call to `decode`.

use super::{VideoCodec, VideoDecoder};
use crate::error::Error;
use crate::types::{Frame, FrameQuality, PixelFormat};
use bytes::Bytes;
use objc2::rc::Retained;
use objc2_core_foundation::{
    CFDictionary, CFMutableDictionary, CFNumber, CFRetained, CFString, kCFAllocatorDefault,
};
use objc2_core_media::{
    CMBlockBuffer, CMBlockBufferCreateWithMemoryBlock, CMFormatDescription, CMSampleBuffer,
    CMSampleBufferCreate, CMTime, CMTimeFlags, CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_32BGRA,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTDecompressionSessionCreate, VTDecompressionSessionDecodeFrame,
    VTDecompressionSessionInvalidate,
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

impl VideoDecoder for VideoToolboxDecoder {
    fn new(codec: VideoCodec, extradata: &[u8]) -> Result<Self, Error> {
        let format_desc = build_format_description(codec, extradata)?;
        let output = Arc::new(Mutex::new(OutputQueue::default()));
        let attrs = build_destination_attributes();

        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: Arc::into_raw(Arc::clone(&output)) as *mut c_void,
        };

        let mut raw_session: *mut VTDecompressionSession = std::ptr::null_mut();
        let status = unsafe {
            VTDecompressionSessionCreate(
                None,
                &format_desc,
                None,
                attrs.as_deref(),
                &callback,
                NonNull::from(&mut raw_session),
            )
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
        let status = unsafe {
            VTDecompressionSessionDecodeFrame(
                &self.session,
                &sample,
                VTDecodeFrameFlags::empty(),
                std::ptr::null_mut(),
                NonNull::from(&mut info_flags),
            )
        };
        if status != 0 {
            return Err(Error::Backend {
                platform: "macos",
                message: format!("VTDecompressionSessionDecodeFrame failed: {status}"),
            });
        }

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
        unsafe { VTDecompressionSessionInvalidate(&self.session) };
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
}

fn pixel_buffer_to_frame(pb: &CVPixelBuffer) -> Option<Frame> {
    unsafe {
        CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
    }

    let width = CVPixelBufferGetWidth(pb) as u32;
    let height = CVPixelBufferGetHeight(pb) as u32;
    let stride = CVPixelBufferGetBytesPerRow(pb) as u32;
    let base = CVPixelBufferGetBaseAddress(pb);

    let mut data = Vec::new();
    if !base.is_null() {
        let byte_count = stride as usize * height as usize;
        data = unsafe { std::slice::from_raw_parts(base as *const u8, byte_count) }.to_vec();
    }

    unsafe {
        CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
    }

    Some(Frame {
        width,
        height,
        stride,
        timestamp: Duration::ZERO,
        pixel_format: PixelFormat::Bgra8,
        quality: FrameQuality::Intact,
        plane_primary: Bytes::from(data),
        plane_secondary: Bytes::new(),
    })
}

fn build_destination_attributes() -> Option<CFRetained<CFDictionary>> {
    let dict = CFMutableDictionary::new();
    let key: &CFString = unsafe { kCVPixelBufferPixelFormatTypeKey };
    let value = CFNumber::new_i32(kCVPixelFormatType_32BGRA as i32);
    unsafe {
        CFDictionary::set_value(
            &dict,
            key as *const CFString as *const c_void,
            &*value as *const CFNumber as *const c_void,
        );
    }
    Some(dict.into())
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
    let status = unsafe {
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
    };
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
    let status = unsafe {
        CMBlockBufferCreateWithMemoryBlock(
            None,
            nal.as_ptr() as *mut c_void,
            nal.len(),
            None,
            std::ptr::null(),
            0,
            nal.len(),
            0,
            NonNull::from(&mut raw),
        )
    };
    if status != 0 || raw.is_null() {
        return Err(Error::Backend {
            platform: "macos",
            message: format!("CMBlockBufferCreateWithMemoryBlock failed: {status}"),
        });
    }
    unsafe { Retained::from_raw(raw) }.ok_or(Error::Backend {
        platform: "macos",
        message: "block buffer null".into(),
    })
}

fn create_sample_buffer(
    block: &CMBlockBuffer,
    format: &CMFormatDescription,
    timestamp: Duration,
) -> Result<Retained<CMSampleBuffer>, Error> {
    let mut raw: *mut CMSampleBuffer = std::ptr::null_mut();
    let pts = duration_to_cmtime(timestamp);
    let status = unsafe {
        CMSampleBufferCreate(
            None,
            Some(block),
            true,
            None,
            std::ptr::null_mut(),
            Some(format),
            1,
            1,
            &pts,
            0,
            std::ptr::null(),
            NonNull::from(&mut raw),
        )
    };
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

#[allow(dead_code)]
fn _allocator_default() -> Option<&'static objc2_core_foundation::CFAllocator> {
    unsafe { kCFAllocatorDefault }
}
